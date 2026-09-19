//! Scalar-only cycle-collector observations. Never retains a runtime owner.
use serde::Serialize;
use std::cell::{Cell, RefCell};

pub const PHASE_NAMES: [&str; 6] = [
    "seed",
    "trace",
    "root",
    "mark",
    "clear",
    "scratch_and_graph_drop",
];
pub const NODE_KIND_NAMES: [&str; 19] = [
    "env",
    "rec",
    "type",
    "members",
    "types",
    "registry",
    "roots",
    "arr",
    "map",
    "object",
    "pre_array",
    "json_array",
    "pre_value",
    "closure",
    "local",
    "constant",
    "namespace",
    "exports",
    "compute",
];
pub const GROWTH_NAMES: [&str; 8] = [
    "nodes",
    "ids",
    "edges",
    "edge_offsets",
    "incoming",
    "borrowed",
    "nested_edges",
    "queue",
];

/// Entry path that requested a cycle collection.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GcTrigger {
    /// An explicit call to the public collection API.
    #[default]
    Explicit,
    /// Release of the last registered Engine owner on this thread.
    LastEngine,
    /// Release of the command lifetime guard.
    Command,
}
impl GcTrigger {
    fn index(self) -> usize {
        match self {
            Self::Explicit => 0,
            Self::LastEngine => 1,
            Self::Command => 2,
        }
    }
}

/// CLOCK_MONOTONIC brackets CLOCK_PROCESS_CPUTIME_ID. Clock errors are explicit;
/// a failed clock cannot silently become a zero-duration successful interval.
#[derive(Clone, Copy, Default, Serialize)]
pub struct Stamp {
    pub monotonic_before_ns: Option<u64>,
    pub process_cpu_ns: Option<u64>,
    pub monotonic_after_ns: Option<u64>,
    pub errno: [i32; 3],
    pub allocation: crate::allocation_diagnostics::Snapshot,
}
fn clock(id: libc::clockid_t) -> (Option<u64>, i32) {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid writable timespec. The two callers use supported
    // POSIX clock identifiers and do not transfer ownership to the C function.
    if unsafe { libc::clock_gettime(id, &mut ts) } != 0 {
        return (
            None,
            std::io::Error::last_os_error().raw_os_error().unwrap_or(-1),
        );
    }
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        return (None, libc::EINVAL);
    }
    let ns = (ts.tv_sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|s| s.checked_add(ts.tv_nsec as u64));
    (ns, if ns.is_some() { 0 } else { libc::EOVERFLOW })
}
pub(super) fn stamp() -> Stamp {
    let (monotonic_before_ns, a) = clock(libc::CLOCK_MONOTONIC);
    let (process_cpu_ns, b) = clock(libc::CLOCK_PROCESS_CPUTIME_ID);
    let (monotonic_after_ns, c) = clock(libc::CLOCK_MONOTONIC);
    Stamp {
        monotonic_before_ns,
        process_cpu_ns,
        monotonic_after_ns,
        errno: [a, b, c],
        allocation: crate::allocation_diagnostics::snapshot(),
    }
}

/// Raw value variants of unique Check descriptors inspected in one graph pass.
/// This is a population census, not a count of equal values or cache hits.
/// No raw value is evaluated, cloned, retained, or compared with another value.
#[derive(Clone, Copy, Default, Serialize)]
pub struct CheckRawWork {
    pub small_int: usize,
    pub big_int: usize,
    pub float: usize,
    pub string: usize,
    pub boolean: usize,
    pub null: usize,
    pub absent: usize,
    pub undefined: usize,
    pub quantity: usize,
    pub reference: usize,
    pub record: usize,
    pub array: usize,
    pub map: usize,
    pub range: usize,
    pub closure: usize,
    pub native: usize,
    pub std_path: usize,
    pub namespace: usize,
    pub pattern: usize,
    pub pre_object: usize,
    pub pre_array: usize,
    pub pre_value: usize,
    pub json_object: usize,
    pub json_array: usize,
    pub segments: usize,
}
impl CheckRawWork {
    pub(super) fn record(&mut self, raw: &super::Value) {
        use super::{Num, Value};
        let count = match raw {
            Value::Int(Num::Small(_)) => &mut self.small_int,
            Value::Int(Num::Big(_)) => &mut self.big_int,
            Value::Float(_) => &mut self.float,
            Value::Str(_) => &mut self.string,
            Value::Bool(_) => &mut self.boolean,
            Value::Null => &mut self.null,
            Value::Absent => &mut self.absent,
            Value::Undef => &mut self.undefined,
            Value::Q(_) => &mut self.quantity,
            Value::Ref(_) => &mut self.reference,
            Value::Rec(_) => &mut self.record,
            Value::Arr(_) => &mut self.array,
            Value::Map(_) => &mut self.map,
            Value::Range(_) => &mut self.range,
            Value::Clo(_) => &mut self.closure,
            Value::Nat(_) => &mut self.native,
            Value::Std(_) => &mut self.std_path,
            Value::NsRef(_) => &mut self.namespace,
            Value::Pat(_) => &mut self.pattern,
            Value::PreObj(_) => &mut self.pre_object,
            Value::PreArr(_) => &mut self.pre_array,
            Value::PreVal(_) => &mut self.pre_value,
            Value::JObj(_) => &mut self.json_object,
            Value::JArr(_) => &mut self.json_array,
            Value::Segs(_) => &mut self.segments,
        };
        *count += 1;
    }
}

#[derive(Clone, Copy, Default, Serialize)]
pub struct ComputeWork {
    /// Unique descriptor nodes in this pass, never slot-handle occurrences.
    pub check: usize,
    /// Disjoint subsets whose sum equals `check`; expired nodes are excluded.
    pub check_raw: CheckRawWork,
    pub default: usize,
    pub derived: usize,
    /// Subset of derived descriptors with an explicitly supplied value.
    pub derived_supplied: usize,
    pub bridge: usize,
    /// Weak descriptor nodes that could no longer be inspected.
    pub expired: usize,
    /// Inline descriptor body only; excludes Rc headers and captured allocations.
    pub body_size_bytes: usize,
}

#[derive(Clone, Copy, Default, Serialize)]
pub struct Work {
    pub add_attempts: usize,
    pub duplicate_node_hits: usize,
    pub node_kinds: [usize; 19],
    /// Optional representation attribution; not an additional set of graph nodes.
    pub compute: ComputeWork,
    pub seed_nodes: usize,
    pub traced_nodes: usize,
    pub strong_edge_occurrences: usize,
    pub borrowed_nodes: usize,
    pub roots: usize,
    pub external_roots: usize,
    pub borrow_only_roots: usize,
    pub queue_pushes: usize,
    pub queue_pops: usize,
    pub duplicate_queue_pops: usize,
    pub queue_high_water: usize,
    pub mark_edge_examinations: usize,
    pub live_nodes: usize,
    pub visited_garbage_nodes: usize,
    pub nonempty_adjacencies: usize,
    pub maximum_out_degree: usize,
    pub nested_edge_capacity: usize,
    pub capacity_growth_events: [usize; 8],
}
impl Work {
    pub(super) fn queue_added(&mut self, added: usize, len: usize, before: usize, after: usize) {
        self.queue_pushes += added;
        self.queue_high_water = self.queue_high_water.max(len);
        self.capacity_growth_events[7] += usize::from(after > before);
    }
}

/// All capacities are element counts. Hash-map/set capacity is usable entries,
/// deliberately not estimated allocator bytes. Vec payload bytes exclude their
/// inline headers, allocator metadata, and all runtime objects being traced.
#[derive(Clone, Copy, Default, Serialize)]
pub struct Capacities {
    pub nodes: usize,
    pub ids: usize,
    pub edges: usize,
    pub edge_offsets: usize,
    pub incoming: usize,
    pub borrowed: usize,
    pub nested_edge_elements: usize,
    pub live: usize,
    pub queue: usize,
    pub vector_payload_capacity_bytes: usize,
    pub node_size_bytes: usize,
    pub edge_element_size_bytes: usize,
}

#[derive(Serialize)]
pub struct Pass {
    pub ordinal: usize,
    pub start: Stamp,
    pub phase_ends: [Option<Stamp>; 6],
    pub tracked_weak_before: [usize; 3],
    pub tracked_weak_after: [usize; 3],
    pub tracked_weak_capacity_before: [usize; 3],
    pub tracked_weak_capacity_after: [usize; 3],
    pub work: Work,
    pub capacities: Capacities,
    pub tracked_tls_available: bool,
}
impl Pass {
    pub(super) fn new(ordinal: usize) -> Self {
        Self {
            ordinal,
            start: stamp(),
            phase_ends: [None; 6],
            tracked_weak_before: [0; 3],
            tracked_weak_after: [0; 3],
            tracked_weak_capacity_before: [0; 3],
            tracked_weak_capacity_after: [0; 3],
            work: Work::default(),
            capacities: Capacities::default(),
            tracked_tls_available: false,
        }
    }
    pub(super) fn end(&mut self, phase: usize) {
        self.phase_ends[phase] = Some(stamp());
    }
}

#[derive(Serialize)]
pub struct Call {
    pub trigger: GcTrigger,
    pub start: Stamp,
    pub end: Option<Stamp>,
    pub passes: Vec<Pass>,
    pub visited_garbage_nodes: usize,
    pub termination: &'static str,
}
impl Call {
    pub(super) fn new(trigger: GcTrigger) -> Self {
        Self {
            trigger,
            start: stamp(),
            end: None,
            passes: Vec::new(),
            visited_garbage_nodes: 0,
            termination: "not_finished",
        }
    }
}

/// Thread-local scalar observations drained after runtime owners are released.
#[derive(Serialize)]
pub struct GcDiagnostics {
    /// Report schema; version 2 includes the shared Compute descriptor node kind.
    pub schema_version: u32,
    /// Ordered names of the six collection intervals.
    pub phase_names: [&'static str; 6],
    /// Node-tag names used to index each pass's per-kind counts.
    pub node_kind_names: [&'static str; 19],
    /// Container names used to index observed capacity-growth events.
    pub growth_names: [&'static str; 8],
    /// Trigger names used to index suppressed collection attempts.
    pub trigger_names: [&'static str; 3],
    /// Completed collection calls, including their passes and termination reason.
    pub calls: Vec<Call>,
    /// Reentrant or unavailable collection entries, counted by trigger.
    pub suppressed_attempts: [usize; 3],
}
impl Default for GcDiagnostics {
    fn default() -> Self {
        Self {
            schema_version: 2,
            phase_names: PHASE_NAMES,
            node_kind_names: NODE_KIND_NAMES,
            growth_names: GROWTH_NAMES,
            trigger_names: ["explicit", "last_engine", "command"],
            calls: Vec::new(),
            suppressed_attempts: [0; 3],
        }
    }
}
thread_local! {
    static REPORT: RefCell<GcDiagnostics> = RefCell::new(GcDiagnostics::default());
    static NEXT_TRIGGER: Cell<GcTrigger> = const { Cell::new(GcTrigger::Explicit) };
}
pub(super) fn set_next_trigger(trigger: GcTrigger) {
    let _ = NEXT_TRIGGER.try_with(|v| v.set(trigger));
}
pub(super) fn take_trigger() -> GcTrigger {
    NEXT_TRIGGER
        .try_with(|v| v.replace(GcTrigger::Explicit))
        .unwrap_or_default()
}
pub(super) fn suppressed(trigger: GcTrigger) {
    let _ = REPORT.try_with(|v| v.borrow_mut().suppressed_attempts[trigger.index()] += 1);
}
pub(super) fn finish(mut call: Call, visited: usize) {
    call.visited_garbage_nodes = visited;
    call.end = Some(stamp());
    // No report borrow spans tracing, clearing, Graph drop, or user owners' Drop.
    let _ = REPORT.try_with(|v| v.borrow_mut().calls.push(call));
}

/// Drain scalar observations on this thread. Call after all measured owners and
/// guards have dropped; draining earlier excludes their later collection calls.
/// The returned Vec owns no Engine, Value, Type, Node, or weak runtime handle.
pub fn take_gc_diagnostics() -> GcDiagnostics {
    REPORT
        .try_with(|v| std::mem::take(&mut *v.borrow_mut()))
        .unwrap_or_default()
}
