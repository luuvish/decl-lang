//! Replay Session root binding over retained records and member revisions.
use crate::engine::{Engine, Inst, RootSrc};
use crate::qengine::graph::{QueryId, ReadSet};
use crate::qengine::revisions::{comparable, Matches, Revisions};
use crate::semantics::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// A pair of process-wide clocks, sampled only at edit phase boundaries.
#[cfg(feature = "runtime-diagnostics")]
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct EditStamp {
    /// Monotonic wall-clock timestamp in nanoseconds, with an unspecified epoch.
    pub wall_ns: u64,
    /// Process CPU-clock timestamp in nanoseconds, including all process threads.
    pub cpu_ns: u64,
    /// False if either clock could not be read. Clocks are not sampled atomically.
    pub valid: bool,
}

#[cfg(feature = "runtime-diagnostics")]
impl EditStamp {
    fn now() -> Self {
        fn read(id: libc::clockid_t) -> Option<u64> {
            let mut time = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // Diagnostic clock failure must not change evaluation behavior.
            if unsafe { libc::clock_gettime(id, &mut time) } != 0 {
                return None;
            }
            u64::try_from(time.tv_sec)
                .ok()?
                .checked_mul(1_000_000_000)?
                .checked_add(u64::try_from(time.tv_nsec).ok()?)
        }
        match (
            read(libc::CLOCK_MONOTONIC),
            read(libc::CLOCK_PROCESS_CPUTIME_ID),
        ) {
            (Some(wall_ns), Some(cpu_ns)) => Self {
                wall_ns,
                cpu_ns,
                valid: true,
            },
            _ => Self::default(),
        }
    }
}

/// Disjoint interval in one preparation or finish call, including its bookkeeping.
#[cfg(feature = "runtime-diagnostics")]
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct EditPhase {
    /// Fixed phase identifier, in execution order within its containing array.
    pub name: &'static str,
    /// Opening clock sample; consecutive recorded phases share their boundary.
    pub start: EditStamp,
    /// Closing clock sample, including partial work when unwinding.
    pub end: EditStamp,
    /// Elapsed wall nanoseconds, including instrumentation; zero when invalid.
    pub wall_ns: u64,
    /// Elapsed process CPU nanoseconds, including instrumentation; zero when invalid.
    pub cpu_ns: u64,
    /// Whether this phase was entered and its closing boundary was recorded.
    pub recorded: bool,
    /// Whether this interval finished normally rather than during unwinding.
    pub completed: bool,
    /// Whether both boundary samples succeeded and neither clock decreased.
    pub valid: bool,
}

#[cfg(feature = "runtime-diagnostics")]
impl EditPhase {
    fn empty(name: &'static str) -> Self {
        Self {
            name,
            start: EditStamp::default(),
            end: EditStamp::default(),
            wall_ns: 0,
            cpu_ns: 0,
            recorded: false,
            completed: false,
            valid: false,
        }
    }
}

/// Counts collected from existing traversals; no query names or values are retained.
/// All counts concern one preparation call. Table sizes describe observed state,
/// while scans, writes, and enqueues count work, including repeated entries.
/// Incomplete preparations may leave later counters at their initial zero values.
#[cfg(feature = "runtime-diagnostics")]
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct EditPreparationWork {
    /// Number of requested changed-root names, including any repeated names.
    pub changed_roots: u64,
    /// Whether the prior environment diagnostic list was nonempty, regardless of severity.
    pub previous_errors: bool,
    /// Entries in the initial registry snapshot, including duplicate record handles.
    pub registry_records: u64,
    /// Distinct canonical paths in the retained-record pool after the root census.
    pub pooled_records: u64,
    /// Distinct root names in the saved root-value table after the census.
    pub saved_roots: u64,
    /// Root query entries saved with their read sets, including empty read sets.
    pub saved_root_reads: u64,
    /// Changed-root visits with both a prior document and a next document to diff.
    pub diff_roots: u64,
    /// Changed-root visits using whole-value collection instead of document diffing.
    pub collected_roots: u64,
    /// Distinct known producer keys after processing all requested changed roots.
    pub known_producers: u64,
    /// Cumulative unique forced counts after diff, special reads, saved values,
    /// retention checks, and the final error fallback. These reasons overlap.
    /// Adjacent differences depend on this order and are not disjoint causal totals.
    pub forced_after_seeding: [u64; 5],
    /// Query entries scanned in the existing dependency graph, including empty read sets.
    pub read_queries: u64,
    /// Dependency entries traversed across all query read sets.
    pub read_edges: u64,
    /// Dependency occurrences, in edge/snapshot/round/value order (not queries).
    pub special_edges: [u64; 4],
    /// Distinct dependency keys in the temporary reverse index.
    pub reverse_keys: u64,
    /// Distinct reverse-index dependency keys with the whole-value `value:` prefix.
    pub value_dependency_keys: u64,
    /// Final reverse degrees: 1, 2, 3, 4, 5..8, 9..32, 33..128, 129+.
    pub reverse_degree_buckets: [u64; 8],
    /// Largest number of reader entries attached to any one dependency key.
    pub reverse_max_degree: u64,
    /// Sum of reverse-reader vector capacities, in entries rather than bytes.
    pub reverse_vector_capacity: u64,
    /// Slot visits across the registry snapshot, including repeated record entries.
    pub registered_slots_scanned: u64,
    /// Writes of Ok or Absent slot values, including overwrites of an existing key.
    pub saved_slot_writes: u64,
    /// Constant entries visited across all registered constant environments.
    pub constants_scanned: u64,
    /// Writes of evaluated constant values, including any existing-key overwrites.
    pub saved_constant_writes: u64,
    /// Distinct root, slot, and constant keys in the completed saved-value table.
    pub saved_values: u64,
    /// Saved-value hash-table capacity in entries, not bytes or allocated buckets.
    pub saved_values_capacity: u64,
    /// Top-level saved values checked for safe retention; recursive visits are excluded.
    pub retention_checks: u64,
    /// Top-level retention checks that failed, whether or not their keys were already forced.
    pub retention_rejections: u64,
    /// Forced keys placed in the queue before whole-value owner expansion.
    pub initial_queue: u64,
    /// Whole-value dependency owners examined, stopping at the first missing owner.
    pub value_owners_examined: u64,
    /// Examined whole-value owners found in the retained-record pool.
    pub value_owners_found: u64,
    /// Whether a missing whole-value owner triggered the conservative all-values fallback.
    pub missing_owner_fallback: bool,
    /// Saved-value keys enqueued by that fallback, without checking prior queue membership.
    pub fallback_enqueues: u64,
    /// Descendant keys enqueued from found whole-value owners, before closure traversal.
    pub owner_descendant_enqueues: u64,
    /// Descendant-collection calls for dequeued keys outside the known-producer set.
    pub producer_collects: u64,
    /// Producer descendant keys enqueued after filtering already-invalid keys.
    pub producer_descendant_enqueues: u64,
    /// First visits to known producer keys that skip descendant collection.
    pub known_producers_skipped: u64,
    /// Reverse-reader entries enqueued, including keys already queued or invalid.
    pub reader_enqueues: u64,
    /// Total queue removals, including repeated visits to the same key.
    pub queue_pops: u64,
    /// Queue removals whose key had already entered the invalid set.
    pub duplicate_pops: u64,
    /// Maximum queue length observed at its initial fill and each extension boundary.
    pub queue_peak_len: u64,
    /// Maximum queue capacity observed at the same boundaries, in key entries.
    pub queue_peak_capacity: u64,
    /// Distinct record, array, and map identities visited by closure collection.
    pub walked_values: u64,
    /// Distinct keys in the final invalid set, matching the prepared-query count.
    pub invalid_queries: u64,
    /// One call per Pending value, excluding recursive capture/memo visits.
    pub pending_captures: u64,
    /// Record, mutable collection, immutable collection, other root captures.
    pub pending_capture_kinds: [u64; 4],
    /// Capture-memo entries after Pending creation and before the memo is dropped.
    pub capture_memo_entries: u64,
    /// Computed slots assigned Unforced/Undef during reset, regardless of their prior state.
    pub reset_slots: u64,
    /// Invalid constants assigned unevaluated/Undef during reset.
    pub reset_constants: u64,
}

/// Table cardinalities sampled at fixed finish boundaries, not traversal counts.
/// An inactive finish runs only prune; fields for later phases remain zero.
#[cfg(feature = "runtime-diagnostics")]
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct EditFinishWork {
    /// Cached input descriptions before and after prune.
    pub input_entries: [u64; 2],
    /// Cached root descriptions before prune, after prune, and after graph prune.
    pub root_entries: [u64; 3],
    /// Registered query-slot entries before and after graph prune.
    pub slot_entries: [u64; 2],
    /// Query read-set entries before and after graph prune, including empty sets.
    pub read_entries: [u64; 2],
    /// Retained revision stamps before and after releasing Pending values and pruning stamps.
    pub revision_entries: [u64; 2],
}

/// Sum of one finish phase across separate calls; this is not a continuous interval.
#[cfg(feature = "runtime-diagnostics")]
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct EditFinishPhaseTotal {
    /// Fixed phase identifier, in the same order as the latest finish phases.
    pub name: &'static str,
    /// Recorded intervals included in this sum, including partial intervals.
    pub recorded_calls: u64,
    /// Sum of valid interval wall nanoseconds, excluding gaps between calls.
    pub wall_ns: u64,
    /// Sum of valid interval process CPU nanoseconds, excluding gaps between calls.
    pub cpu_ns: u64,
    /// Whether every included interval had valid clocks and its totals fit in u64.
    pub valid: bool,
}

#[cfg(feature = "runtime-diagnostics")]
impl EditFinishPhaseTotal {
    fn empty(name: &'static str) -> Self {
        Self {
            name,
            recorded_calls: 0,
            wall_ns: 0,
            cpu_ns: 0,
            valid: true,
        }
    }
}

/// Bounded finish attribution since the latest begin, reset by the next begin.
/// Phase sums exclude work between calls and must not be added to the latest
/// finish again. Before the first begin, sequence-zero finishes share this tally.
#[cfg(feature = "runtime-diagnostics")]
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct EditFinishAggregate {
    /// Calls that returned or unwound, including calls omitted because of reentry.
    pub calls: u64,
    /// Counted calls that passed the active check after prune.
    pub active_calls: u64,
    /// Counted calls that did not complete normally.
    pub partial_calls: u64,
    /// Outer calls omitted after same-preparation reentry superseded their trace.
    /// Their intervals may overlap newer calls, so adding them would double-count.
    pub superseded_calls: u64,
    /// Complete unambiguous coverage: no partial/omitted call, clock failure, or overflow.
    /// True with zero calls denotes an empty tally, not a measured finish.
    pub valid: bool,
    /// Noncontiguous per-phase sums; no endpoints or between-call duration are implied.
    pub phases: [EditFinishPhaseTotal; 6],
}

#[cfg(feature = "runtime-diagnostics")]
impl Default for EditFinishAggregate {
    fn default() -> Self {
        Self {
            calls: 0,
            active_calls: 0,
            partial_calls: 0,
            superseded_calls: 0,
            valid: true,
            phases: [
                "prune",
                "graph_prune",
                "revisions_release",
                "caches_release",
                "query_sweep",
                "scope_release",
            ]
            .map(EditFinishPhaseTotal::empty),
        }
    }
}

#[cfg(feature = "runtime-diagnostics")]
impl EditFinishAggregate {
    fn add(&mut self, report: &SessionEditDiagnostics, superseded: bool) {
        fn add(total: &mut u64, value: u64) -> bool {
            if let Some(next) = total.checked_add(value) {
                *total = next;
                true
            } else {
                *total = u64::MAX;
                false
            }
        }
        self.valid &= add(&mut self.calls, 1);
        self.valid &= add(&mut self.active_calls, u64::from(report.finish_was_active));
        self.valid &= add(&mut self.partial_calls, u64::from(!report.finish_completed));
        self.valid &= add(&mut self.superseded_calls, u64::from(superseded));
        self.valid &= report.finish_completed && !superseded;
        if superseded {
            return;
        }
        for (total, phase) in self.phases.iter_mut().zip(&report.finish) {
            if phase.recorded {
                total.valid &= add(&mut total.recorded_calls, 1);
                total.valid &= add(&mut total.wall_ns, phase.wall_ns);
                total.valid &= add(&mut total.cpu_ns, phase.cpu_ns);
                total.valid &= phase.valid;
                self.valid &= total.valid;
            }
        }
    }
}

/// Latest edit attribution. Native builds without runtime-diagnostics contain
/// none of these clocks, counters, or storage. Access is a fixed-size copy.
/// A hot/equal run without begin preserves `sequence`; finish has its own count.
#[cfg(feature = "runtime-diagnostics")]
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct SessionEditDiagnostics {
    /// Diagnostic serialization schema version; currently 1.
    pub schema: u32,
    /// Wrapping preparation-call count; zero before the first begin call.
    pub sequence: u64,
    /// Wrapping finish-call count, including inactive prune-only calls.
    pub finish_sequence: u64,
    /// Revision immediately after Pending creation; later reference rounds may advance it.
    pub revision: usize,
    /// Whether the latest preparation and its original scratch drops completed normally.
    pub preparation_completed: bool,
    /// Whether the latest finish completed normally; reset by the next preparation.
    pub finish_completed: bool,
    /// Whether the latest finish remained active after prune and entered graph cleanup.
    pub finish_was_active: bool,
    /// Ordered preparation intervals from root census through implicit scratch release.
    /// Normal rebinding and member evaluation follow this call; native callbacks
    /// triggered by drops remain inside the phase that triggered them.
    pub preparation: [EditPhase; 9],
    /// Ordered finish intervals from prune through implicit scope release.
    /// An inactive finish records only the first interval.
    pub finish: [EditPhase; 6],
    /// State sizes and work counts from the latest preparation, retained across finish calls.
    pub preparation_work: EditPreparationWork,
    /// State sizes from the latest finish, reset on each begin or finish call.
    pub finish_work: EditFinishWork,
    /// All finish calls associated with this preparation, including later inactive calls.
    pub finish_aggregate: EditFinishAggregate,
}

#[cfg(feature = "runtime-diagnostics")]
impl Default for SessionEditDiagnostics {
    fn default() -> Self {
        Self {
            schema: 1,
            sequence: 0,
            finish_sequence: 0,
            revision: 0,
            preparation_completed: false,
            finish_completed: false,
            finish_was_active: false,
            preparation: [
                "root_census",
                "changed_seeds",
                "reverse_index",
                "saved_values",
                "retention_seeds",
                "invalidation_closure",
                "pending_capture",
                "reset",
                "scratch_release",
            ]
            .map(EditPhase::empty),
            finish: [
                "prune",
                "graph_prune",
                "revisions_release",
                "caches_release",
                "query_sweep",
                "scope_release",
            ]
            .map(EditPhase::empty),
            preparation_work: EditPreparationWork::default(),
            finish_work: EditFinishWork::default(),
            finish_aggregate: EditFinishAggregate::default(),
        }
    }
}

/// Declared before the original locals, so the last interval includes their
/// existing implicit drops. Holds no runtime owner and no diagnostic borrow
/// across native callbacks. A nested edit supersedes this call's publication.
#[cfg(feature = "runtime-diagnostics")]
struct EditTrace<'a> {
    edits: &'a Edits,
    epoch: u64,
    report: SessionEditDiagnostics,
    preparing: bool,
    next: usize,
    stamp: EditStamp,
    completed: bool,
    close_on_drop: bool,
}

#[cfg(feature = "runtime-diagnostics")]
impl<'a> EditTrace<'a> {
    fn new(edits: &'a Edits, preparing: bool) -> Self {
        let stamp = EditStamp::now();
        let old = edits.diagnostics.get();
        let report = if preparing {
            SessionEditDiagnostics {
                sequence: old.sequence.wrapping_add(1),
                finish_sequence: old.finish_sequence,
                ..SessionEditDiagnostics::default()
            }
        } else {
            SessionEditDiagnostics {
                finish_sequence: old.finish_sequence.wrapping_add(1),
                finish: SessionEditDiagnostics::default().finish,
                finish_work: EditFinishWork::default(),
                finish_completed: false,
                finish_was_active: false,
                ..old
            }
        };
        let epoch = edits.diagnostic_epoch.get().wrapping_add(1);
        edits.diagnostic_epoch.set(epoch);
        edits.diagnostics.set(report);
        Self {
            edits,
            epoch,
            report,
            preparing,
            next: 0,
            stamp,
            completed: false,
            close_on_drop: true,
        }
    }

    fn mark(&mut self, completed: bool) {
        let end = EditStamp::now();
        let phase = if self.preparing {
            &mut self.report.preparation[self.next]
        } else {
            &mut self.report.finish[self.next]
        };
        phase.start = self.stamp;
        phase.end = end;
        phase.recorded = true;
        phase.completed = completed;
        phase.valid = self.stamp.valid
            && end.valid
            && end.wall_ns >= self.stamp.wall_ns
            && end.cpu_ns >= self.stamp.cpu_ns;
        if phase.valid {
            phase.wall_ns = end.wall_ns - self.stamp.wall_ns;
            phase.cpu_ns = end.cpu_ns - self.stamp.cpu_ns;
        }
        self.stamp = end;
        self.next += 1;
    }

    fn queue(&mut self, queue: &Vec<Cow<'_, str>>) {
        let work = &mut self.report.preparation_work;
        work.queue_peak_len = work.queue_peak_len.max(queue.len() as u64);
        work.queue_peak_capacity = work.queue_peak_capacity.max(queue.capacity() as u64);
    }

    fn degree(n: usize) -> usize {
        match n {
            1 => 0,
            2 => 1,
            3 => 2,
            4 => 3,
            5..=8 => 4,
            9..=32 => 5,
            33..=128 => 6,
            _ => 7,
        }
    }
}

#[cfg(feature = "runtime-diagnostics")]
impl Drop for EditTrace<'_> {
    fn drop(&mut self) {
        let completed = self.completed && !std::thread::panicking();
        if self.close_on_drop {
            self.mark(completed);
        }
        if self.preparing {
            self.report.preparation_completed = completed;
            if self.edits.diagnostic_epoch.get() == self.epoch {
                self.edits.diagnostics.set(self.report);
            }
        } else {
            self.report.finish_completed = completed;
            let mut current = self.edits.diagnostics.get();
            // A newly prepared edit owns a separate aggregate. Old callback
            // scopes must not add their times to the new preparation.
            if current.sequence != self.report.sequence {
                return;
            }
            let superseded = self.edits.diagnostic_epoch.get() != self.epoch;
            current.finish_aggregate.add(&self.report, superseded);
            if !superseded {
                self.report.finish_aggregate = current.finish_aggregate;
                self.edits.diagnostics.set(self.report);
            } else {
                // Preserve the newer latest-call report. The aggregate records
                // omitted coverage explicitly rather than summing nested time.
                self.edits.diagnostics.set(current);
            }
        }
    }
}

fn same_rc<T>(a: &Option<Rc<T>>, b: &Option<Rc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn root_exists(eng: &Engine, name: &str) -> bool {
    eng.env
        .roots
        .borrow()
        .borrow()
        .iter()
        .any(|(n, _)| n == name)
}

/// Raw Session descriptions use the first occurrence of a repeated key. Keep
/// narrow or infrequently searched descriptions as a slice; amortize a borrowed
/// index only over wider repeated searches. The index owns no input payloads.
struct FirstInputs<'a> {
    entries: &'a [(String, Value)],
    index: Option<FxHashMap<&'a str, &'a Value>>,
}

impl<'a> FirstInputs<'a> {
    fn new(entries: &'a [(String, Value)], lookups: usize) -> Self {
        let index =
            if entries.len() > 8 && lookups > 1 && entries.len().saturating_mul(lookups) > 64 {
                let mut index = FxHashMap::default();
                for (key, value) in entries {
                    index.entry(key.as_str()).or_insert(value);
                }
                Some(index)
            } else {
                None
            };
        Self { entries, index }
    }

    fn get(&self, name: &str) -> Option<&'a Value> {
        match &self.index {
            Some(index) => index.get(name).copied(),
            None => self
                .entries
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value),
        }
    }
}

fn same_raw(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::JArr(a), Value::JArr(b)) => {
            Rc::ptr_eq(a, b)
                || a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_raw(a, b))
        }
        (Value::JObj(a), Value::JObj(b)) => {
            Rc::ptr_eq(a, b)
                || a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((ak, a), (bk, b))| ak == bk && same_raw(a, b))
        }
        (Value::PreVal(a), Value::PreVal(b)) => {
            let (x, y) = (&a.scope, &b.scope);
            let (al, bl) = (x.locals.entries(), y.locals.entries());
            Rc::ptr_eq(a, b)
                || Rc::ptr_eq(&a.expr, &b.expr)
                    && same_rc(&x.inst, &y.inst)
                    && same_rc(&x.menv, &y.menv)
                    && x.root_name == y.root_name
                    && al.len() == bl.len()
                    && al
                        .iter()
                        .all(|(k, v)| bl.get(k).is_some_and(|w| same_raw(v, w)))
        }
        (Value::Rec(a), Value::Rec(b)) => Rc::ptr_eq(a, b),
        (Value::Arr(a), Value::Arr(b)) => Rc::ptr_eq(a, b),
        (Value::Map(a), Value::Map(b)) => Rc::ptr_eq(a, b),
        (Value::Int(_), Value::Int(_))
        | (Value::Float(_), Value::Float(_))
        | (Value::Str(_), Value::Str(_))
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Null, Value::Null)
        | (Value::Absent, Value::Absent)
        | (Value::Undef, Value::Undef)
        | (Value::Q(_), Value::Q(_)) => value_eq(a, b),
        _ => false,
    }
}

#[derive(Hash, PartialEq, Eq)]
enum CaptureKey {
    Record(usize),
    Array(usize),
    Map(usize),
    Int(i64),
    Float(u64),
    String(Rc<str>),
    Bool(bool),
    Null,
    Absent,
    Undef,
}
fn capture(value: &Value, eng: &Engine, memo: &mut FxHashMap<CaptureKey, Matches>) -> Matches {
    let key = match value {
        Value::Rec(r) => Some(CaptureKey::Record(Rc::as_ptr(r) as usize)),
        Value::Arr(a) => Some(CaptureKey::Array(Rc::as_ptr(a) as usize)),
        Value::Map(m) => Some(CaptureKey::Map(Rc::as_ptr(m) as usize)),
        Value::Int(Num::Small(n)) => Some(CaptureKey::Int(*n)),
        Value::Float(n) => Some(CaptureKey::Float(n.to_bits())),
        Value::Str(s) => Some(CaptureKey::String(s.to_rc())),
        Value::Bool(b) => Some(CaptureKey::Bool(*b)),
        Value::Null => Some(CaptureKey::Null),
        Value::Absent => Some(CaptureKey::Absent),
        Value::Undef => Some(CaptureKey::Undef),
        _ => None,
    };
    if let Some(prior) = key.as_ref().and_then(|k| memo.get(k)) {
        return prior.clone();
    }
    let matches = capture_shape(value, eng, memo);
    if let Some(key) = key {
        memo.insert(key, matches.clone());
    }
    matches
}
fn capture_shape(
    value: &Value,
    eng: &Engine,
    memo: &mut FxHashMap<CaptureKey, Matches>,
) -> Matches {
    match value {
        Value::Absent => Rc::new(|n, _| matches!(n, Value::Absent)),
        Value::Rec(inst) => {
            let identity = Rc::as_ptr(inst) as usize;
            let b = inst.borrow();
            let rt = b.rt.clone();
            let order = b.entry_order.clone();
            let members: Vec<_> = b
                .slots
                .iter()
                .map(|(n, s)| (n.clone(), s.kind, s.hidden))
                .collect();
            let extras: Vec<_> = b
                .extras
                .iter()
                .map(|(k, v)| (k.clone(), capture(v, eng, memo)))
                .collect();
            Rc::new(move |next, eng| {
                let Value::Rec(n) = next else {
                    return false;
                };
                let b = n.borrow();
                Rc::as_ptr(n) as usize == identity
                    && Rc::ptr_eq(&rt, &b.rt)
                    && b.entry_order == order
                    && b.slots.len() == members.len()
                    && b.slots
                        .iter()
                        .zip(&members)
                        .all(|((n, s), (k, kind, hidden))| {
                            n == k && s.kind == *kind && s.hidden == *hidden
                        })
                    && b.extras.len() == extras.len()
                    && b.extras
                        .iter()
                        .zip(&extras)
                        .all(|((k, v), (n, matches))| k == n && matches(v, eng))
            })
        }
        Value::Arr(a) => {
            let a = a.borrow();
            let path = a.path.clone();
            let items: Vec<_> = a.items.iter().map(|v| capture(v, eng, memo)).collect();
            Rc::new(move |n, eng| {
                let Value::Arr(n) = n else {
                    return false;
                };
                let n = n.borrow();
                n.path == path
                    && n.items.len() == items.len()
                    && n.items.iter().zip(&items).all(|(v, m)| m(v, eng))
            })
        }
        Value::Map(m) => {
            let m = m.borrow();
            let path = m.path.clone();
            let entries: Vec<_> = m
                .entries
                .iter()
                .map(|(k, v)| (k.clone(), capture(v, eng, memo)))
                .collect();
            Rc::new(move |n, eng| {
                let Value::Map(n) = n else {
                    return false;
                };
                let n = n.borrow();
                n.path == path
                    && n.entries.len() == entries.len()
                    && n.entries
                        .iter()
                        .zip(&entries)
                        .all(|((k, v), (l, m))| k == l && m(v, eng))
            })
        }
        _ if comparable(value, eng) => {
            let old = value.clone();
            Rc::new(move |n, eng| comparable(n, eng) && value_eq(&old, n))
        }
        _ => Rc::new(|_, _| false),
    }
}

#[derive(Clone)]
enum RootInput {
    Expr(Rc<crate::ast::Expr>),
    Doc(Value),
}
type Inputs = FxHashMap<usize, (Weak<RefCell<RecInst>>, Vec<(String, Value)>)>;

/// Retained Session queries, sharing the Engine's member cache.
pub struct Edits {
    /// Whether an edit is currently being materialized.
    pub active: Cell<bool>,
    /// Revision verification and result cutoff counters.
    pub revisions: Rc<Revisions>,
    /// Member programs actually executed, including reference rounds.
    pub slot_computes: Cell<usize>,
    /// Record instances retained in the current edit's first materialization.
    pub retained_records: Cell<usize>,
    /// Queries prepared for verification in the latest edit.
    pub prepared_queries: Cell<usize>,
    #[cfg(feature = "runtime-diagnostics")]
    diagnostics: Cell<SessionEditDiagnostics>,
    #[cfg(feature = "runtime-diagnostics")]
    diagnostic_epoch: Cell<u64>,
    invalid: RefCell<FxHashSet<String>>,
    root_values: RefCell<FxHashMap<String, Value>>,
    root_reads: RefCell<FxHashMap<String, ReadSet>>,
    binding_reuse: Cell<bool>,
    inputs: RefCell<Inputs>,
    roots: RefCell<FxHashMap<String, RootInput>>,
    pool: RefCell<FxHashMap<String, Inst>>,
    live: RefCell<FxHashSet<usize>>,
}

impl Default for Edits {
    fn default() -> Self {
        let revisions = Rc::new(Revisions::default());
        revisions.resolve.set(Some(Self::resolve));
        Self {
            active: Cell::new(false),
            revisions,
            slot_computes: Cell::new(0),
            retained_records: Cell::new(0),
            prepared_queries: Cell::new(0),
            #[cfg(feature = "runtime-diagnostics")]
            diagnostics: Cell::new(SessionEditDiagnostics::default()),
            #[cfg(feature = "runtime-diagnostics")]
            diagnostic_epoch: Cell::new(0),
            invalid: RefCell::new(FxHashSet::default()),
            root_values: RefCell::new(FxHashMap::default()),
            root_reads: RefCell::new(FxHashMap::default()),
            binding_reuse: Cell::new(true),
            inputs: RefCell::new(FxHashMap::default()),
            roots: RefCell::new(FxHashMap::default()),
            pool: RefCell::new(FxHashMap::default()),
            live: RefCell::new(FxHashSet::default()),
        }
    }
}

impl Edits {
    /// Copy the latest bounded Session edit attribution without reading clocks.
    /// Phase work includes instrumentation; it is not a native timing claim.
    #[cfg(feature = "runtime-diagnostics")]
    pub fn diagnostics(&self) -> SessionEditDiagnostics {
        self.diagnostics.get()
    }

    fn resolve(eng: &Engine, key: &str) -> R<bool> {
        let slot = eng.query_slot_shared(key);
        if let Some((inst, name)) = slot {
            eng.force_slot(&inst, &name)?;
            return Ok(true);
        }
        if let Some((index, name)) = key.strip_prefix("const:").and_then(|k| k.split_once('|')) {
            let env = index
                .parse::<usize>()
                .ok()
                .and_then(|i| eng.const_envs.borrow().get(i).cloned());
            if let Some(env) = env {
                if env.consts.borrow().contains_key(name) {
                    eng.force_const_in(&env, name, "")?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn retain(&self, eng: &Engine, value: &Value, seen: &mut FxHashSet<usize>) -> bool {
        match value {
            Value::Rec(inst) => {
                let id = Rc::as_ptr(inst) as usize;
                if !seen.insert(id) {
                    return true;
                }
                let path = path_str(&inst.borrow().path, None);
                self.pool
                    .borrow()
                    .get(&path)
                    .is_some_and(|old| Rc::ptr_eq(old, inst))
                    && self.inputs.borrow().get(&id).is_none_or(|(_, entries)| {
                        entries.iter().all(|(_, v)| self.retain(eng, v, seen))
                    })
            }
            Value::Arr(a) => a.borrow().items.iter().all(|v| self.retain(eng, v, seen)),
            Value::Map(m) => m
                .borrow()
                .entries
                .iter()
                .all(|(_, v)| self.retain(eng, v, seen)),
            Value::JArr(a) => a.iter().all(|v| self.retain(eng, v, seen)),
            Value::JObj(m) => m.iter().all(|(_, v)| self.retain(eng, v, seen)),
            Value::PreVal(p) => {
                p.scope.inst.as_ref().is_none_or(|i| {
                    self.pool
                        .borrow()
                        .get(&path_str(&i.borrow().path, None))
                        .is_some_and(|old| Rc::ptr_eq(old, i))
                }) && p
                    .scope
                    .locals
                    .entries()
                    .values()
                    .all(|v| self.retain(eng, v, seen))
            }
            _ => comparable(value, eng),
        }
    }

    fn collect(
        eng: &Engine,
        value: &Value,
        into: &mut FxHashSet<String>,
        seen: &mut FxHashSet<(u8, usize)>,
    ) {
        let identity = match value {
            Value::Rec(r) => (0, Rc::as_ptr(r) as usize),
            Value::Arr(a) => (1, Rc::as_ptr(a) as usize),
            Value::Map(m) => (2, Rc::as_ptr(m) as usize),
            _ => return,
        };
        if !seen.insert(identity) {
            return;
        }
        match value {
            Value::Rec(inst) => {
                let b = inst.borrow();
                let path = path_str(&b.path, None);
                for (name, slot) in &b.slots {
                    let key = format!("{path}.{name}");
                    eng.register_query_slot(&key, inst.clone(), name.clone());
                    into.insert(key);
                    if slot.state == SlotState::Ok {
                        Self::collect(eng, &slot.value, into, seen);
                    }
                }
            }
            Value::Arr(a) => {
                for v in &a.borrow().items {
                    Self::collect(eng, v, into, seen);
                }
            }
            Value::Map(m) => {
                for v in m.borrow().entries.values() {
                    Self::collect(eng, v, into, seen);
                }
            }
            _ => {}
        }
    }

    fn diff(
        eng: &Engine,
        before: &Value,
        next: &Value,
        value: &Value,
        producer: &str,
        forced: &mut FxHashSet<String>,
        known: &mut FxHashSet<String>,
    ) {
        if same_raw(before, next) {
            return;
        }
        known.insert(producer.into());
        forced.insert(producer.into());
        match (value, before, next) {
            (Value::Rec(inst), Value::JObj(a), Value::JObj(b)) => {
                let names: FxHashSet<_> = a.iter().chain(b.iter()).map(|(k, _)| k).collect();
                let before = FirstInputs::new(a, names.len());
                let next = FirstInputs::new(b, names.len());
                for name in names {
                    let av = before.get(name);
                    let bv = next.get(name);
                    if av.is_some() == bv.is_some()
                        && same_raw(av.unwrap_or(&Value::Undef), bv.unwrap_or(&Value::Undef))
                    {
                        continue;
                    }
                    let bound = inst.borrow();
                    if let Some(slot) = bound.slot(name) {
                        let key = format!("{}.{name}", path_str(&bound.path, None));
                        eng.register_query_slot(&key, inst.clone(), Rc::from(name.as_str()));
                        Self::diff(
                            eng,
                            av.unwrap_or(&Value::Undef),
                            bv.unwrap_or(&Value::Undef),
                            &slot.value,
                            &key,
                            forced,
                            known,
                        );
                        forced.insert(key);
                    }
                }
            }
            (Value::Arr(v), Value::JArr(a), Value::JArr(b)) => {
                let v = v.borrow();
                for i in 0..a.len().max(b.len()) {
                    Self::diff(
                        eng,
                        a.get(i).unwrap_or(&Value::Undef),
                        b.get(i).unwrap_or(&Value::Undef),
                        v.items.get(i).unwrap_or(&Value::Undef),
                        producer,
                        forced,
                        known,
                    );
                }
            }
            (Value::Map(v), Value::JObj(a), Value::JObj(b)) => {
                let names: FxHashSet<_> = a.iter().chain(b.iter()).map(|(k, _)| k).collect();
                let before = FirstInputs::new(a, names.len());
                let next = FirstInputs::new(b, names.len());
                let v = v.borrow();
                for name in names {
                    let av = before.get(name).unwrap_or(&Value::Undef);
                    let bv = next.get(name).unwrap_or(&Value::Undef);
                    Self::diff(
                        eng,
                        av,
                        bv,
                        v.entries.get(name).unwrap_or(&Value::Undef),
                        producer,
                        forced,
                        known,
                    );
                }
            }
            _ => Self::collect(eng, value, forced, &mut FxHashSet::default()),
        }
    }

    pub(crate) fn begin(
        &self,
        eng: &Engine,
        changed: &[String],
        documents: &HashMap<String, Value>,
    ) {
        #[cfg(feature = "runtime-diagnostics")]
        let mut trace = EditTrace::new(self, true);
        eng.clear_materialized();
        let mut values = FxHashMap::default();
        let mut forced = FxHashSet::default();
        let errors = eng.env.diag_len() > 0;
        self.binding_reuse.set(!errors);
        let records = eng.env.registry_snapshot();
        *self.pool.borrow_mut() = records
            .iter()
            .map(|r| (path_str(&r.borrow().path, None), r.clone()))
            .collect();
        self.live.borrow_mut().clear();
        self.retained_records.set(0);
        *self.root_values.borrow_mut() = eng.env.roots_vec().into_iter().collect();
        *self.root_reads.borrow_mut() = eng
            .env
            .root_names()
            .iter()
            .map(|n| {
                let key = format!("root:{n}");
                let deps = eng.query_reads(&key).unwrap_or_default();
                (key, deps)
            })
            .collect();
        #[cfg(feature = "runtime-diagnostics")]
        {
            let work = &mut trace.report.preparation_work;
            work.changed_roots = changed.len() as u64;
            work.previous_errors = errors;
            work.registry_records = records.len() as u64;
            work.pooled_records = self.pool.borrow().len() as u64;
            work.saved_roots = self.root_values.borrow().len() as u64;
            work.saved_root_reads = self.root_reads.borrow().len() as u64;
            trace.mark(true);
        }
        let mut known = FxHashSet::default();
        for name in changed {
            let key = format!("root:{name}");
            forced.insert(key.clone());
            known.insert(key.clone());
            let previous = self.roots.borrow().get(name).cloned();
            let value = eng.env.root(name).unwrap_or(Value::Undef);
            if let (Some(RootInput::Doc(before)), Some(next)) = (previous, documents.get(name)) {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.diff_roots += 1;
                }
                Self::diff(eng, &before, next, &value, &key, &mut forced, &mut known);
            } else {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.collected_roots += 1;
                }
                Self::collect(eng, &value, &mut forced, &mut FxHashSet::default());
            }
        }
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.preparation_work.known_producers = known.len() as u64;
            trace.report.preparation_work.forced_after_seeding[0] = forced.len() as u64;
            trace.mark(true);
        }
        // Preparation only reads the dependency graph. Borrow its thin reader
        // handles so queued readers can share the already-canonical key text.
        // Keep textual reverse keys and the original iteration/enqueue order.
        let reads = eng.reads.borrow();
        let mut readers: FxHashMap<&str, Vec<&QueryId>> = FxHashMap::default();
        for (key, deps) in reads.iter() {
            for dep in deps.iter() {
                let next = readers.entry(dep.as_str()).or_default();
                #[cfg(feature = "runtime-diagnostics")]
                let before_capacity = next.capacity();
                #[cfg(feature = "runtime-diagnostics")]
                {
                    let work = &mut trace.report.preparation_work;
                    work.read_edges += 1;
                    if next.is_empty() {
                        work.reverse_keys += 1;
                        if dep.starts_with("value:") {
                            work.value_dependency_keys += 1;
                        }
                    } else {
                        work.reverse_degree_buckets[EditTrace::degree(next.len())] -= 1;
                    }
                    for (i, prefix) in ["edge:", "snapshot:", "round:", "value:"]
                        .iter()
                        .enumerate()
                    {
                        if dep.starts_with(prefix) {
                            work.special_edges[i] += 1;
                        }
                    }
                }
                next.push(key);
                #[cfg(feature = "runtime-diagnostics")]
                {
                    let work = &mut trace.report.preparation_work;
                    work.reverse_degree_buckets[EditTrace::degree(next.len())] += 1;
                    work.reverse_max_degree = work.reverse_max_degree.max(next.len() as u64);
                    work.reverse_vector_capacity += (next.capacity() - before_capacity) as u64;
                }
                if ["edge:", "snapshot:", "round:", "value:"]
                    .iter()
                    .any(|p| dep.starts_with(p))
                {
                    forced.insert(key.as_str().to_owned());
                }
            }
        }
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.preparation_work.read_queries = reads.len() as u64;
            trace.report.preparation_work.forced_after_seeding[1] = forced.len() as u64;
            trace.mark(true);
        }
        for (name, value) in eng.env.roots_vec() {
            values.insert(format!("root:{name}"), value);
        }
        for inst in &records {
            let b = inst.borrow();
            let path = path_str(&b.path, None);
            for (name, slot) in &b.slots {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.registered_slots_scanned += 1;
                }
                let key = format!("{path}.{name}");
                if matches!(slot.state, SlotState::Ok | SlotState::Absent) {
                    #[cfg(feature = "runtime-diagnostics")]
                    {
                        trace.report.preparation_work.saved_slot_writes += 1;
                    }
                    values.insert(
                        key.clone(),
                        if slot.state == SlotState::Absent {
                            Value::Absent
                        } else {
                            slot.value.clone()
                        },
                    );
                }
                if errors {
                    forced.insert(key.clone());
                    eng.register_query_slot(&key, inst.clone(), name.clone());
                }
            }
        }
        for (index, env) in eng.const_envs.borrow().iter().enumerate() {
            for (name, con) in env.consts.borrow().iter() {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.constants_scanned += 1;
                }
                let key = format!("const:{index}|{name}");
                if con.state.get() {
                    #[cfg(feature = "runtime-diagnostics")]
                    {
                        trace.report.preparation_work.saved_constant_writes += 1;
                    }
                    values.insert(key.clone(), con.value.borrow().clone());
                }
                if errors {
                    forced.insert(key);
                }
            }
        }
        #[cfg(feature = "runtime-diagnostics")]
        {
            let work = &mut trace.report.preparation_work;
            work.saved_values = values.len() as u64;
            work.saved_values_capacity = values.capacity() as u64;
            work.forced_after_seeding[2] = forced.len() as u64;
            trace.mark(true);
        }
        if !eng.queried.borrow().is_empty() {
            for (key, value) in &values {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.retention_checks += 1;
                }
                if !self.retain(eng, value, &mut FxHashSet::default()) {
                    #[cfg(feature = "runtime-diagnostics")]
                    {
                        trace.report.preparation_work.retention_rejections += 1;
                    }
                    forced.insert(key.clone());
                }
            }
        }
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.preparation_work.forced_after_seeding[3] = forced.len() as u64;
        }
        if errors {
            forced.extend(reads.keys().map(|key| key.as_str().to_owned()));
        }
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.preparation_work.forced_after_seeding[4] = forced.len() as u64;
            trace.mark(true);
        }
        let mut invalid = FxHashSet::default();
        // Stable seed, saved-value and reader text can be borrowed until the
        // traversal ends. Generated descendant sets instead transfer their
        // Strings, preserving their owned iteration order without interning.
        let mut queue: Vec<Cow<'_, str>> = forced
            .iter()
            .map(|key| Cow::Borrowed(key.as_str()))
            .collect();
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.preparation_work.initial_queue = queue.len() as u64;
            trace.queue(&queue);
        }
        let mut walked = FxHashSet::default();
        for dep in readers.keys() {
            let Some(path) = dep.strip_prefix("value:") else {
                continue;
            };
            #[cfg(feature = "runtime-diagnostics")]
            {
                trace.report.preparation_work.value_owners_examined += 1;
            }
            let record = self.pool.borrow().get(path).cloned();
            if let Some(record) = record {
                let mut descendants = FxHashSet::default();
                Self::collect(eng, &Value::Rec(record), &mut descendants, &mut walked);
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.value_owners_found += 1;
                    trace.report.preparation_work.owner_descendant_enqueues +=
                        descendants.len() as u64;
                }
                queue.extend(descendants.into_iter().map(Cow::Owned));
            } else {
                // Unknown/frozen owners retain the conservative fallback.
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.missing_owner_fallback = true;
                    trace.report.preparation_work.fallback_enqueues = values.len() as u64;
                }
                queue.extend(values.keys().map(|key| Cow::Borrowed(key.as_str())));
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.queue(&queue);
                }
                break;
            }
            #[cfg(feature = "runtime-diagnostics")]
            {
                trace.queue(&queue);
            }
        }
        while let Some(key) = queue.pop() {
            #[cfg(feature = "runtime-diagnostics")]
            {
                trace.report.preparation_work.queue_pops += 1;
            }
            // Duplicates keep their LIFO position, but do not allocate another
            // owned string merely to discover that the key is already invalid.
            if invalid.contains(key.as_ref()) {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.duplicate_pops += 1;
                }
                continue;
            }
            invalid.insert(key.as_ref().to_owned());
            if !known.contains(key.as_ref()) {
                if let Some(value) = values.get(key.as_ref()) {
                    let mut descendants = FxHashSet::default();
                    Self::collect(eng, value, &mut descendants, &mut walked);
                    #[cfg(feature = "runtime-diagnostics")]
                    let before_len = queue.len();
                    queue.extend(
                        descendants
                            .into_iter()
                            .filter(|k| !invalid.contains(k))
                            .map(Cow::Owned),
                    );
                    #[cfg(feature = "runtime-diagnostics")]
                    {
                        trace.report.preparation_work.producer_collects += 1;
                        trace.report.preparation_work.producer_descendant_enqueues +=
                            (queue.len() - before_len) as u64;
                        trace.queue(&queue);
                    }
                }
            }
            #[cfg(feature = "runtime-diagnostics")]
            if known.contains(key.as_ref()) {
                trace.report.preparation_work.known_producers_skipped += 1;
            }
            if let Some(next) = readers.get(key.as_ref()) {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.report.preparation_work.reader_enqueues += next.len() as u64;
                }
                queue.extend(next.iter().map(|key| Cow::Borrowed(key.as_str())));
                #[cfg(feature = "runtime-diagnostics")]
                {
                    trace.queue(&queue);
                }
            }
        }
        // Release the drained queue's buffer and all borrowed text before
        // capturing Pending values or allowing dependency mutation.
        drop(queue);
        drop(readers);
        drop(reads);
        self.prepared_queries.set(invalid.len());
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.preparation_work.walked_values = walked.len() as u64;
            trace.report.preparation_work.invalid_queries = invalid.len() as u64;
            trace.mark(true);
        }
        let mut captured = FxHashMap::default();
        self.revisions
            .begin_queries(eng, &invalid, &values, &forced, |v| {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    let work = &mut trace.report.preparation_work;
                    work.pending_captures += 1;
                    let kind = match v {
                        Value::Rec(_) => 0,
                        Value::Arr(_) | Value::Map(_) => 1,
                        Value::JArr(_) | Value::JObj(_) => 2,
                        _ => 3,
                    };
                    work.pending_capture_kinds[kind] += 1;
                }
                capture(v, eng, &mut captured)
            });
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.preparation_work.capture_memo_entries = captured.len() as u64;
        }
        drop(captured);
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.revision = self.revisions.revision.get();
            trace.mark(true);
        }
        for key in &invalid {
            if let Some((inst, name)) = eng.query_slot_shared(key) {
                if let Some(slot) = inst.borrow_mut().slot_mut(&name) {
                    if slot.compute.is_some() {
                        #[cfg(feature = "runtime-diagnostics")]
                        {
                            trace.report.preparation_work.reset_slots += 1;
                        }
                        slot.state = SlotState::Unforced;
                        slot.value = Value::Undef;
                    }
                }
            }
        }
        for (index, env) in eng.const_envs.borrow().iter().enumerate() {
            for (name, con) in env.consts.borrow().iter() {
                if invalid.contains(&format!("const:{index}|{name}")) {
                    #[cfg(feature = "runtime-diagnostics")]
                    {
                        trace.report.preparation_work.reset_constants += 1;
                    }
                    con.state.set(false);
                    *con.value.borrow_mut() = Value::Undef;
                }
            }
        }
        *self.invalid.borrow_mut() = invalid;
        eng.env.roots_clear();
        eng.env.registry_clear();
        eng.env.diag_truncate(0);
        eng.failed_inputs.borrow_mut().clear();
        eng.deferred_slots.borrow_mut().clear();
        eng.deferred_roots.borrow_mut().clear();
        eng.phase.set(1);
        eng.settled.set(false);
        *eng.prev.borrow_mut() = None;
        *eng.snap.borrow_mut() = None;
        eng.queried.borrow_mut().clear();
        eng.ref_index.borrow_mut().clear();
        eng.computing_edges.borrow_mut().clear();
        eng.edge_bases.borrow_mut().clear();
        *eng.round_roots.borrow_mut() = None;
        self.active.set(true);
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.mark(true);
            trace.completed = true;
        }
    }

    pub(crate) fn root(
        &self,
        eng: &Engine,
        name: &str,
        raw: &RootSrc,
        run: impl FnOnce() -> R<Value>,
    ) -> R<Value> {
        let next = match raw {
            RootSrc::Expr(e) => RootInput::Expr((*e).clone()),
            RootSrc::Doc(v) => RootInput::Doc(v.clone()),
        };
        let previous = self.roots.borrow_mut().insert(name.into(), next.clone());
        if !self.active.get() {
            return run();
        }
        let same = match (&previous, &next) {
            (Some(RootInput::Expr(a)), RootInput::Expr(b)) => Rc::ptr_eq(a, b),
            (Some(RootInput::Doc(a)), RootInput::Doc(b)) => same_raw(a, b),
            _ => false,
        };
        let key = format!("root:{name}");
        if !same {
            self.revisions.force(eng, &key);
        }
        if !self.invalid.borrow().contains(&key) {
            let value = self.root_values.borrow().get(name).cloned();
            if let Some(value) = value {
                eng.replace_query_reads(
                    &key,
                    self.root_reads
                        .borrow()
                        .get(&key)
                        .cloned()
                        .unwrap_or_default(),
                );
                self.activate(eng, &value);
                return Ok(value);
            }
        }
        self.compute(eng, &key, run)
    }

    pub(crate) fn unchanged(&self, inst: &Inst, entries: &[(String, Value)]) -> bool {
        if !self.binding_reuse.get() {
            return false;
        }
        let inputs = self.inputs.borrow();
        let Some((weak, before)) = inputs.get(&(Rc::as_ptr(inst) as usize)) else {
            return false;
        };
        if !weak.upgrade().is_some_and(|r| Rc::ptr_eq(&r, inst)) {
            return false;
        }
        before.len() == entries.len()
            && inst
                .borrow()
                .entry_order
                .iter()
                .map(String::as_str)
                .eq(entries.iter().map(|(n, _)| n.as_str()))
            && {
                let before = FirstInputs::new(before, entries.len());
                entries
                    .iter()
                    .all(|(k, v)| before.get(k).is_some_and(|b| same_raw(b, v)))
            }
    }

    pub(crate) fn record(&self, rt: &RT, path: &[Seg], parent: Option<&Inst>) -> Option<Inst> {
        if !self.active.get() {
            return None;
        }
        let old = self.pool.borrow().get(&path_str(path, None)).cloned()?;
        let matches = {
            let b = old.borrow();
            Rc::ptr_eq(&b.rt, rt) && same_rc(&b.parent, &parent.cloned())
        };
        matches.then_some(old)
    }

    pub(crate) fn bound(
        &self,
        eng: &Engine,
        inst: &Inst,
        entries: &[(String, Value)],
        old_slots: Vec<(Rc<str>, Slot)>,
    ) {
        let id = Rc::as_ptr(inst) as usize;
        if self.active.get() {
            let mut old: FxHashMap<_, _> = old_slots.into_iter().collect();
            let inputs = self.inputs.borrow();
            let before = inputs
                .get(&id)
                .filter(|(weak, _)| weak.upgrade().is_some_and(|r| Rc::ptr_eq(&r, inst)))
                .map(|(_, v)| v);
            let names: Vec<_> = inst.borrow().slots.iter().map(|(n, _)| n.clone()).collect();
            let before = before.map(|entries| FirstInputs::new(entries, names.len()));
            let next = FirstInputs::new(entries, names.len());
            for name in names {
                let key = format!("{}.{name}", path_str(&inst.borrow().path, None));
                let a = before.as_ref().and_then(|entries| entries.get(&name));
                let n = next.get(&name);
                let same = match (a, n) {
                    (Some(a), Some(b)) => same_raw(a, b),
                    (None, None) => true,
                    _ => false,
                };
                let old_slot = old.remove(&name);
                let kind = inst.borrow().slot(&name).unwrap().kind;
                if before.is_none() || !same || old_slot.as_ref().is_none_or(|s| s.kind != kind) {
                    self.revisions.force(eng, &key);
                } else if let Some(slot) = old_slot {
                    *inst.borrow_mut().slot_mut(&name).unwrap() = slot;
                }
                eng.register_query_slot(&key, inst.clone(), name);
            }
        }
        self.inputs
            .borrow_mut()
            .insert(id, (Rc::downgrade(inst), entries.to_vec()));
        if self.active.get() {
            for (_, slot) in &inst.borrow().slots {
                if slot.state == SlotState::Ok {
                    self.activate(eng, &slot.value);
                }
            }
        }
    }

    pub(crate) fn register(&self, eng: &Engine, inst: &Inst) {
        if !self.live.borrow_mut().insert(Rc::as_ptr(inst) as usize) {
            return;
        }
        eng.env.registry_push(inst.clone());
        if self
            .pool
            .borrow()
            .get(&path_str(&inst.borrow().path, None))
            .is_some_and(|old| Rc::ptr_eq(old, inst))
        {
            self.retained_records.set(self.retained_records.get() + 1);
        }
    }

    pub(crate) fn activate(&self, eng: &Engine, value: &Value) {
        if !self.active.get() || eng.no_reg.get() > 0 {
            return;
        }
        match value {
            Value::Rec(inst) => {
                if eng.owner_of(inst).is_some()
                    || self.live.borrow().contains(&(Rc::as_ptr(inst) as usize))
                {
                    return;
                }
                self.register(eng, inst);
                for (_, s) in &inst.borrow().slots {
                    if s.state == SlotState::Ok {
                        self.activate(eng, &s.value);
                    }
                }
            }
            Value::Arr(a) => {
                for v in &a.borrow().items {
                    self.activate(eng, v);
                }
            }
            Value::Map(m) => {
                for v in m.borrow().entries.values() {
                    self.activate(eng, v);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn compute(
        &self,
        eng: &Engine,
        key: &str,
        run: impl FnOnce() -> R<Value>,
    ) -> R<Value> {
        let id = eng.query_id(key);
        self.compute_id(eng, &id, run)
    }

    pub(crate) fn compute_id(
        &self,
        eng: &Engine,
        id: &QueryId,
        run: impl FnOnce() -> R<Value>,
    ) -> R<Value> {
        let value = self.revisions.compute_id(eng, id, run)?;
        self.activate(eng, &value);
        Ok(value)
    }

    /// Number of retained record input descriptions (a diagnostic counter).
    pub fn cached_inputs(&self) -> usize {
        self.inputs.borrow().len()
    }

    pub(crate) fn prune(&self, eng: &Engine) {
        if !self.inputs.borrow().is_empty() {
            // Only addresses escape this borrow. Release both registry borrows
            // before dropping cached values: a native capture's destructor can
            // replace the public registry or roots during retain.
            let live: FxHashSet<_> = eng
                .env
                .registry
                .borrow()
                .borrow()
                .iter()
                .map(|r| Rc::as_ptr(r) as usize)
                .collect();
            self.inputs
                .borrow_mut()
                .retain(|id, (weak, _)| live.contains(id) && weak.strong_count() > 0);
        }
        self.roots
            .borrow_mut()
            .retain(|name, _| root_exists(eng, name));
    }

    pub(crate) fn finish(&self, eng: &Engine) {
        #[cfg(feature = "runtime-diagnostics")]
        let mut trace = EditTrace::new(self, false);
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.finish_work.input_entries[0] = self.inputs.borrow().len() as u64;
            trace.report.finish_work.root_entries[0] = self.roots.borrow().len() as u64;
        }
        self.prune(eng);
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.finish_work.input_entries[1] = self.inputs.borrow().len() as u64;
            trace.report.finish_work.root_entries[1] = self.roots.borrow().len() as u64;
            trace.report.finish_was_active = self.active.get();
            trace.mark(true);
        }
        if !self.active.get() {
            #[cfg(feature = "runtime-diagnostics")]
            {
                trace.completed = true;
                trace.close_on_drop = false;
            }
            return;
        }
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.finish_work.slot_entries[0] = eng.slots_by_key.borrow().len() as u64;
            trace.report.finish_work.read_entries[0] = eng.reads.borrow().len() as u64;
        }
        let live = self.live.borrow();
        eng.slots_by_key.borrow_mut().retain(|key, (inst, _)| {
            if live.contains(&(Rc::as_ptr(inst) as usize)) {
                true
            } else {
                eng.reads.borrow_mut().remove(key);
                false
            }
        });
        self.roots
            .borrow_mut()
            .retain(|name, _| root_exists(eng, name));
        // Assertions run again after settling; their previous reads have no
        // cached result to retain, just like roots removed by this edit.
        eng.reads.borrow_mut().retain(|key, _| {
            !key.starts_with("assert:")
                && key
                    .strip_prefix("root:")
                    .is_none_or(|name| root_exists(eng, name))
        });
        #[cfg(feature = "runtime-diagnostics")]
        {
            let work = &mut trace.report.finish_work;
            work.root_entries[2] = self.roots.borrow().len() as u64;
            work.slot_entries[1] = eng.slots_by_key.borrow().len() as u64;
            work.read_entries[1] = eng.reads.borrow().len() as u64;
            trace.mark(true);
            trace.report.finish_work.revision_entries[0] = self.revisions.tracked_queries() as u64;
        }
        self.revisions.finish();
        self.revisions.prune(eng);
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.report.finish_work.revision_entries[1] = self.revisions.tracked_queries() as u64;
            trace.mark(true);
        }
        self.pool.borrow_mut().clear();
        drop(live);
        self.live.borrow_mut().clear();
        self.invalid.borrow_mut().clear();
        self.root_values.borrow_mut().clear();
        self.root_reads.borrow_mut().clear();
        self.active.set(false);
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.mark(true);
        }
        eng.sweep_query_ids();
        #[cfg(feature = "runtime-diagnostics")]
        {
            trace.mark(true);
            trace.completed = true;
        }
    }
}

#[cfg(test)]
#[path = "../../tests/private/edits_prune_test.rs"]
mod prune_tests;

#[cfg(all(test, feature = "runtime-diagnostics"))]
#[path = "../../tests/private/edits_diagnostics_test.rs"]
mod diagnostics_tests;
