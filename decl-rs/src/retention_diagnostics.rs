//! External runtime-diagnostics only: fixed scalar traffic counters, no owner graph.
use serde_json::{json, Value};
use std::cell::RefCell;

/// Array constructor sites; completed counts are allocation traffic, not live owners.
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum ArraySite {
    Bind,
    Materialize,
    SnapshotCopy,
    Rebased,
    Sort,
    SortBy,
    Filter,
    Unique,
    Reverse,
    MapKeys,
    MapValues,
    MapEntries,
    StringSplit,
    StdOther,
    Referrers,
}
const ARRAY_NAMES: [&str; 15] = [
    "bind_typed",
    "materialize_prearr",
    "snapshot_copy",
    "round_rebased",
    "std_sort",
    "std_sort_by",
    "std_filter",
    "std_unique",
    "std_reverse",
    "std_map_keys",
    "std_map_values",
    "std_map_entries",
    "std_string_split",
    "std_other",
    "referrers",
];
const PREVAL_NAMES: [&str; 6] = [
    "tree_object_literal",
    "tree_array_literal",
    "tree_comprehension",
    "default_ref",
    "derived_ref",
    "query_program_pre",
];
const HIST_NAMES: [&str; 16] = [
    "0",
    "1",
    "2",
    "3",
    "4",
    "5..8",
    "9..16",
    "17..32",
    "33..64",
    "65..128",
    "129..256",
    "257..512",
    "513..1024",
    "1025..2048",
    "2049..4096",
    "4097+",
];

#[derive(Default, Clone)]
struct ArrayCounters {
    attempts: u64,
    completed: u64,
    failed_or_unwound: u64,
    expected_items: u64,
    length_mismatches: u64,
    items: u64,
    capacity: u64,
    tail: u64,
    zero_tail: u64,
    len_hist: [u64; 16],
    cap_hist: [u64; 16],
}
#[derive(Default, Clone)]
struct CacheCounters {
    calls: [u64; 2],
    hits: [u64; 2],
    misses: [u64; 2],
    inserted: [u64; 2],
    errors_or_unwound: [u64; 2],
    replacements: [u64; 2],
    clears: u64,
    clear_entries: u64,
    suspends: u64,
    suspend_entries: u64,
    restores: u64,
    discarded_transient_entries: u64,
    restored_entries: u64,
    highest_observed_entries: u64,
    highest_observed_capacity: u64,
}
#[derive(Default, Clone)]
struct Counters {
    arrays: [ArrayCounters; 15],
    cache: CacheCounters,
    preval: [u64; 6],
    local_frames: u64,
}
thread_local! { static COUNTERS: RefCell<Counters> = RefCell::new(Counters::default()); }

fn add(x: &mut u64, n: usize) {
    *x = x.saturating_add(n as u64);
}
fn bucket(n: usize) -> usize {
    match n {
        0..=4 => n,
        5..=8 => 5,
        9..=16 => 6,
        17..=32 => 7,
        33..=64 => 8,
        65..=128 => 9,
        129..=256 => 10,
        257..=512 => 11,
        513..=1024 => 12,
        1025..=2048 => 13,
        2049..=4096 => 14,
        _ => 15,
    }
}

/// Reset this evaluation thread's counters only at a quiescent caller boundary.
/// Reset must not occur while an ArrayAttempt/CacheAttempt is active.
pub fn reset() {
    COUNTERS.with(|s| *s.borrow_mut() = Counters::default());
}

/// A scalar guard counts early failure without retaining a Value, Rc, or borrow.
pub(crate) struct ArrayAttempt {
    site: ArraySite,
    expected: Option<usize>,
    done: bool,
}
impl ArrayAttempt {
    pub(crate) fn start(site: ArraySite, expected: Option<usize>) -> Self {
        COUNTERS.with(|s| {
            let mut s = s.borrow_mut();
            let c = &mut s.arrays[site as usize];
            add(&mut c.attempts, 1);
            if let Some(n) = expected {
                add(&mut c.expected_items, n);
            }
        });
        Self {
            site,
            expected,
            done: false,
        }
    }
    pub(crate) fn finish(mut self, len: usize, capacity: usize) {
        COUNTERS.with(|s| {
            let mut s = s.borrow_mut();
            let c = &mut s.arrays[self.site as usize];
            add(&mut c.completed, 1);
            add(&mut c.items, len);
            add(&mut c.capacity, capacity);
            add(&mut c.tail, capacity.saturating_sub(len));
            add(&mut c.len_hist[bucket(len)], 1);
            add(&mut c.cap_hist[bucket(capacity)], 1);
            if len == capacity {
                add(&mut c.zero_tail, 1);
            }
            if self.expected.is_some_and(|n| n != len) {
                add(&mut c.length_mismatches, 1);
            }
        });
        self.done = true;
    }
}
impl Drop for ArrayAttempt {
    fn drop(&mut self) {
        if !self.done {
            let _ = COUNTERS.try_with(|s| {
                add(
                    &mut s.borrow_mut().arrays[self.site as usize].failed_or_unwound,
                    1,
                )
            });
        }
    }
}
pub(crate) fn array(site: ArraySite, len: usize, capacity: usize) {
    ArrayAttempt::start(site, None).finish(len, capacity);
}
pub(crate) fn standard_array(name: &str, len: usize, capacity: usize) {
    let site = match name {
        "array.sort" => ArraySite::Sort,
        "array.sort_by" => ArraySite::SortBy,
        "array.filter" => ArraySite::Filter,
        "array.unique" => ArraySite::Unique,
        "array.reverse" => ArraySite::Reverse,
        "map.keys" => ArraySite::MapKeys,
        "map.values" => ArraySite::MapValues,
        "map.entries" => ArraySite::MapEntries,
        "string.split" => ArraySite::StringSplit,
        _ => ArraySite::StdOther,
    };
    array(site, len, capacity);
}
pub(crate) fn preval(site: usize) {
    COUNTERS.with(|s| add(&mut s.borrow_mut().preval[site], 1));
}
pub(crate) fn local_frame() {
    COUNTERS.with(|s| add(&mut s.borrow_mut().local_frames, 1));
}

/// Literal kind: 0 PreArr, 1 PreObj. Guards never own the raw or result.
pub(crate) struct CacheAttempt {
    kind: usize,
    done: bool,
}
impl CacheAttempt {
    pub(crate) fn start(kind: usize) -> Self {
        COUNTERS.with(|s| add(&mut s.borrow_mut().cache.calls[kind], 1));
        Self { kind, done: false }
    }
    pub(crate) fn hit(mut self) {
        COUNTERS.with(|s| add(&mut s.borrow_mut().cache.hits[self.kind], 1));
        self.done = true;
    }
    pub(crate) fn miss(&self) {
        COUNTERS.with(|s| add(&mut s.borrow_mut().cache.misses[self.kind], 1));
    }
    pub(crate) fn inserted(mut self, before: usize, after: usize, capacity: usize) {
        COUNTERS.with(|s| {
            let mut s = s.borrow_mut();
            let c = &mut s.cache;
            add(&mut c.inserted[self.kind], 1);
            if after == before {
                add(&mut c.replacements[self.kind], 1);
            }
            c.highest_observed_entries = c.highest_observed_entries.max(after as u64);
            c.highest_observed_capacity = c.highest_observed_capacity.max(capacity as u64);
        });
        self.done = true;
    }
}
impl Drop for CacheAttempt {
    fn drop(&mut self) {
        if !self.done {
            let _ = COUNTERS
                .try_with(|s| add(&mut s.borrow_mut().cache.errors_or_unwound[self.kind], 1));
        }
    }
}
pub(crate) fn cache_clear(entries: usize) {
    COUNTERS.with(|s| {
        let mut s = s.borrow_mut();
        add(&mut s.cache.clears, 1);
        add(&mut s.cache.clear_entries, entries);
    });
}
pub(crate) fn cache_suspend(entries: usize) {
    COUNTERS.with(|s| {
        let mut s = s.borrow_mut();
        add(&mut s.cache.suspends, 1);
        add(&mut s.cache.suspend_entries, entries);
    });
}
pub(crate) fn cache_restore(discarded: usize, restored: usize) {
    COUNTERS.with(|s| {
        let mut s = s.borrow_mut();
        add(&mut s.cache.restores, 1);
        add(&mut s.cache.discarded_transient_entries, discarded);
        add(&mut s.cache.restored_entries, restored);
    });
}

/// Read fixed counters. JSON allocation occurs here only, outside measured work.
/// Counts sum constructor/cache traffic across all Engines on this thread.
pub fn snapshot() -> Value {
    let s = COUNTERS.with(|s| s.borrow().clone());
    let arrays: Vec<_>=s.arrays.iter().zip(ARRAY_NAMES).map(|(c,name)|json!({"site":name,"attempts":c.attempts,"completed":c.completed,"failed_or_unwound":c.failed_or_unwound,"known_expected_items":c.expected_items,"successful_length_mismatches":c.length_mismatches,"completed_len_sum":c.items,"completed_capacity_sum":c.capacity,"completed_unused_tail_sum":c.tail,"completed_tight_buffers":c.zero_tail,"length_histogram":c.len_hist,"capacity_histogram":c.cap_hist})).collect();
    json!({"schema":1,"arrays":arrays,"histogram_buckets":HIST_NAMES,
        "mat_cache":{"kinds":["PreArr","PreObj"],"calls":s.cache.calls,"hits":s.cache.hits,"misses":s.cache.misses,"inserted":s.cache.inserted,"errors_or_unwound":s.cache.errors_or_unwound,"replacements":s.cache.replacements,"clear_calls":s.cache.clears,"clear_entries":s.cache.clear_entries,"suspend_calls":s.cache.suspends,"suspended_entries":s.cache.suspend_entries,"restore_calls":s.cache.restores,"discarded_transient_entries":s.cache.discarded_transient_entries,"restored_entries":s.cache.restored_entries,"highest_observed_single_cache_entries":s.cache.highest_observed_entries,"highest_observed_single_cache_capacity":s.cache.highest_observed_capacity},
        "preval_constructor_sites":PREVAL_NAMES,"preval_constructed":s.preval,"local_frames_constructed":s.local_frames,
        "units":{"array_lengths":"Value elements at successful constructor completion; may refer to shared child values","value_size_bytes":std::mem::size_of::<crate::semantics::Value>(),"counter_saturation":"u64 saturating counts"},
        "limitations":["constructor traffic is not unique retained ownership, allocation counts or bytes freed","failed std-library construction before final ArrV factory is outside factory counts","no destructor/live PreVal or LocalFrame census; external callers can construct public PreValV/ArrV without hooks","no graph walk, callback invocation, force, cache mutation or Rc owner held by counters","reset only at a quiescent evaluation-thread boundary; counters include any Engines executed after reset","diagnostic TLS/branch overhead is real; plain primary builds must exclude this feature"]})
}
