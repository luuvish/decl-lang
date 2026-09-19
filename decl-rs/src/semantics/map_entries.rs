//! Ordered map entries with optional shared, immutable key shapes.
//!
//! Each map owns its values. Sharing a shape never shares mutable values or
//! introduces an additional owner of a runtime value. Structural mutations
//! promote a shared map to an ordinary ordered map; replacing an existing value
//! does not copy keys or affect another map.

use super::{OrderedMap, Value};
use rustc_hash::{FxBuildHasher, FxHashMap, FxHasher};
use std::hash::{Hash, Hasher};
use std::iter::FusedIterator;
use std::rc::{Rc, Weak};

const MAX_SHAPE_KEYS: usize = 8;
const MAX_POOL_ENTRIES: usize = 4096;

struct KeyShape {
    keys: OrderedMap<()>,
}

#[derive(Clone)]
enum Storage {
    // Keep empty construction and temporary replacement allocation-free. In
    // particular, clearing Shared must detach its values before their drops.
    Empty,
    // The ordinary table header must not determine every Shared map's size.
    Owned(Box<OrderedMap<Value>>),
    // Only engine construction starts here. Keep values on the heap and move
    // them into their final storage without constructing a temporary key table.
    Small(Vec<(String, Value)>),
    Shared {
        shape: Rc<KeyShape>,
        values: Vec<Value>,
    },
}

/// Insertion-ordered map entries with independent values and optional shared
/// keys. Duplicate insertion replaces the value at the original key position.
///
/// Cloning clones each value exactly once. When keys are shared, cloning retains
/// their immutable shape instead of cloning the key strings. Use
/// [`Self::as_ordered_mut`] for operations specific to an ordinary ordered map.
#[derive(Clone)]
pub struct MapEntries {
    storage: Storage,
}

impl Default for MapEntries {
    fn default() -> Self {
        Self::new()
    }
}

impl MapEntries {
    /// Construct an empty, unshared map without allocating entry storage.
    pub fn new() -> Self {
        Self {
            storage: Storage::Empty,
        }
    }

    /// Start an engine-produced map without allocating an ordinary key table.
    /// The ninth distinct key promotes to ordinary storage; duplicate keys
    /// replace their value at the original position before either transition.
    pub(crate) fn building() -> Self {
        Self {
            storage: Storage::Small(Vec::new()),
        }
    }

    /// Construct an unshared map with room for at least `capacity` entries.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_hasher(capacity, FxBuildHasher)
    }

    /// Construct an unshared map with the ordinary ordered map's hasher.
    pub fn with_capacity_and_hasher(capacity: usize, hasher: FxBuildHasher) -> Self {
        Self::from(OrderedMap::with_capacity_and_hasher(capacity, hasher))
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        match &self.storage {
            Storage::Empty => 0,
            Storage::Owned(entries) => entries.len(),
            Storage::Small(entries) => entries.len(),
            Storage::Shared { values, .. } => values.len(),
        }
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The capacity of this map's value storage. For shared maps this is the
    /// value vector's capacity, not the capacity of the shared key table.
    pub fn capacity(&self) -> usize {
        match &self.storage {
            Storage::Empty => 0,
            Storage::Owned(entries) => entries.capacity(),
            Storage::Small(entries) => entries.capacity(),
            Storage::Shared { values, .. } => values.capacity(),
        }
    }

    /// Whether this map currently uses an immutable shared key shape.
    pub fn is_shared(&self) -> bool {
        matches!(self.storage, Storage::Shared { .. })
    }

    /// Borrow the value at `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match &self.storage {
            Storage::Empty => None,
            Storage::Owned(entries) => entries.get(key),
            Storage::Small(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            Storage::Shared { shape, values } => {
                shape.keys.get_index_of(key).map(|index| &values[index])
            }
        }
    }

    /// Whether the key is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.get_index_of(key).is_some()
    }

    /// The insertion-order index of a key.
    pub fn get_index_of(&self, key: &str) -> Option<usize> {
        match &self.storage {
            Storage::Empty => None,
            Storage::Owned(entries) => entries.get_index_of(key),
            Storage::Small(entries) => entries.iter().position(|(k, _)| k == key),
            Storage::Shared { shape, .. } => shape.keys.get_index_of(key),
        }
    }

    /// Borrow an entry by insertion-order index.
    pub fn get_index(&self, index: usize) -> Option<(&String, &Value)> {
        match &self.storage {
            Storage::Empty => None,
            Storage::Owned(entries) => entries.get_index(index),
            Storage::Small(entries) => entries.get(index).map(|(k, v)| (k, v)),
            Storage::Shared { shape, values } => shape
                .keys
                .get_index(index)
                .map(|(key, ())| (key, &values[index])),
        }
    }

    /// Mutably borrow one value without changing or copying its key shape.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match &mut self.storage {
            Storage::Empty => None,
            Storage::Owned(entries) => entries.get_mut(key),
            Storage::Small(entries) => entries
                .iter_mut()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, v)| v),
            Storage::Shared { shape, values } => {
                let index = shape.keys.get_index_of(key)?;
                Some(&mut values[index])
            }
        }
    }

    /// Borrow a key and mutably borrow its value by insertion-order index.
    pub fn get_index_mut(&mut self, index: usize) -> Option<(&String, &mut Value)> {
        match &mut self.storage {
            Storage::Empty => None,
            Storage::Owned(entries) => entries.get_index_mut(index),
            Storage::Small(entries) => entries.get_mut(index).map(|(k, v)| (&*k, v)),
            Storage::Shared { shape, values } => {
                let (key, ()) = shape.keys.get_index(index)?;
                Some((key, &mut values[index]))
            }
        }
    }

    /// Borrow entries in insertion order without cloning keys or values.
    pub fn iter(&self) -> MapEntriesIter<'_> {
        MapEntriesIter {
            inner: match &self.storage {
                Storage::Empty => IterStorage::Empty,
                Storage::Owned(entries) => IterStorage::Owned(entries.iter()),
                Storage::Small(entries) => IterStorage::Small(entries.iter()),
                Storage::Shared { shape, values } => {
                    IterStorage::Shared(shape.keys.keys().zip(values.iter()))
                }
            },
        }
    }

    /// Borrow keys in insertion order.
    pub fn keys(&self) -> MapEntriesKeys<'_> {
        MapEntriesKeys { inner: self.iter() }
    }

    /// Borrow values in insertion order.
    pub fn values(&self) -> MapEntriesValues<'_> {
        MapEntriesValues { inner: self.iter() }
    }

    /// Borrow keys and mutably borrow values in insertion order. Keys remain
    /// immutable and each mutable value belongs to this map alone.
    pub fn iter_mut(&mut self) -> MapEntriesIterMut<'_> {
        MapEntriesIterMut {
            inner: match &mut self.storage {
                Storage::Empty => IterMutStorage::Empty,
                Storage::Owned(entries) => IterMutStorage::Owned(entries.iter_mut()),
                Storage::Small(entries) => IterMutStorage::Small(entries.iter_mut()),
                Storage::Shared { shape, values } => {
                    IterMutStorage::Shared(shape.keys.keys().zip(values.iter_mut()))
                }
            },
        }
    }

    /// Mutably borrow values in insertion order without copying keys.
    pub fn values_mut(&mut self) -> MapEntriesValuesMut<'_> {
        MapEntriesValuesMut {
            inner: self.iter_mut(),
        }
    }

    /// Insert or replace an entry. Replacing an existing key keeps its original
    /// position and returns its previous value without promoting shared keys.
    /// Adding a key promotes shared storage before insertion.
    pub fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        match &mut self.storage {
            Storage::Empty => {}
            Storage::Owned(entries) => return entries.insert(key, value),
            Storage::Small(entries) => {
                if let Some((_, old)) = entries.iter_mut().find(|(k, _)| k.as_str() == key.as_str())
                {
                    return Some(std::mem::replace(old, value));
                }
                if entries.len() < MAX_SHAPE_KEYS {
                    entries.push((key, value));
                    return None;
                }
            }
            Storage::Shared { shape, values } => {
                if let Some(index) = shape.keys.get_index_of(key.as_str()) {
                    return Some(std::mem::replace(&mut values[index], value));
                }
            }
        }
        self.as_ordered_mut().insert(key, value)
    }

    /// Remove a key, shifting later entries left while preserving their order.
    /// An absent key leaves the representation unchanged.
    pub fn shift_remove(&mut self, key: &str) -> Option<Value> {
        if !self.contains_key(key) {
            return None;
        }
        self.as_ordered_mut().shift_remove(key)
    }

    /// Remove a key, moving the last entry into its position. An absent key
    /// leaves the representation unchanged.
    pub fn swap_remove(&mut self, key: &str) -> Option<Value> {
        if !self.contains_key(key) {
            return None;
        }
        self.as_ordered_mut().swap_remove(key)
    }

    /// Remove all values in insertion order. Shared storage becomes an empty
    /// ordinary map without first allocating or cloning a key table.
    pub fn clear(&mut self) {
        match &mut self.storage {
            Storage::Empty => {}
            Storage::Owned(entries) => entries.clear(),
            Storage::Small(entries) => entries.clear(),
            Storage::Shared { .. } => {
                let old = std::mem::replace(&mut self.storage, Storage::Empty);
                drop(old);
            }
        }
    }

    /// Obtain ordinary mutable map storage. Shared keys are cloned and values
    /// are moved exactly once; other maps retain their keys and values.
    /// Subsequent ordered-map operations follow the ordinary map API.
    pub fn as_ordered_mut(&mut self) -> &mut OrderedMap<Value> {
        if !matches!(self.storage, Storage::Owned(_)) {
            let old = std::mem::replace(&mut self.storage, Storage::Empty);
            self.storage = Storage::Owned(Box::new(Self { storage: old }.into_ordered()));
        }
        match &mut self.storage {
            Storage::Owned(entries) => entries,
            Storage::Empty | Storage::Small(_) | Storage::Shared { .. } => {
                unreachable!("entries were promoted")
            }
        }
    }

    /// Consume the entries as an ordinary ordered map. Shared keys are cloned;
    /// values are moved, preserving their order and identity.
    pub fn into_ordered(self) -> OrderedMap<Value> {
        match self.storage {
            Storage::Empty => OrderedMap::default(),
            Storage::Owned(entries) => *entries,
            Storage::Small(entries) => entries.into_iter().collect(),
            Storage::Shared { shape, values } => {
                let mut entries = OrderedMap::with_capacity_and_hasher(values.len(), FxBuildHasher);
                for (key, value) in shape.keys.keys().zip(values) {
                    entries.insert(key.clone(), value);
                }
                entries
            }
        }
    }

    /// Reconstruct entries with new, independently owned values in the same
    /// order. Shared storage retains exactly the same immutable key shape.
    ///
    /// The caller must register any recursive destination before producing its
    /// values. This method does not perform recursion or resolve cycles.
    ///
    /// # Panics
    ///
    /// Panics if `values` does not contain exactly one value per source key.
    pub(crate) fn with_values(&self, values: Vec<Value>) -> Self {
        assert_eq!(
            values.len(),
            self.len(),
            "map reconstruction changes key count"
        );
        match &self.storage {
            Storage::Empty => Self::new(),
            Storage::Owned(entries) => entries.keys().cloned().zip(values).collect(),
            Storage::Small(entries) => Self {
                storage: Storage::Small(
                    entries.iter().map(|(k, _)| k.clone()).zip(values).collect(),
                ),
            },
            Storage::Shared { shape, .. } => Self {
                storage: Storage::Shared {
                    shape: shape.clone(),
                    values,
                },
            },
        }
    }
}

impl From<OrderedMap<Value>> for MapEntries {
    fn from(entries: OrderedMap<Value>) -> Self {
        if entries.capacity() == 0 {
            Self::new()
        } else {
            Self {
                storage: Storage::Owned(Box::new(entries)),
            }
        }
    }
}

impl From<MapEntries> for OrderedMap<Value> {
    fn from(entries: MapEntries) -> Self {
        entries.into_ordered()
    }
}

impl FromIterator<(String, Value)> for MapEntries {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        Self::from(OrderedMap::from_iter(iter))
    }
}

impl Extend<(String, Value)> for MapEntries {
    fn extend<T: IntoIterator<Item = (String, Value)>>(&mut self, iter: T) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

enum IterStorage<'a> {
    Empty,
    Owned(indexmap::map::Iter<'a, String, Value>),
    Small(std::slice::Iter<'a, (String, Value)>),
    Shared(std::iter::Zip<indexmap::map::Keys<'a, String, ()>, std::slice::Iter<'a, Value>>),
}

/// An iterator over borrowed map entries in insertion order.
pub struct MapEntriesIter<'a> {
    inner: IterStorage<'a>,
}

impl<'a> Iterator for MapEntriesIter<'a> {
    type Item = (&'a String, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterStorage::Empty => None,
            IterStorage::Owned(iter) => iter.next(),
            IterStorage::Small(iter) => iter.next().map(|(k, v)| (k, v)),
            IterStorage::Shared(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl DoubleEndedIterator for MapEntriesIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterStorage::Empty => None,
            IterStorage::Owned(iter) => iter.next_back(),
            IterStorage::Small(iter) => iter.next_back().map(|(k, v)| (k, v)),
            IterStorage::Shared(iter) => iter.next_back(),
        }
    }
}

impl ExactSizeIterator for MapEntriesIter<'_> {
    fn len(&self) -> usize {
        match &self.inner {
            IterStorage::Empty => 0,
            IterStorage::Owned(iter) => iter.len(),
            IterStorage::Small(iter) => iter.len(),
            IterStorage::Shared(iter) => iter.len(),
        }
    }
}

impl FusedIterator for MapEntriesIter<'_> {}

/// An iterator over borrowed map keys in insertion order.
pub struct MapEntriesKeys<'a> {
    inner: MapEntriesIter<'a>,
}

impl<'a> Iterator for MapEntriesKeys<'a> {
    type Item = &'a String;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, _)| key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl DoubleEndedIterator for MapEntriesKeys<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(key, _)| key)
    }
}

impl ExactSizeIterator for MapEntriesKeys<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl FusedIterator for MapEntriesKeys<'_> {}

/// An iterator over borrowed map values in insertion order.
pub struct MapEntriesValues<'a> {
    inner: MapEntriesIter<'a>,
}

impl<'a> Iterator for MapEntriesValues<'a> {
    type Item = &'a Value;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl DoubleEndedIterator for MapEntriesValues<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, value)| value)
    }
}

impl ExactSizeIterator for MapEntriesValues<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl FusedIterator for MapEntriesValues<'_> {}

enum IterMutStorage<'a> {
    Empty,
    Owned(indexmap::map::IterMut<'a, String, Value>),
    Small(std::slice::IterMut<'a, (String, Value)>),
    Shared(std::iter::Zip<indexmap::map::Keys<'a, String, ()>, std::slice::IterMut<'a, Value>>),
}

/// An iterator over borrowed keys and independently mutable map values.
pub struct MapEntriesIterMut<'a> {
    inner: IterMutStorage<'a>,
}

impl<'a> Iterator for MapEntriesIterMut<'a> {
    type Item = (&'a String, &'a mut Value);

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterMutStorage::Empty => None,
            IterMutStorage::Owned(iter) => iter.next(),
            IterMutStorage::Small(iter) => iter.next().map(|(k, v)| (&*k, v)),
            IterMutStorage::Shared(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl DoubleEndedIterator for MapEntriesIterMut<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterMutStorage::Empty => None,
            IterMutStorage::Owned(iter) => iter.next_back(),
            IterMutStorage::Small(iter) => iter.next_back().map(|(k, v)| (&*k, v)),
            IterMutStorage::Shared(iter) => iter.next_back(),
        }
    }
}

impl ExactSizeIterator for MapEntriesIterMut<'_> {
    fn len(&self) -> usize {
        match &self.inner {
            IterMutStorage::Empty => 0,
            IterMutStorage::Owned(iter) => iter.len(),
            IterMutStorage::Small(iter) => iter.len(),
            IterMutStorage::Shared(iter) => iter.len(),
        }
    }
}

impl FusedIterator for MapEntriesIterMut<'_> {}

/// An iterator over independently mutable map values in insertion order.
pub struct MapEntriesValuesMut<'a> {
    inner: MapEntriesIterMut<'a>,
}

impl<'a> Iterator for MapEntriesValuesMut<'a> {
    type Item = &'a mut Value;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl DoubleEndedIterator for MapEntriesValuesMut<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(_, value)| value)
    }
}

impl ExactSizeIterator for MapEntriesValuesMut<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl FusedIterator for MapEntriesValuesMut<'_> {}

impl<'a> IntoIterator for &'a MapEntries {
    type Item = (&'a String, &'a Value);
    type IntoIter = MapEntriesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut MapEntries {
    type Item = (&'a String, &'a mut Value);
    type IntoIter = MapEntriesIterMut<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

/// An owning iterator over map entries. Shared keys become owned strings when
/// this iterator is constructed; values are moved without cloning.
pub struct MapEntriesIntoIter {
    inner: IntoIterStorage,
}

enum IntoIterStorage {
    Empty,
    Owned(indexmap::map::IntoIter<String, Value>),
    Small(std::vec::IntoIter<(String, Value)>),
}

impl Iterator for MapEntriesIntoIter {
    type Item = (String, Value);

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IntoIterStorage::Empty => None,
            IntoIterStorage::Owned(iter) => iter.next(),
            IntoIterStorage::Small(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl DoubleEndedIterator for MapEntriesIntoIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IntoIterStorage::Empty => None,
            IntoIterStorage::Owned(iter) => iter.next_back(),
            IntoIterStorage::Small(iter) => iter.next_back(),
        }
    }
}

impl ExactSizeIterator for MapEntriesIntoIter {
    fn len(&self) -> usize {
        match &self.inner {
            IntoIterStorage::Empty => 0,
            IntoIterStorage::Owned(iter) => iter.len(),
            IntoIterStorage::Small(iter) => iter.len(),
        }
    }
}

impl FusedIterator for MapEntriesIntoIter {}

impl IntoIterator for MapEntries {
    type Item = (String, Value);
    type IntoIter = MapEntriesIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        MapEntriesIntoIter {
            inner: match self.storage {
                Storage::Empty => IntoIterStorage::Empty,
                Storage::Small(entries) => IntoIterStorage::Small(entries.into_iter()),
                other => IntoIterStorage::Owned(Self { storage: other }.into_ordered().into_iter()),
            },
        }
    }
}

/// A bounded interner holding only weak references to immutable key shapes.
/// Fingerprints are a lookup hint; complete ordered key equality is always
/// checked before sharing. Neither the pool nor a shape owns runtime values.
#[derive(Default)]
pub(crate) struct MapShapePool {
    entries: FxHashMap<u64, Weak<KeyShape>>,
}

impl MapShapePool {
    /// Share the keys of a completed map containing one through eight entries.
    /// Empty and wider maps remain ordinary maps. Returns whether the resulting
    /// map is shared, including maps that were already shared on entry.
    pub(crate) fn seal(&mut self, entries: &mut MapEntries) -> bool {
        if entries.is_shared() {
            return true;
        }
        if entries.is_empty() {
            // A successful empty construction publishes the same ordinary
            // empty representation as before, without allocating either table.
            if matches!(entries.storage, Storage::Small(_)) {
                entries.storage = Storage::Empty;
            }
            return false;
        }
        if !(1..=MAX_SHAPE_KEYS).contains(&entries.len()) {
            return false;
        }
        let mut hash = FxHasher::default();
        entries.len().hash(&mut hash);
        for key in entries.keys() {
            key.len().hash(&mut hash);
            key.hash(&mut hash);
        }
        self.seal_with_fingerprint(entries, hash.finish())
    }

    // Kept separate so native tests can inject a collision without depending
    // on the hasher or attempting to discover an actual fingerprint collision.
    fn seal_with_fingerprint(&mut self, entries: &mut MapEntries, fingerprint: u64) -> bool {
        if entries.is_shared() {
            return true;
        }
        if !(1..=MAX_SHAPE_KEYS).contains(&entries.len()) {
            return false;
        }
        let existing = self
            .entries
            .get(&fingerprint)
            .and_then(Weak::upgrade)
            .filter(|shape| shape.keys.keys().eq(entries.keys()));
        let old = std::mem::replace(&mut entries.storage, Storage::Empty);
        let owned = MapEntries { storage: old }.into_iter();
        let mut values = Vec::with_capacity(owned.len());
        let shape = if let Some(shape) = existing {
            for (_, value) in owned {
                values.push(value);
            }
            shape
        } else {
            let mut keys = OrderedMap::with_capacity_and_hasher(owned.len(), FxBuildHasher);
            for (key, value) in owned {
                keys.insert(key, ());
                values.push(value);
            }
            let shape = Rc::new(KeyShape { keys });
            if self.entries.len() >= MAX_POOL_ENTRIES && !self.entries.contains_key(&fingerprint) {
                // Clearing only weak references cannot destroy any live shape
                // or value. It bounds stale fingerprints without a full scan
                // on every insertion or a separately allocated collision list.
                self.entries.clear();
            }
            self.entries.insert(fingerprint, Rc::downgrade(&shape));
            shape
        };
        entries.storage = Storage::Shared { shape, values };
        true
    }
}

#[cfg(test)]
#[path = "../../tests/private/map_entries_test.rs"]
mod tests;
