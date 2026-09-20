//! Canonical query identities and shared dependency snapshots.
//!
//! A pool keeps one canonical identity for each key's text. The
//! graph, revision snapshots and computing stack own the identities they use;
//! keys contain no references back to any graph or value. Quiescent sweeping
//! removes identities whose only remaining owner is the pool.
use hashbrown::{hash_table::Entry, HashTable};
use rustc_hash::{FxHashSet, FxHasher};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::iter::FusedIterator;
use std::mem::size_of;
use std::ops::Deref;
use std::rc::{Rc, Weak};

struct QueryKey {
    text: Rc<str>,
    hash: u64,
}

/// A thin shared handle. Text equality also permits safe cross-pool use.
#[derive(Clone)]
pub(crate) struct QueryId(Rc<QueryKey>);

impl QueryId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0.text
    }
    /// The hash of the text, computed once when the key was interned: what
    /// [`QueryPool::text_hash`] gives for the same spelling, in any pool.
    pub(crate) fn text_hash(&self) -> u64 {
        self.0.hash
    }
}

impl Deref for QueryId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for QueryId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for QueryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl fmt::Debug for QueryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq for QueryId {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
            || self.0.hash == other.0.hash && self.as_str() == other.as_str()
    }
}

impl Eq for QueryId {}

impl Hash for QueryId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Deliberately no Borrow<str>: this cached-hash encoding is not the
        // encoding used by str::hash, even when the final hasher is FxHasher.
        state.write_u64(self.0.hash);
    }
}

impl Ord for QueryId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for QueryId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Pool storage counters and target-specific payload layouts, not RSS.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QueryPoolStats {
    pub(crate) entries: usize,
    pub(crate) live: usize,
    pub(crate) dead: usize,
    pub(crate) text_bytes: usize,
    pub(crate) live_text_bytes: usize,
    pub(crate) dead_text_bytes: usize,
    /// HashTable's usable element capacity, not its physical bucket count.
    pub(crate) capacity: usize,
    pub(crate) query_id_bytes: usize,
    pub(crate) query_key_bytes: usize,
    pub(crate) text_handle_bytes: usize,
    pub(crate) weak_key_bytes: usize,
    pub(crate) interner_entry_bytes: usize,
    pub(crate) read_set_bytes: usize,
    pub(crate) dependency_set_bytes: usize,
    pub(crate) rc_header_bytes: usize,
    /// Raw weak entries, including pool-only keys and other retained Engines.
    pub(crate) scratch_index_entries: usize,
    pub(crate) scratch_index_capacity: usize,
    pub(crate) scratch_index_entry_bytes: usize,
}

pub(crate) struct QueryPool {
    by_text: RefCell<HashTable<QueryId>>,
    next_sweep: Cell<usize>,
    creations_since_sweep: Cell<usize>,
    swept_during_intern: Cell<bool>,
    // Only the spellings Session's underscore cleanup may inspect. Weak
    // handles neither keep graph identities live nor retain their text.
    scratch: RefCell<Vec<Weak<QueryKey>>>,
}

impl Default for QueryPool {
    fn default() -> Self {
        Self {
            by_text: RefCell::new(HashTable::new()),
            next_sweep: Cell::new(1024),
            creations_since_sweep: Cell::new(0),
            swept_during_intern: Cell::new(false),
            scratch: RefCell::new(Vec::new()),
        }
    }
}

impl QueryPool {
    pub(crate) fn intern(&self, text: &str) -> QueryId {
        let mut by_text = self.by_text.borrow_mut();
        let hash = Self::text_hash(text);
        let matches = |key: &QueryId| key.0.hash == hash && key.as_str() == text;
        // Live hits never trigger maintenance. At the threshold, inspect before
        // cloning: the table owns exactly one strong handle per spelling.
        let id = if by_text.len() >= self.next_sweep.get() {
            if let Some(key) = by_text.find(hash, matches) {
                if Rc::strong_count(&key.0) > 1 {
                    return key.clone();
                }
            }
            // Remove a pool-only identity before the scan. Keeping a clone in
            // the table would falsely count its revival as an existing live key
            // and change the next sweep threshold. Reuse its allocation after.
            let recycled = by_text
                .find_entry(hash, matches)
                .ok()
                .map(|entry| entry.remove().0);
            self.sweep_entries(&mut by_text);
            self.swept_during_intern.set(true);
            let id = recycled.unwrap_or_else(|| self.new_id(text, hash));
            by_text.insert_unique(hash, id.clone(), |key| key.0.hash);
            id
        } else {
            // One full-text hash and one normal table probe. Rehashing a growing
            // table uses each identity's cached hash rather than reading text.
            match by_text.entry(hash, matches, |key| key.0.hash) {
                Entry::Occupied(entry) => {
                    let key = entry.get();
                    if Rc::strong_count(&key.0) > 1 {
                        return key.clone();
                    }
                    key.clone() // pool-only revival: no new allocation
                }
                Entry::Vacant(entry) => {
                    let id = self.new_id(text, hash);
                    entry.insert(id.clone());
                    id
                }
            }
        };
        // Revival counts as a birth even when its allocation is reused.
        self.creations_since_sweep
            .set(self.creations_since_sweep.get().saturating_add(1));
        id
    }

    /// The hash every key caches for its text; a table keyed by it can be
    /// asked for a spelling without holding a key.
    pub(crate) fn text_hash(text: &str) -> u64 {
        let mut hasher = FxHasher::default();
        text.hash(&mut hasher);
        hasher.finish()
    }

    fn new_id(&self, text: &str, hash: u64) -> QueryId {
        let id = QueryId(Rc::new(QueryKey {
            text: Rc::from(text),
            hash,
        }));
        // A revived pool-only identity already has its weak entry. Index only
        // fresh allocations, and strip at most one assertion-owner prefix.
        let path = text.strip_prefix("assert:").unwrap_or(text);
        if path.strip_prefix('_').is_some_and(|suffix| {
            suffix.is_empty() || suffix.starts_with('.') || suffix.starts_with('[')
        }) {
            self.scratch.borrow_mut().push(Rc::downgrade(&id.0));
        }
        id
    }

    fn prune_scratch(&self) {
        let mut scratch = self.scratch.borrow_mut();
        scratch.retain(|key| key.strong_count() > 0);
        if scratch.capacity() > scratch.len().saturating_mul(2).max(64) {
            scratch.shrink_to_fit();
        }
    }

    /// Candidates only: a shared lineage can contain old Engines' identities
    /// and pool-only keys. Callers must inspect their current graph membership.
    /// Drop returned handles before quiescent maintenance.
    pub(crate) fn scratch_ids(&self) -> Vec<QueryId> {
        self.prune_scratch();
        self.scratch
            .borrow()
            .iter()
            .filter_map(|key| key.upgrade().map(QueryId))
            .collect()
    }

    /// Look up an externally owned identity without reviving a pool-only key.
    pub(crate) fn lookup(&self, text: &str) -> Option<QueryId> {
        let hash = Self::text_hash(text);
        self.by_text
            .borrow()
            .find(hash, |key| key.0.hash == hash && key.as_str() == text)
            .filter(|key| Rc::strong_count(&key.0) > 1)
            .cloned()
    }

    /// Run only after request-owned snapshots and temporary identities drop.
    pub(crate) fn sweep(&self) {
        let mut by_text = self.by_text.borrow_mut();
        self.sweep_entries(&mut by_text);
        self.swept_during_intern.set(false);
    }

    /// Coalesce small scratch requests instead of scanning their live universe
    /// after each expression. Count recreated keys as well as new spellings.
    pub(crate) fn maintain_transient(&self) {
        if self.swept_during_intern.get() || self.creations_since_sweep.get() >= 1024 {
            self.sweep();
        }
    }

    fn sweep_entries(&self, by_text: &mut HashTable<QueryId>) {
        by_text.retain(|key| Rc::strong_count(&key.0) > 1);
        // Avoid rehashing for small occupancy fluctuations, while bounding
        // retained capacity after a much larger former query universe.
        if by_text.capacity() > by_text.len().saturating_mul(2).max(64) {
            by_text.shrink_to_fit(|key| key.0.hash);
        }
        self.next_sweep
            .set(by_text.len().saturating_mul(2).max(1024));
        self.creations_since_sweep.set(0);
        self.prune_scratch();
    }

    pub(crate) fn census(&self) -> QueryPoolStats {
        let by_text = self.by_text.borrow();
        let scratch = self.scratch.borrow();
        let mut stats = QueryPoolStats {
            entries: by_text.len(),
            capacity: by_text.capacity(),
            scratch_index_entries: scratch.len(),
            scratch_index_capacity: scratch.capacity(),
            scratch_index_entry_bytes: size_of::<Weak<QueryKey>>(),
            query_id_bytes: size_of::<QueryId>(),
            query_key_bytes: size_of::<QueryKey>(),
            text_handle_bytes: size_of::<Rc<str>>(),
            weak_key_bytes: 0,
            interner_entry_bytes: size_of::<QueryId>(),
            read_set_bytes: size_of::<ReadSet>(),
            dependency_set_bytes: size_of::<FxHashSet<QueryId>>(),
            rc_header_bytes: 2 * size_of::<usize>(),
            ..QueryPoolStats::default()
        };
        for key in by_text.iter() {
            let text = key.as_str();
            stats.text_bytes += text.len();
            if Rc::strong_count(&key.0) > 1 {
                stats.live += 1;
                stats.live_text_bytes += text.len();
            } else {
                stats.dead += 1;
                stats.dead_text_bytes += text.len();
            }
        }
        stats
    }
}

/// Empty snapshots allocate nothing. Nonempty snapshots share their set until
/// a writer changes it; starting a new computation replaces its old handle.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReadSet(Option<Rc<FxHashSet<QueryId>>>);

/// A borrowed census of one allocation; inspecting it creates no Rc owner.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReadSetStorage {
    pub(crate) identity: usize,
    pub(crate) len: usize,
    /// HashSet's usable element capacity, not its physical bucket count.
    pub(crate) capacity: usize,
    pub(crate) strong_count: usize,
}

impl ReadSet {
    pub(crate) fn iter(&self) -> ReadSetIter<'_> {
        ReadSetIter(self.0.as_ref().map(|deps| deps.iter()))
    }

    pub(crate) fn insert(&mut self, key: QueryId) -> bool {
        let deps = self.0.get_or_insert_with(|| Rc::new(FxHashSet::default()));
        Rc::make_mut(deps).insert(key)
    }

    pub(crate) fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |deps| deps.len())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn storage(&self) -> Option<ReadSetStorage> {
        self.0.as_ref().map(|deps| ReadSetStorage {
            identity: Rc::as_ptr(deps) as usize,
            len: deps.len(),
            capacity: deps.capacity(),
            strong_count: Rc::strong_count(deps),
        })
    }
}

impl FromIterator<QueryId> for ReadSet {
    fn from_iter<T: IntoIterator<Item = QueryId>>(iter: T) -> Self {
        let deps: FxHashSet<_> = iter.into_iter().collect();
        Self((!deps.is_empty()).then(|| Rc::new(deps)))
    }
}

pub(crate) struct ReadSetIter<'a>(Option<std::collections::hash_set::Iter<'a, QueryId>>);

impl<'a> Iterator for ReadSetIter<'a> {
    type Item = &'a QueryId;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.as_mut().and_then(Iterator::next)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl ExactSizeIterator for ReadSetIter<'_> {
    fn len(&self) -> usize {
        self.0.as_ref().map_or(0, ExactSizeIterator::len)
    }
}

impl FusedIterator for ReadSetIter<'_> {}

impl<'a> IntoIterator for &'a ReadSet {
    type Item = &'a QueryId;
    type IntoIter = ReadSetIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

// Rust-only representation and ownership checks; language behavior stays in
// the shared corpora, exercised by the three implementation drivers.
#[cfg(test)]
#[path = "../../tests/private/query_graph_test.rs"]
mod tests;
