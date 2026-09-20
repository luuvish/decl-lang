//! Conservative cycle collection for the Rust value layer. Ordinary Rc owners
//! keep their usual lifetimes. A sweep subtracts only edges it can enumerate;
//! opaque callbacks and caller-held values are therefore roots, never garbage.
use super::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::rc::Weak;

#[cfg(feature = "runtime-diagnostics")]
mod diagnostics;
#[cfg(feature = "runtime-diagnostics")]
pub use diagnostics::{take_gc_diagnostics, GcDiagnostics, GcTrigger};

#[derive(Default)]
struct Tracked {
    envs: Vec<Weak<Env>>,
    records: Vec<Weak<RefCell<RecInst>>>,
    types: Vec<Weak<Ty>>,
}
thread_local! {
    static TRACKED: RefCell<Tracked> = RefCell::new(Tracked::default());
    static COLLECTING: Cell<bool> = const { Cell::new(false) };
    static ENGINES: Cell<usize> = const { Cell::new(0) };
    static LEFT_TO_PROCESS: Cell<bool> = const { Cell::new(false) };
}
/// A process about to exit leaves what it built to the operating system: after
/// this, the thread's guards no longer sweep. Only a one-shot command calls it,
/// once its runtime is no longer dropped either (cli.rs).
pub(crate) fn leave_to_process() {
    let _ = LEFT_TO_PROCESS.try_with(|b| b.set(true));
}
fn left_to_process() -> bool {
    LEFT_TO_PROCESS.try_with(Cell::get).unwrap_or(false)
}
pub(super) fn track_env(e: &Rc<Env>) {
    TRACKED.with(|t| t.borrow_mut().envs.push(Rc::downgrade(e)));
}
pub(super) fn track_record(r: &Rc<RefCell<RecInst>>) {
    TRACKED.with(|t| t.borrow_mut().records.push(Rc::downgrade(r)));
}
pub(super) fn track_type(t: &RT) {
    if matches!(t.k, RTk::Rec(_) | RTk::Union(_)) {
        TRACKED.with(|s| s.borrow_mut().types.push(Rc::downgrade(t)));
    }
}

/// Drop after all fields of the last Engine, including frozen engines.
pub(crate) struct EngineGuard;
impl EngineGuard {
    pub(crate) fn new() -> Self {
        ENGINES.with(|n| n.set(n.get() + 1));
        Self
    }
}
impl Drop for EngineGuard {
    fn drop(&mut self) {
        let last = ENGINES
            .try_with(|n| {
                n.set(n.get() - 1);
                n.get() == 0
            })
            .unwrap_or(false);
        if last && !left_to_process() {
            #[cfg(feature = "runtime-diagnostics")]
            diagnostics::set_next_trigger(diagnostics::GcTrigger::LastEngine);
            collect_cycles();
        }
    }
}
/// An outer command drops its module handles after its Engine.
pub(crate) struct CommandGuard;
impl Drop for CommandGuard {
    fn drop(&mut self) {
        if left_to_process() {
            return;
        }
        #[cfg(feature = "runtime-diagnostics")]
        diagnostics::set_next_trigger(diagnostics::GcTrigger::Command);
        collect_cycles();
    }
}

#[derive(Clone)]
enum Node {
    Env(Rc<Env>),
    Rec(Rc<RefCell<RecInst>>),
    Type(RT),
    Members(Rc<Vec<Member>>),
    Types(Rc<Vec<RT>>),
    Registry(Rc<RefCell<Vec<Rc<RefCell<RecInst>>>>>),
    Roots(Rc<RefCell<Vec<(String, Value)>>>),
    Arr(Rc<RefCell<ArrV>>),
    Map(Rc<RefCell<MapV>>),
    Object(Rc<Vec<(String, Value)>>),
    PreArray(Rc<Vec<(bool, Value)>>),
    JsonArray(Rc<Vec<Value>>),
    PreValue(Rc<PreValV>),
    Closure(Rc<Closure>),
    Local(Rc<LocalFrame>),
    Constant(Rc<ConstEntry>),
    Namespace(Rc<NsRefV>),
    Exports(Rc<RefCell<HashMap<String, Export>>>),
    Compute(Weak<Compute>),
}
impl Node {
    fn key_count(&self) -> ((u8, usize), usize) {
        macro_rules! key {
            ($tag:expr, $rc:expr) => {
                (($tag, Rc::as_ptr($rc) as usize), Rc::strong_count($rc))
            };
        }
        match self {
            Self::Env(x) => key!(0, x),
            Self::Rec(x) => key!(1, x),
            Self::Type(x) => key!(2, x),
            Self::Members(x) => key!(3, x),
            Self::Types(x) => key!(4, x),
            Self::Registry(x) => key!(5, x),
            Self::Roots(x) => key!(6, x),
            Self::Arr(x) => key!(7, x),
            Self::Map(x) => key!(8, x),
            Self::Object(x) => key!(9, x),
            Self::PreArray(x) => key!(10, x),
            Self::JsonArray(x) => key!(11, x),
            Self::PreValue(x) => key!(12, x),
            Self::Closure(x) => key!(13, x),
            Self::Local(x) => key!(14, x),
            Self::Constant(x) => key!(15, x),
            Self::Namespace(x) => key!(16, x),
            Self::Exports(x) => key!(17, x),
            Self::Compute(x) => ((18, x.as_ptr() as usize), x.strong_count()),
        }
    }
    // Descriptor slots are already held by Graph::Rec. Keeping another strong
    // descriptor owner here would postpone opaque capture destructors until
    // Graph drop instead of the original record-clear phase.
    fn graph_strong_owners(&self) -> usize {
        usize::from(!matches!(self, Self::Compute(_)))
    }
    // A failed borrow makes this node an external root. It also leaves its
    // unenumerated outgoing references in the children's external counts.
    fn trace(&self, out: &mut impl FnMut(Node)) -> Option<()> {
        match self {
            Self::Env(e) => {
                for t in e.type_memo.try_borrow().ok()?.values() {
                    out(Self::Type(t.clone()));
                }
                for t in e.cyclic.try_borrow().ok()?.values() {
                    out(Self::Type(t.clone()));
                }
                for c in e.consts.try_borrow().ok()?.values() {
                    out(Self::Constant(c.clone()));
                }
                out(Self::Registry(e.registry.try_borrow().ok()?.clone()));
                out(Self::Roots(e.roots.try_borrow().ok()?.clone()));
                for x in e.imports.try_borrow().ok()?.values() {
                    out(Self::Env(x.env.clone()));
                }
                for (env, exports) in e.namespaces.try_borrow().ok()?.values() {
                    out(Self::Env(env.clone()));
                    out(Self::Exports(exports.clone()));
                }
            }
            Self::Rec(r) => {
                let r = r.try_borrow().ok()?;
                out(Self::Type(r.rt.clone()));
                if let Some(p) = &r.parent {
                    out(Self::Rec(p.clone()));
                }
                if let Some(e) = &r.menv {
                    out(Self::Env(e.clone()));
                }
                for (_, s) in &r.slots {
                    value(&s.value, out);
                    if let Some(c) = &s.compute {
                        // One edge per slot handle, then trace the shared
                        // descriptor's captured edges once at its own node.
                        out(Self::Compute(Rc::downgrade(c)));
                    }
                }
                for (_, v) in &r.extras {
                    value(v, out);
                }
            }
            Self::Type(t) => match &t.k {
                RTk::Lit(v) => value(v, out),
                RTk::Range { lo, hi, .. } => {
                    value(lo, out);
                    value(hi, out);
                }
                RTk::Arr { elem, .. } => out(Self::Type(elem.clone())),
                RTk::Map { key, val } => {
                    out(Self::Type(key.clone()));
                    out(Self::Type(val.clone()));
                }
                RTk::Union(arms) => {
                    for t in arms.try_borrow().ok()?.iter() {
                        out(Self::Type(t.clone()));
                    }
                }
                RTk::IsectN(arms) => {
                    for t in arms {
                        out(Self::Type(t.clone()));
                    }
                }
                RTk::Rec(r) => {
                    out(Self::Members(r.members.try_borrow().ok()?.clone()));
                    for a in r.asserts.try_borrow().ok()?.iter() {
                        if let Some(e) = &a.menv {
                            out(Self::Env(e.clone()));
                        }
                    }
                    for (_, t) in r.ctx_decls.try_borrow().ok()?.iter() {
                        out(Self::Type(t.clone()));
                    }
                    for (a, b) in r.pending.try_borrow().ok()?.iter() {
                        out(Self::Type(a.clone()));
                        out(Self::Type(b.clone()));
                    }
                }
                RTk::Pred { base, .. } | RTk::Ref(base) => out(Self::Type(base.clone())),
                RTk::Func { params, ret } => {
                    for t in params {
                        out(Self::Type(t.clone()));
                    }
                    out(Self::Type(ret.clone()));
                }
                _ => {}
            },
            Self::Members(ms) => {
                for m in ms.iter() {
                    if let Some(t) = &m.ty {
                        out(Self::Type(t.clone()));
                    }
                    if let Some(ts) = &m.conj {
                        for t in ts {
                            out(Self::Type(t.clone()));
                        }
                    }
                    if let Some(e) = &m.menv {
                        out(Self::Env(e.clone()));
                    }
                }
            }
            Self::Types(ts) => {
                for t in ts.iter() {
                    out(Self::Type(t.clone()));
                }
            }
            Self::Registry(rs) => {
                for r in rs.try_borrow().ok()?.iter() {
                    out(Self::Rec(r.clone()));
                }
            }
            Self::Roots(vs) => {
                for (_, v) in vs.try_borrow().ok()?.iter() {
                    value(v, out);
                }
            }
            Self::Arr(a) => {
                for v in &a.try_borrow().ok()?.items {
                    value(v, out);
                }
            }
            Self::Map(m) => {
                for v in m.try_borrow().ok()?.entries.values() {
                    value(v, out);
                }
            }
            Self::Object(vs) => {
                for (_, v) in vs.iter() {
                    value(v, out);
                }
            }
            Self::PreArray(vs) => {
                for (_, v) in vs.iter() {
                    value(v, out);
                }
            }
            Self::JsonArray(vs) => {
                for v in vs.iter() {
                    value(v, out);
                }
            }
            Self::PreValue(v) => scope(&v.scope, out),
            Self::Closure(v) => scope(&v.scope, out),
            Self::Local(l) => {
                value(&l.value, out);
                if let Some(n) = &l.next {
                    out(Self::Local(n.clone()));
                }
            }
            Self::Constant(c) => value(&*c.value.try_borrow().ok()?, out),
            Self::Namespace(n) => out(Self::Exports(n.exports.clone())),
            Self::Exports(es) => {
                for e in es.try_borrow().ok()?.values() {
                    out(Self::Env(e.env.clone()));
                }
            }
            Self::Compute(c) => {
                let captured = c.upgrade()?;
                compute(&captured, out);
            }
        }
        Some(())
    }
    // Only unreachable mutable nodes are cut. Immutable and opaque nodes keep
    // their contents until Rc drops them normally after these cycles break.
    fn clear(&self) {
        match self {
            Self::Env(e) => {
                e.type_memo.borrow_mut().clear();
                e.cyclic.borrow_mut().clear();
                e.consts.borrow_mut().clear();
                e.imports.borrow_mut().clear();
                e.namespaces.borrow_mut().clear();
            }
            Self::Rec(r) => {
                let mut r = r.borrow_mut();
                r.parent = None;
                r.menv = None;
                r.slots.clear();
                r.extras.clear();
            }
            Self::Type(t) => match &t.k {
                RTk::Rec(r) => {
                    *r.members.borrow_mut() = Rc::new(vec![]);
                    r.asserts.borrow_mut().clear();
                    r.ctx_decls.borrow_mut().clear();
                    r.pending.borrow_mut().clear();
                }
                RTk::Union(ts) => ts.borrow_mut().clear(),
                _ => {}
            },
            Self::Registry(rs) => rs.borrow_mut().clear(),
            Self::Roots(vs) => vs.borrow_mut().clear(),
            Self::Arr(a) => a.borrow_mut().items.clear(),
            Self::Map(m) => m.borrow_mut().entries.clear(),
            Self::Constant(c) => *c.value.borrow_mut() = Value::Undef,
            Self::Exports(es) => es.borrow_mut().clear(),
            _ => {}
        }
    }
}
fn value(v: &Value, out: &mut impl FnMut(Node)) {
    match v {
        Value::Rec(r) => out(Node::Rec(r.clone())),
        Value::Arr(a) => out(Node::Arr(a.clone())),
        Value::Map(m) => out(Node::Map(m.clone())),
        Value::Clo(c) => out(Node::Closure(c.clone())),
        Value::NsRef(n) => out(Node::Namespace(n.clone())),
        Value::PreObj(vs) | Value::JObj(vs) => out(Node::Object(vs.clone())),
        Value::PreArr(vs) => out(Node::PreArray(vs.clone())),
        Value::JArr(vs) => out(Node::JsonArray(vs.clone())),
        Value::PreVal(p) => out(Node::PreValue(p.clone())),
        Value::Range(range) => {
            value(&range.lo, out);
            value(&range.hi, out);
        }
        _ => {}
    }
}
fn scope(s: &Scope, out: &mut impl FnMut(Node)) {
    if let Some(i) = &s.inst {
        out(Node::Rec(i.clone()));
    }
    if let Some(e) = &s.menv {
        out(Node::Env(e.clone()));
    }
    if let Some(l) = &s.locals.0 {
        out(Node::Local(l.clone()));
    }
}
fn compute(c: &Compute, out: &mut impl FnMut(Node)) {
    match c {
        Compute::Check {
            raw, types, menv, ..
        } => {
            value(raw, out);
            out(Node::Types(types.clone()));
            if let Some(e) = menv {
                out(Node::Env(e.clone()));
            }
        }
        Compute::Default { types, menv, .. } => {
            out(Node::Types(types.clone()));
            if let Some(e) = menv {
                out(Node::Env(e.clone()));
            }
        }
        Compute::Derived {
            ty, supplied, menv, ..
        } => {
            if let Some(t) = ty {
                out(Node::Type(t.clone()));
            }
            if let Some(v) = supplied {
                value(v, out);
            }
            if let Some(e) = menv {
                out(Node::Env(e.clone()));
            }
        }
        Compute::Bridge(_) => {}
    }
}
#[derive(Default)]
struct Graph {
    nodes: Vec<Node>,
    // Each Node keeps its allocation reserved with Rc or Weak until the pass
    // releases its nodes. An address cannot be reused during this pass. Node
    // variants have distinct concrete pointee types; aliases with the same
    // type (PreObj/JObj) must normalize to one variant in value(). The kind
    // tag is needed for dispatch and diagnostics, but not allocation identity.
    ids: FxHashMap<usize, usize>,
    edges: Vec<usize>,
    edge_offsets: Vec<usize>,
    incoming: Vec<usize>,
    borrowed: FxHashSet<usize>,
    #[cfg(feature = "runtime-diagnostics")]
    work: diagnostics::Work,
}
impl Graph {
    // Only empty storage crosses a pass boundary. Drop every Rc/Weak before
    // seeding again: an old graph owner would change the next root counts, and
    // an old address could identify an unrelated allocation after destruction.
    fn reset(&mut self) {
        self.nodes.clear();
        self.ids.clear();
        self.edges.clear();
        self.edge_offsets.clear();
        self.incoming.clear();
        self.borrowed.clear();
        #[cfg(feature = "runtime-diagnostics")]
        {
            self.work = diagnostics::Work::default();
        }
    }

    fn add(&mut self, n: Node) -> usize {
        let key = n.key_count().0;
        #[cfg(feature = "runtime-diagnostics")]
        {
            self.work.add_attempts += 1;
        }
        if let Some(i) = self.ids.get(&key.1) {
            #[cfg(feature = "runtime-diagnostics")]
            {
                self.work.duplicate_node_hits += 1;
            }
            return *i;
        }
        #[cfg(feature = "runtime-diagnostics")]
        let before = [
            self.nodes.capacity(),
            self.ids.capacity(),
            self.edges.capacity(),
            self.incoming.capacity(),
        ];
        let i = self.nodes.len();
        self.ids.insert(key.1, i);
        self.nodes.push(n);
        self.incoming.push(0);
        #[cfg(feature = "runtime-diagnostics")]
        {
            self.work.node_kinds[key.0 as usize] += 1;
            if let Node::Compute(weak) = &self.nodes[i] {
                // The graph retains descriptors weakly. This temporary inspection
                // neither persists an owner nor changes the graph's edge counts.
                let work = &mut self.work.compute;
                work.body_size_bytes = std::mem::size_of::<Compute>();
                match weak.upgrade().as_deref() {
                    Some(Compute::Check { raw, .. }) => {
                        work.check += 1;
                        work.check_raw.record(raw);
                    }
                    Some(Compute::Default { .. }) => work.default += 1,
                    Some(Compute::Derived { supplied, .. }) => {
                        work.derived += 1;
                        work.derived_supplied += usize::from(supplied.is_some());
                    }
                    Some(Compute::Bridge(_)) => work.bridge += 1,
                    None => work.expired += 1,
                }
            }
            let after = [
                self.nodes.capacity(),
                self.ids.capacity(),
                self.edges.capacity(),
                self.incoming.capacity(),
            ];
            for (slot, b, a) in [0, 1, 2, 4]
                .into_iter()
                .zip(before)
                .zip(after)
                .map(|((s, b), a)| (s, b, a))
            {
                self.work.capacity_growth_events[slot] += usize::from(a > b);
            }
        }
        i
    }
    fn collect(
        &mut self,
        #[cfg(feature = "runtime-diagnostics")] pass: &mut diagnostics::Pass,
    ) -> usize {
        let mut i = 0;
        while i < self.nodes.len() {
            #[cfg(feature = "runtime-diagnostics")]
            let offset_cap = self.edge_offsets.capacity();
            self.edge_offsets.push(self.edges.len());
            #[cfg(feature = "runtime-diagnostics")]
            {
                self.work.capacity_growth_events[3] +=
                    usize::from(self.edge_offsets.capacity() > offset_cap);
            }
            let node = self.nodes[i].clone();
            if node
                .trace(&mut |n| {
                    let j = self.add(n);
                    #[cfg(feature = "runtime-diagnostics")]
                    let before = self.edges.capacity();
                    self.edges.push(j);
                    #[cfg(feature = "runtime-diagnostics")]
                    {
                        self.work.capacity_growth_events[2] +=
                            usize::from(self.edges.capacity() > before);
                        let degree = self.edges.len() - self.edge_offsets[i];
                        self.work.nonempty_adjacencies += usize::from(degree == 1);
                        self.work.maximum_out_degree = self.work.maximum_out_degree.max(degree);
                        self.work.strong_edge_occurrences += 1;
                    }
                    self.incoming[j] += 1;
                })
                .is_none()
            {
                #[cfg(feature = "runtime-diagnostics")]
                let before = self.borrowed.capacity();
                self.borrowed.insert(i);
                #[cfg(feature = "runtime-diagnostics")]
                {
                    self.work.borrowed_nodes += 1;
                    self.work.capacity_growth_events[5] +=
                        usize::from(self.borrowed.capacity() > before);
                }
            }
            #[cfg(feature = "runtime-diagnostics")]
            {
                self.work.traced_nodes += 1;
            }
            i += 1;
        }
        #[cfg(feature = "runtime-diagnostics")]
        let offset_cap = self.edge_offsets.capacity();
        self.edge_offsets.push(self.edges.len());
        #[cfg(feature = "runtime-diagnostics")]
        {
            self.work.capacity_growth_events[3] +=
                usize::from(self.edge_offsets.capacity() > offset_cap);
        }
        #[cfg(feature = "runtime-diagnostics")]
        pass.end(1);
        let mut live = vec![false; self.nodes.len()];
        let mut queue = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            // All ordinary nodes have one Graph strong owner; Compute nodes
            // are weak and have none. Unenumerated strong owners are external.
            let external = n.key_count().1 > self.incoming[i] + n.graph_strong_owners();
            if external || self.borrowed.contains(&i) {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    self.work.roots += 1;
                    self.work.external_roots += usize::from(external);
                    self.work.borrow_only_roots += usize::from(!external);
                }
                #[cfg(feature = "runtime-diagnostics")]
                let before = queue.capacity();
                queue.push(i);
                #[cfg(feature = "runtime-diagnostics")]
                {
                    self.work
                        .queue_added(1, queue.len(), before, queue.capacity());
                }
            }
        }
        #[cfg(feature = "runtime-diagnostics")]
        pass.end(2);
        while let Some(i) = queue.pop() {
            #[cfg(feature = "runtime-diagnostics")]
            {
                self.work.queue_pops += 1;
            }
            if live[i] {
                #[cfg(feature = "runtime-diagnostics")]
                {
                    self.work.duplicate_queue_pops += 1;
                }
                continue;
            }
            live[i] = true;
            #[cfg(feature = "runtime-diagnostics")]
            {
                self.work.live_nodes += 1;
            }
            #[cfg(feature = "runtime-diagnostics")]
            let before = queue.capacity();
            queue.extend(
                self.edges[self.edge_offsets[i]..self.edge_offsets[i + 1]]
                    .iter()
                    .copied(),
            );
            #[cfg(feature = "runtime-diagnostics")]
            {
                self.work.mark_edge_examinations +=
                    self.edges[self.edge_offsets[i]..self.edge_offsets[i + 1]].len();
                self.work.queue_added(
                    self.edges[self.edge_offsets[i]..self.edge_offsets[i + 1]].len(),
                    queue.len(),
                    before,
                    queue.capacity(),
                );
            }
        }
        #[cfg(feature = "runtime-diagnostics")]
        pass.end(3);
        let mut cleared = 0;
        for (i, n) in self.nodes.iter().enumerate() {
            if !live[i] {
                n.clear();
                cleared += 1;
            }
        }
        #[cfg(feature = "runtime-diagnostics")]
        {
            self.work.visited_garbage_nodes = cleared;
            pass.work = self.work;
            pass.capacities = diagnostics::Capacities {
                nodes: self.nodes.capacity(),
                ids: self.ids.capacity(),
                edges: self.edges.capacity(),
                edge_offsets: self.edge_offsets.capacity(),
                incoming: self.incoming.capacity(),
                borrowed: self.borrowed.capacity(),
                nested_edge_elements: self.work.nested_edge_capacity,
                live: live.capacity(),
                queue: queue.capacity(),
                vector_payload_capacity_bytes: self.nodes.capacity() * std::mem::size_of::<Node>()
                    + self.edges.capacity() * std::mem::size_of::<usize>()
                    + (self.edge_offsets.capacity()
                        + self.incoming.capacity()
                        + self.work.nested_edge_capacity
                        + queue.capacity())
                        * std::mem::size_of::<usize>()
                    + live.capacity() * std::mem::size_of::<bool>(),
                node_size_bytes: std::mem::size_of::<Node>(),
                edge_element_size_bytes: std::mem::size_of::<usize>(),
            };
            pass.end(4);
        }
        // Keep these pass-local, including their drop before Node owners.
        // collect_cycles resets or drops the graph after this method returns.
        drop(queue);
        drop(live);
        cleared
    }
}

/// Reclaim inaccessible Rust value/type cycles on this thread. Live Engines,
/// returned values, types, scopes, imported modules and frozen snapshots remain
/// roots. Opaque native callbacks are treated conservatively. The last Engine
/// and CLI command sweep automatically; callers holding bare values beyond an
/// Engine may also sweep after releasing those values. Returns visited garbage
/// nodes (not bytes). No evaluation or user callback is executed by tracing.
pub fn collect_cycles() -> usize {
    #[cfg(feature = "runtime-diagnostics")]
    let trigger = diagnostics::take_trigger();
    if COLLECTING.try_with(|b| b.replace(true)).unwrap_or(true) {
        #[cfg(feature = "runtime-diagnostics")]
        diagnostics::suppressed(trigger);
        return 0;
    }
    #[cfg(feature = "runtime-diagnostics")]
    let mut observation = diagnostics::Call::new(trigger);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            let _ = COLLECTING.try_with(|b| b.set(false));
        }
    }
    let _reset = Reset;
    let mut total = 0;
    // Reuse capacity only while this collection is active; never keep a large
    // graph buffer in TLS or across last-Engine/outer-command boundaries.
    let mut graph = Graph::default();
    loop {
        #[cfg(feature = "runtime-diagnostics")]
        let mut pass = diagnostics::Pass::new(observation.passes.len());
        let ok = TRACKED
            .try_with(|t| {
                let mut t = t.borrow_mut();
                #[cfg(feature = "runtime-diagnostics")]
                {
                    pass.tracked_tls_available = true;
                    pass.tracked_weak_before = [t.envs.len(), t.records.len(), t.types.len()];
                    pass.tracked_weak_capacity_before =
                        [t.envs.capacity(), t.records.capacity(), t.types.capacity()];
                }
                t.envs.retain(|w| {
                    if let Some(e) = w.upgrade() {
                        graph.add(Node::Env(e));
                        true
                    } else {
                        false
                    }
                });
                t.records.retain(|w| {
                    if let Some(r) = w.upgrade() {
                        graph.add(Node::Rec(r));
                        true
                    } else {
                        false
                    }
                });
                t.types.retain(|w| {
                    if let Some(t) = w.upgrade() {
                        graph.add(Node::Type(t));
                        true
                    } else {
                        false
                    }
                });
                #[cfg(feature = "runtime-diagnostics")]
                {
                    pass.tracked_weak_after = [t.envs.len(), t.records.len(), t.types.len()];
                    pass.tracked_weak_capacity_after =
                        [t.envs.capacity(), t.records.capacity(), t.types.capacity()];
                }
            })
            .is_ok();
        #[cfg(feature = "runtime-diagnostics")]
        {
            graph.work.seed_nodes = graph.nodes.len();
            pass.end(0);
        }
        if !ok {
            drop(graph);
            #[cfg(feature = "runtime-diagnostics")]
            {
                pass.end(5);
                observation.termination = "tracked_tls_unavailable";
                observation.passes.push(pass);
            }
            break;
        }
        #[cfg(feature = "runtime-diagnostics")]
        let count = graph.collect(&mut pass);
        #[cfg(not(feature = "runtime-diagnostics"))]
        let count = graph.collect();
        total += count;
        if count == 0 {
            // Include all terminal storage release in the last pass and call.
            drop(graph);
            #[cfg(feature = "runtime-diagnostics")]
            {
                pass.end(5);
                observation.passes.push(pass);
                observation.termination = "zero_garbage_pass";
            }
            break;
        }
        graph.reset();
        #[cfg(feature = "runtime-diagnostics")]
        {
            // The existing scratch_and_graph_drop phase now releases every
            // runtime owner but retains empty graph capacity on nonterminal
            // passes. Terminal passes still release all scratch. Its endpoint
            // is not an owner-only live-byte metric; capacity growth may be 0
            // even when the next pass traces the same number of nodes/edges.
            pass.end(5);
            observation.passes.push(pass);
        }
    }
    #[cfg(feature = "runtime-diagnostics")]
    diagnostics::finish(observation, total);
    total
}

#[cfg(test)]
#[path = "../../tests/private/lifetime_test.rs"]
mod lifetime_tests;
