//! Feature-only census of the value graph that canonical serialization emits.
//!
//! [`census`] visits the values `Engine::serialize` would append, in the same
//! order and under the same omission rules, and counts them. It evaluates
//! nothing: a raw-document `PreVal` is counted and not entered, so whatever it
//! would produce is absent from every other count. The census retains no
//! value, scope or engine, and it is never called by the serializer itself.
//!
//! The module has its own `serialization-census` Cargo feature, which
//! `runtime-diagnostics` implies. Enabled alone it compiles none of the
//! evaluation hooks, whose recording allocates, so the engine whose text
//! placement it observes is the ordinary one.
//!
//! For text it also records where the two allocations behind a [`SharedText`]
//! live, because serialization reaches the characters through the descriptor:
//! the distance from each descriptor to its characters, and the distances
//! between the descriptors and between the characters of consecutively emitted
//! text values. A descriptor shared by many values is counted once in
//! [`TextCensus::distinct_descriptors`]; characters shared by several
//! descriptors are counted once in [`TextCensus::distinct_characters`].
//!
//! Addresses are observations of this process's allocator at the time of the
//! call. They are not stable across runs, they say nothing about cache or page
//! state, and a distance is not a cost. Map keys, member names and raw-object
//! keys are borrowed `String` text without a descriptor; they are counted in
//! [`KeyCensus`] and carry no placement record. The census allocates two
//! address sets proportional to the distinct text allocations it meets.

use crate::semantics::{rec_members, MKind, SharedText, SlotState, Value};
use serde::Serialize;
use std::collections::HashSet;
use std::rc::Rc;

/// Upper bound of [`Distances::up_to_64`]: one cache line.
pub const CACHE_LINE_BYTES: usize = 64;
/// Upper bound of [`Distances::up_to_4_kib`].
pub const SMALL_PAGE_BYTES: usize = 4 * 1024;
/// Upper bound of [`Distances::up_to_16_kib`]: one page on Apple silicon.
pub const LARGE_PAGE_BYTES: usize = 16 * 1024;
/// Upper bound of [`Distances::up_to_2_mib`].
pub const SEGMENT_BYTES: usize = 2 * 1024 * 1024;

/// Absolute address distances, each counted in exactly one bucket.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Distances {
    /// Distances recorded; the five buckets sum to this.
    pub pairs: u64,
    /// At most [`CACHE_LINE_BYTES`] apart, including zero.
    pub up_to_64: u64,
    /// Farther than a cache line and at most [`SMALL_PAGE_BYTES`] apart.
    pub up_to_4_kib: u64,
    /// Farther than that and at most [`LARGE_PAGE_BYTES`] apart.
    pub up_to_16_kib: u64,
    /// Farther than that and at most [`SEGMENT_BYTES`] apart.
    pub up_to_2_mib: u64,
    /// Farther than [`SEGMENT_BYTES`] apart.
    pub beyond_2_mib: u64,
    /// Saturating sum of every recorded distance.
    pub total_bytes: u64,
}

impl Distances {
    fn record(&mut self, a: usize, b: usize) {
        let distance = a.abs_diff(b);
        self.pairs += 1;
        self.total_bytes = self.total_bytes.saturating_add(distance as u64);
        let bucket = if distance <= CACHE_LINE_BYTES {
            &mut self.up_to_64
        } else if distance <= SMALL_PAGE_BYTES {
            &mut self.up_to_4_kib
        } else if distance <= LARGE_PAGE_BYTES {
            &mut self.up_to_16_kib
        } else if distance <= SEGMENT_BYTES {
            &mut self.up_to_2_mib
        } else {
            &mut self.beyond_2_mib
        };
        *bucket += 1;
    }
}

/// Text values in emission order, and the placement of their two allocations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TextCensus {
    /// Emitted `Value::Str` occurrences.
    pub values: u64,
    /// Their UTF-8 bytes before escaping, every occurrence counted.
    pub bytes: u64,
    /// Distinct descriptor allocations among those occurrences.
    pub distinct_descriptors: u64,
    /// Distinct character allocations among those occurrences.
    pub distinct_characters: u64,
    /// From each occurrence's descriptor to its characters.
    pub descriptor_to_characters: Distances,
    /// Between the descriptors of consecutively emitted occurrences.
    pub consecutive_descriptors: Distances,
    /// Between the characters of consecutively emitted occurrences.
    pub consecutive_characters: Distances,
}

/// Emitted map keys, record member names and raw-object keys.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct KeyCensus {
    /// Keys written before an emitted value.
    pub count: u64,
    /// Their UTF-8 bytes before escaping.
    pub bytes: u64,
}

/// Visited values by the form serialization gives them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ValueCounts {
    /// `null` values.
    pub nulls: u64,
    /// Boolean values.
    pub booleans: u64,
    /// Integer values.
    pub integers: u64,
    /// Float values.
    pub floats: u64,
    /// Text values; [`TextCensus`] describes them.
    pub text: u64,
    /// Quantities, each emitted as a value-and-unit object.
    pub quantities: u64,
    /// References, each emitted as freshly formatted path text.
    pub references: u64,
    /// Typed arrays.
    pub arrays: u64,
    /// Typed maps.
    pub maps: u64,
    /// Records.
    pub records: u64,
    /// Raw-document arrays.
    pub raw_arrays: u64,
    /// Raw-document objects.
    pub raw_objects: u64,
    /// Raw-document `PreVal` positions, counted and not evaluated or entered.
    pub unevaluated_raw_values: u64,
    /// Values that append nothing: absent, undefined or without a JSON form.
    pub omitted: u64,
    /// Omitted values in raw-document positions, which serialize as `null`.
    pub raw_null_substitutions: u64,
}

/// What one canonical serialization of a value visits.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SerializationCensus {
    /// Visited values by emitted form.
    pub values: ValueCounts,
    /// Emitted keys and member names.
    pub keys: KeyCensus,
    /// Emitted text values and their placement.
    pub text: TextCensus,
    /// Deepest container nesting; a scalar root is depth zero.
    pub max_depth: u32,
}

/// Count what `Engine::serialize(value, _, settable_only)` would emit.
///
/// The traversal borrows each container exactly as the serializer does and
/// evaluates nothing. It must not run while a container of `value` is mutably
/// borrowed.
pub fn census(value: &Value, settable_only: bool) -> SerializationCensus {
    let mut walker = Walker::default();
    walker.typed(value, settable_only);
    walker.census.text.distinct_descriptors = walker.descriptors.len() as u64;
    walker.census.text.distinct_characters = walker.characters.len() as u64;
    walker.census
}

#[derive(Default)]
struct Walker {
    census: SerializationCensus,
    descriptors: HashSet<usize>,
    characters: HashSet<usize>,
    previous: Option<(usize, usize)>,
    depth: u32,
}

impl Walker {
    fn text(&mut self, text: &SharedText) {
        // `as_rc` borrows the character owner stored inside the descriptor
        // allocation, so its address identifies that descriptor.
        let descriptor = text.as_rc() as *const Rc<str> as usize;
        let characters = text.as_ptr() as usize;
        let record = &mut self.census.text;
        record.values += 1;
        record.bytes += text.len() as u64;
        record
            .descriptor_to_characters
            .record(descriptor, characters);
        if let Some((descriptor_before, characters_before)) = self.previous {
            record
                .consecutive_descriptors
                .record(descriptor_before, descriptor);
            record
                .consecutive_characters
                .record(characters_before, characters);
        }
        self.previous = Some((descriptor, characters));
        self.descriptors.insert(descriptor);
        self.characters.insert(characters);
    }

    fn key(&mut self, key: &str) {
        self.census.keys.count += 1;
        self.census.keys.bytes += key.len() as u64;
    }

    fn enter(&mut self) {
        self.depth += 1;
        self.census.max_depth = self.census.max_depth.max(self.depth);
    }

    // Mirrors `Engine::go`: false means the value appends nothing, so its
    // container omits the member or item and writes no key for it.
    fn typed(&mut self, value: &Value, settable_only: bool) -> bool {
        let counts = &mut self.census.values;
        match value {
            Value::Null => counts.nulls += 1,
            Value::Bool(_) => counts.booleans += 1,
            Value::Int(_) => counts.integers += 1,
            Value::Float(_) => counts.floats += 1,
            Value::Str(text) => {
                counts.text += 1;
                self.text(text);
            }
            Value::Q(_) => counts.quantities += 1,
            Value::Ref(_) => counts.references += 1,
            Value::Arr(array) => {
                counts.arrays += 1;
                self.enter();
                for item in &array.borrow().items {
                    self.typed(item, settable_only);
                }
                self.depth -= 1;
            }
            Value::Map(map) => {
                counts.maps += 1;
                self.enter();
                for (key, item) in &map.borrow().entries {
                    if self.typed(item, settable_only) {
                        self.key(key);
                    }
                }
                self.depth -= 1;
            }
            Value::Rec(record) => {
                counts.records += 1;
                self.enter();
                let record = record.borrow();
                let mut done: HashSet<&str> = HashSet::new();
                for name in record.entry_order.iter() {
                    done.insert(name);
                    if let Some(extra) = record.extra(name) {
                        self.key(name);
                        self.raw(extra);
                        continue;
                    }
                    let Some(slot) = record.slot(name) else {
                        continue;
                    };
                    if matches!(slot.state, SlotState::Invalid | SlotState::Absent)
                        || slot.kind == MKind::Der
                    {
                        continue;
                    }
                    if self.typed(&slot.value, settable_only) {
                        self.key(name);
                    }
                }
                for member in rec_members(&record.rt).iter() {
                    if done.contains(member.name.as_str()) && member.kind != MKind::Der {
                        continue;
                    }
                    if settable_only && member.kind == MKind::Der {
                        continue;
                    }
                    let Some(slot) = record.slot(&member.name) else {
                        continue;
                    };
                    if slot.hidden
                        || matches!(
                            slot.state,
                            SlotState::Invalid
                                | SlotState::Absent
                                | SlotState::Unforced
                                | SlotState::Deferred
                        )
                    {
                        continue;
                    }
                    if self.typed(&slot.value, settable_only) {
                        self.key(&member.name);
                    }
                }
                self.depth -= 1;
            }
            Value::JObj(_) | Value::JArr(_) => self.raw(value),
            _ => {
                counts.omitted += 1;
                return false;
            }
        }
        true
    }

    // Mirrors `Engine::raw_json`, except that a `PreVal` is never evaluated.
    fn raw(&mut self, value: &Value) {
        match value {
            Value::JArr(items) => {
                self.census.values.raw_arrays += 1;
                self.enter();
                for item in items.iter() {
                    self.raw(item);
                }
                self.depth -= 1;
            }
            Value::JObj(entries) => {
                self.census.values.raw_objects += 1;
                self.enter();
                for (key, item) in entries.iter() {
                    self.key(key);
                    self.raw(item);
                }
                self.depth -= 1;
            }
            Value::PreVal(_) => self.census.values.unevaluated_raw_values += 1,
            other => {
                if !self.typed(other, false) {
                    self.census.values.raw_null_substitutions += 1;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/private/serialization_diagnostics_test.rs"]
mod tests;
