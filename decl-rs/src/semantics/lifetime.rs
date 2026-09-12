//! Conservative cycle collection for the Rust value layer. Ordinary Rc owners
//! keep their usual lifetimes. A sweep subtracts only edges it can enumerate;
//! opaque callbacks and caller-held values are therefore roots, never garbage.
use super::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::rc::Weak;

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
        if last {
            collect_cycles();
        }
    }
}
/// An outer command drops its module handles after its Engine.
pub(crate) struct CommandGuard;
impl Drop for CommandGuard {
    fn drop(&mut self) {
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
        }
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
                        compute(c, out);
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
        Value::Range { lo, hi, .. } => {
            value(lo, out);
            value(hi, out);
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
    ids: FxHashMap<(u8, usize), usize>,
    edges: Vec<Vec<usize>>,
    incoming: Vec<usize>,
    borrowed: FxHashSet<usize>,
}
impl Graph {
    fn add(&mut self, n: Node) -> usize {
        let key = n.key_count().0;
        if let Some(i) = self.ids.get(&key) {
            return *i;
        }
        let i = self.nodes.len();
        self.ids.insert(key, i);
        self.nodes.push(n);
        self.edges.push(vec![]);
        self.incoming.push(0);
        i
    }
    fn collect(mut self) -> usize {
        let mut i = 0;
        while i < self.nodes.len() {
            let node = self.nodes[i].clone();
            if node
                .trace(&mut |n| {
                    let j = self.add(n);
                    self.edges[i].push(j);
                    self.incoming[j] += 1;
                })
                .is_none()
            {
                self.borrowed.insert(i);
            }
            i += 1;
        }
        let mut live = vec![false; self.nodes.len()];
        let mut queue = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            // Exactly one strong reference belongs to this Graph. Every other
            // reference not enumerated above belongs to an external owner.
            if n.key_count().1 > self.incoming[i] + 1 || self.borrowed.contains(&i) {
                queue.push(i);
            }
        }
        while let Some(i) = queue.pop() {
            if live[i] {
                continue;
            }
            live[i] = true;
            queue.extend(self.edges[i].iter().copied());
        }
        let mut cleared = 0;
        for (i, n) in self.nodes.iter().enumerate() {
            if !live[i] {
                n.clear();
                cleared += 1;
            }
        }
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
    if COLLECTING.try_with(|b| b.replace(true)).unwrap_or(true) {
        return 0;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            let _ = COLLECTING.try_with(|b| b.set(false));
        }
    }
    let _reset = Reset;
    let mut total = 0;
    loop {
        let mut graph = Graph::default();
        let ok = TRACKED
            .try_with(|t| {
                let mut t = t.borrow_mut();
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
            })
            .is_ok();
        if !ok {
            break;
        }
        let count = graph.collect();
        total += count;
        if count == 0 {
            break;
        }
    }
    total
}
