//! Reference-round reuse over the value layer; mirrors qengine/rounds.ts.
use crate::ast::{walk_expr_tree, walk_type_exprs, Expr};
use crate::engine::{Edge, Engine, Inst};
use crate::qengine::revisions::Revisions;
use crate::semantics::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

fn address(inst: &Inst) -> usize {
    Rc::as_ptr(inst) as usize
}
fn identity(v: &Value) -> Option<(u8, usize)> {
    match v {
        Value::Ref(p) => Some((0, Rc::as_ptr(p) as usize)),
        Value::Arr(a) => Some((1, Rc::as_ptr(a) as usize)),
        Value::Map(m) => Some((2, Rc::as_ptr(m) as usize)),
        Value::Rec(r) => Some((3, address(r))),
        _ => None,
    }
}
fn identical(a: &Value, b: &Value) -> bool {
    match (identity(a), identity(b)) {
        (Some(a), Some(b)) => a == b,
        (None, None) => value_eq(a, b),
        _ => false,
    }
}

// Compare observed shapes, leaving independently read member values to the graph.
fn same(a: &Value, b: &Value) -> bool {
    if identity(a).is_some() && identity(a) == identity(b) {
        return true;
    }
    match (a, b) {
        (Value::Rec(a), Value::Rec(b)) => {
            let (a, b) = (a.borrow(), b.borrow());
            a.type_name == b.type_name
                && Rc::ptr_eq(&a.rt, &b.rt)
                && a.path == b.path
                && a.slots.len() == b.slots.len()
                && a.slots.iter().zip(&b.slots).all(|((an, av), (bn, bv))| {
                    an == bn && av.kind == bv.kind && av.hidden == bv.hidden
                })
                && a.entry_order == b.entry_order
                && a.extras.len() == b.extras.len()
                && a.extras
                    .iter()
                    .zip(&b.extras)
                    .all(|((ak, av), (bk, bv))| ak == bk && same(av, bv))
        }
        (Value::Arr(a), Value::Arr(b)) => {
            let (a, b) = (a.borrow(), b.borrow());
            a.path == b.path
                && a.items.len() == b.items.len()
                && a.items.iter().zip(&b.items).all(|(x, y)| same(x, y))
        }
        (Value::Map(a), Value::Map(b)) => {
            let (a, b) = (a.borrow(), b.borrow());
            a.path == b.path
                && a.entries.len() == b.entries.len()
                && a.entries
                    .iter()
                    .zip(b.entries.iter())
                    .all(|((ak, av), (bk, bv))| ak == bk && same(av, bv))
        }
        (Value::Rec(_) | Value::Arr(_) | Value::Map(_), _)
        | (_, Value::Rec(_) | Value::Arr(_) | Value::Map(_)) => false,
        (Value::Absent, Value::Absent) => true,
        _ => value_eq(a, b),
    }
}

// A forwarded inverse reference or foreign record view depends on snapshot age,
// even when its canonical path has not changed. Owned scalar-built records do not.
fn snapshot_result(
    v: &Value,
    eng: &Engine,
    prefix: &str,
    seen: &mut FxHashSet<(u8, usize)>,
) -> bool {
    let Some(key) = identity(v) else {
        return false;
    };
    if !seen.insert(key) {
        return false;
    }
    match v {
        Value::Ref(_) => {
            eng.inverse_refs.borrow().contains_key(&key.1)
                || eng.snap_refs.borrow().contains_key(&key.1)
        }
        Value::Arr(a) => a
            .borrow()
            .items
            .iter()
            .any(|v| snapshot_result(v, eng, prefix, seen)),
        Value::Map(m) => m
            .borrow()
            .entries
            .iter()
            .any(|(_, v)| snapshot_result(v, eng, prefix, seen)),
        Value::Rec(r) => {
            if eng.owner_of(r).is_some() {
                return true;
            }
            let r = r.borrow();
            let path = path_str(&r.path, None);
            if !(path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('.') || tail.starts_with('[')))
            {
                return true;
            }
            r.slots
                .iter()
                .any(|(_, s)| snapshot_result(&s.value, eng, prefix, seen))
                || r.extras
                    .iter()
                    .any(|(_, v)| snapshot_result(v, eng, prefix, seen))
        }
        _ => false,
    }
}

/// Retained round state. Values are memoized by member slots, without a second
/// database copy; structural and reference reads use the same dependency graph.
#[derive(Default)]
pub struct RoundCache {
    /// Revision verification for retained member slots.
    pub revisions: Rc<Revisions>,
    /// Successfully reused round transitions.
    pub reused_rounds: Cell<usize>,
    /// Sum of retained record counts across transitions.
    pub retained_records: Cell<usize>,
    /// Member computations invalidated across transitions.
    pub invalidated_slots: Cell<usize>,
    clean: RefCell<FxHashSet<usize>>,
    rebased: RefCell<FxHashMap<(u8, usize), (Value, Value)>>,
}
impl RoundCache {
    pub(crate) fn needed(env: &Rc<Env>) -> bool {
        fn visit(env: &Rc<Env>, seen: &mut HashSet<usize>) -> bool {
            if !seen.insert(Rc::as_ptr(env) as usize) {
                return false;
            }
            let mut found = false;
            let mut mark = |e: &Rc<Expr>| {
                found |= matches!(&**e, Expr::Referrers { .. });
            };
            for t in env.type_asts.borrow().values() {
                walk_type_exprs(&t.ast, true, &mut mark);
            }
            for f in env.funcs.borrow().values() {
                walk_expr_tree(&f.body, true, &mut mark);
                if let Some(t) = &f.ret {
                    walk_type_exprs(t, true, &mut mark);
                }
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        walk_type_exprs(t, true, &mut mark);
                    }
                }
            }
            for c in env.consts.borrow().values() {
                walk_expr_tree(&c.expr, true, &mut mark);
                if let Some(t) = &c.ty {
                    walk_type_exprs(t, true, &mut mark);
                }
            }
            for (_, t, e) in env.outputs.borrow().iter() {
                walk_type_exprs(t, true, &mut mark);
                walk_expr_tree(e, true, &mut mark);
            }
            for (t, e) in env.inputs.borrow().values() {
                walk_type_exprs(t, true, &mut mark);
                if let Some(e) = e {
                    walk_expr_tree(e, true, &mut mark);
                }
            }
            found
                || env.imports.borrow().values().any(|im| visit(&im.env, seen))
                || env
                    .namespaces
                    .borrow()
                    .values()
                    .any(|(env, _)| visit(env, seen))
        }
        visit(env, &mut HashSet::new())
    }

    pub(crate) fn clean(&self, inst: &Inst) -> bool {
        self.clean.borrow().contains(&address(inst))
    }
    pub(crate) fn value(&self, v: &Value, eng: &Engine) -> Value {
        if eng.prev.borrow().is_none() {
            return v.clone();
        }
        let Some(key) = identity(v) else {
            return v.clone();
        };
        if key.0 == 3 {
            return v.clone();
        }
        if let Some((_, cached)) = self.rebased.borrow().get(&key) {
            return cached.clone();
        }
        let result = match v {
            Value::Ref(p)
                if eng
                    .round_refs
                    .borrow()
                    .contains_key(&(Rc::as_ptr(p) as usize)) =>
            {
                let next = Rc::new((**p).clone());
                if let Some(prev) = eng.prev.borrow().as_ref() {
                    eng.snap_refs
                        .borrow_mut()
                        .insert(Rc::as_ptr(&next) as usize, (next.clone(), prev.clone()));
                }
                eng.round_refs
                    .borrow_mut()
                    .insert(Rc::as_ptr(&next) as usize, next.clone());
                eng.inverse_refs
                    .borrow_mut()
                    .insert(Rc::as_ptr(&next) as usize, next.clone());
                Value::Ref(next)
            }
            Value::Arr(a) => {
                let a = a.borrow();
                let items: Vec<Value> = a.items.iter().map(|v| self.value(v, eng)).collect();
                if items.iter().zip(&a.items).all(|(a, b)| identical(a, b)) {
                    v.clone()
                } else {
                    Value::Arr(Rc::new(RefCell::new(ArrV {
                        items,
                        path: a.path.clone(),
                    })))
                }
            }
            Value::Map(m) => {
                let m = m.borrow();
                let entries: Vec<_> = m
                    .entries
                    .iter()
                    .map(|(k, v)| (k.clone(), self.value(v, eng)))
                    .collect();
                if entries
                    .iter()
                    .zip(m.entries.iter())
                    .all(|((_, a), (_, b))| identical(a, b))
                {
                    v.clone()
                } else {
                    Value::Map(Rc::new(RefCell::new(MapV {
                        entries: entries.into_iter().collect(),
                        path: m.path.clone(),
                    })))
                }
            }
            _ => v.clone(),
        };
        self.rebased
            .borrow_mut()
            .insert(key, (v.clone(), result.clone()));
        result
    }

    pub(crate) fn advance(
        &self,
        eng: &Rc<Engine>,
        edges: &HashMap<String, Edge>,
    ) -> Option<Rc<Engine>> {
        let registry = eng.env.registry_snapshot();
        if eng.env.diag_len() > 0
            || registry.iter().any(|r| {
                r.borrow()
                    .slots
                    .iter()
                    .any(|(_, s)| !matches!(s.state, SlotState::Ok | SlotState::Absent))
            })
        {
            return None;
        }
        let prior_records: FxHashMap<String, Inst> = eng
            .prev
            .borrow()
            .as_ref()
            .and_then(|p| p.frozen_registry.borrow().clone())
            .unwrap_or_default()
            .iter()
            .map(|r| (path_str(&r.borrow().path, None), r.clone()))
            .collect();
        let records: FxHashMap<String, Inst> = registry
            .iter()
            .map(|r| (path_str(&r.borrow().path, None), r.clone()))
            .collect();
        if records.len() != registry.len() {
            return None;
        }
        let invert = |all: &HashMap<String, Edge>| {
            let mut result: FxHashMap<String, FxHashMap<String, Vec<String>>> =
                FxHashMap::default();
            for (key, edge) in all {
                let mut inv: FxHashMap<String, Vec<String>> = FxHashMap::default();
                for (source, (_, refs)) in edge {
                    for target in refs
                        .iter()
                        .map(|p| path_str(p, None))
                        .collect::<FxHashSet<_>>()
                    {
                        inv.entry(target).or_default().push(source.clone());
                    }
                }
                for paths in inv.values_mut() {
                    paths.sort();
                }
                result.insert(key.clone(), inv);
            }
            result
        };
        let old_edges = eng
            .snap
            .borrow()
            .as_ref()
            .map(|s| invert(&s.edges))
            .unwrap_or_default();
        let new_edges = invert(edges);
        let mut reverse: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
        let mut pending = Vec::new();
        for (reader, deps) in eng.reads.borrow().iter() {
            for dep in deps {
                reverse
                    .entry(dep.clone())
                    .or_default()
                    .insert(reader.clone());
                if let Some(s) = dep.strip_prefix("edge:") {
                    let mut parts = s.splitn(3, '|');
                    let key = format!("{}|{}", parts.next()?, parts.next()?);
                    let target = parts.next()?;
                    let a = old_edges.get(&key).and_then(|i| i.get(target));
                    let b = new_edges.get(&key).and_then(|i| i.get(target));
                    if a.map(Vec::as_slice).unwrap_or(&[]) != b.map(Vec::as_slice).unwrap_or(&[]) {
                        pending.push(dep.clone());
                    }
                } else if dep == "round:nested" {
                    pending.push(dep.clone());
                } else if dep == "round:reference" {
                    let prefix = reader.strip_prefix("root:").unwrap_or(reader);
                    let value = eng
                        .slots_by_key
                        .borrow()
                        .get(reader)
                        .and_then(|(r, n)| r.borrow().slot(n).map(|s| s.value.clone()))
                        .or_else(|| eng.env.root(prefix));
                    if reader.starts_with("const:")
                        || value.as_ref().is_some_and(|v| {
                            snapshot_result(v, eng, prefix, &mut FxHashSet::default())
                        })
                    {
                        pending.push(reader.clone());
                    }
                } else if let Some(key) = dep.strip_prefix("snapshot:") {
                    let current = eng.slots_by_key.borrow().get(key).cloned();
                    let equal = current.is_some_and(|(r, n)| {
                        let r = r.borrow();
                        let Some(old) = prior_records.get(&path_str(&r.path, None)) else {
                            return false;
                        };
                        let old = old.borrow();
                        match (old.slot(&n), r.slot(&n)) {
                            (Some(a), Some(b)) => a.state == b.state && same(&a.value, &b.value),
                            _ => false,
                        }
                    });
                    if !equal {
                        pending.push(dep.clone());
                    }
                }
            }
        }
        let mut subtrees: FxHashMap<String, Vec<Inst>> = FxHashMap::default();
        for inst in &registry {
            let b = inst.borrow();
            for i in 1..=b.path.len() {
                subtrees
                    .entry(path_str(&b.path[..i], None))
                    .or_default()
                    .push(inst.clone());
            }
        }
        let forced = pending.iter().cloned().collect();
        let mut invalid = FxHashSet::default();
        let mut dropped = FxHashSet::default();
        let mut roots = HashSet::new();
        let mut dirty = FxHashSet::default();
        while let Some(key) = pending.pop() {
            if !invalid.insert(key.clone()) {
                continue;
            }
            if key.starts_with("const:") {
                return None;
            }
            if let Some(readers) = reverse.get(&key) {
                pending.extend(readers.iter().cloned());
            }
            let prefix = if let Some(name) = key.strip_prefix("root:") {
                roots.insert(name.to_string());
                Some(path_str(&[Seg::Name(name.into())], None))
            } else {
                eng.slots_by_key.borrow().get(&key).map(|(inst, name)| {
                    let mut current = Some(inst.clone());
                    while let Some(i) = current {
                        dirty.insert(address(&i));
                        current = i.borrow().parent.clone();
                    }
                    let mut p = inst.borrow().path.to_vec();
                    p.push(Seg::Name(name.as_str().into()));
                    path_str(&p, None)
                })
            };
            if let Some(insts) = prefix.as_ref().and_then(|p| subtrees.get(p)) {
                for inst in insts {
                    if dropped.insert(address(inst)) {
                        for (n, _) in &inst.borrow().slots {
                            pending.push(Engine::slot_key(inst, n));
                        }
                    }
                }
            }
        }
        let mut rebound = false;
        for (name, _) in eng.env.roots_vec() {
            rebound |= roots.contains(&name);
            if rebound && !roots.contains(&name) {
                return None;
            }
        }
        let snapshot = self.freeze(eng)?;
        self.revisions.begin(eng, &invalid, &forced, &dropped);
        for inst in &registry {
            if dropped.contains(&address(inst)) {
                for (n, _) in &inst.borrow().slots {
                    let key = Engine::slot_key(inst, n);
                    eng.reads.borrow_mut().remove(&key);
                    eng.slots_by_key.borrow_mut().remove(&key);
                }
            }
        }
        for key in &invalid {
            if let Some((inst, name)) = eng.slots_by_key.borrow().get(key) {
                let mut i = inst.borrow_mut();
                let s = i.slot_mut(name)?;
                if s.compute.is_some() {
                    s.state = SlotState::Unforced;
                    s.value = Value::Undef;
                    self.invalidated_slots.set(self.invalidated_slots.get() + 1);
                }
            }
            eng.reads.borrow_mut().remove(key);
        }
        eng.env.registry_retain(|r| !dropped.contains(&address(r)));
        *self.clean.borrow_mut() = registry
            .iter()
            .map(address)
            .filter(|id| !dirty.contains(id) && !dropped.contains(id))
            .collect();
        for root in &roots {
            eng.env.remove_root(root);
        }
        *eng.round_roots.borrow_mut() = Some(roots);
        eng.deferred_slots.borrow_mut().clear();
        eng.deferred_roots.borrow_mut().clear();
        eng.phase.set(1);
        *eng.snap.borrow_mut() = None;
        *eng.queried.borrow_mut() = eng
            .reads
            .borrow()
            .values()
            .flat_map(|deps| deps.iter())
            .filter_map(|dep| {
                let tail = dep.strip_prefix("edge:")?;
                let mut parts = tail.splitn(3, '|');
                Some(format!("{}|{}", parts.next()?, parts.next()?))
            })
            .collect();
        eng.ref_index.borrow_mut().clear();
        eng.computing_edges.borrow_mut().clear();
        eng.edge_bases.borrow_mut().clear();
        for reset in eng.round_resets.borrow().iter() {
            reset();
        }
        self.rebased.borrow_mut().clear();
        *eng.prev.borrow_mut() = Some(snapshot.clone());
        for (name, value) in eng.env.roots_vec() {
            eng.env.set_root(&name, self.value(&value, eng));
        }
        self.retained_records
            .set(self.retained_records.get() + registry.len() - dropped.len());
        self.reused_rounds.set(self.reused_rounds.get() + 1);
        Some(snapshot)
    }

    fn freeze(&self, eng: &Rc<Engine>) -> Option<Rc<Engine>> {
        let tagger = eng.env.tagger.borrow().clone();
        let frozen = Engine::bare(eng.env.clone());
        *eng.env.tagger.borrow_mut() = tagger;
        *frozen.programs.borrow_mut() = eng.programs.borrow().clone();
        *frozen.prev.borrow_mut() = eng.prev.borrow().clone();
        *frozen.snap_refs.borrow_mut() = eng.snap_refs.borrow().clone();
        *frozen.inverse_refs.borrow_mut() = eng.inverse_refs.borrow().clone();
        let mut copies = FxHashMap::default();
        let registry: Option<Vec<_>> = eng
            .env
            .registry_snapshot()
            .iter()
            .map(|r| {
                self.copy(&Value::Rec(r.clone()), eng, &frozen, &mut copies)
                    .and_then(|v| if let Value::Rec(r) = v { Some(r) } else { None })
            })
            .collect();
        let registry = registry?;
        let roots: Option<Vec<_>> = eng
            .env
            .roots_vec()
            .iter()
            .map(|(k, v)| Some((k.clone(), self.copy(v, eng, &frozen, &mut copies)?)))
            .collect();
        *frozen.frozen_roots.borrow_mut() = Some(roots?);
        *frozen.frozen_set.borrow_mut() = registry.iter().map(address).collect();
        // Snapshot navigation uses record slots directly. Only observed reads
        // need comparison at a later boundary; no full field-key index here.
        *frozen.frozen_registry.borrow_mut() = Some(registry);
        Some(frozen)
    }
    fn copy(
        &self,
        raw: &Value,
        eng: &Engine,
        frozen: &Rc<Engine>,
        copies: &mut FxHashMap<(u8, usize), Value>,
    ) -> Option<Value> {
        let v = self.value(raw, eng);
        if let Some(key) = identity(&v) {
            if let Some(c) = copies.get(&key) {
                return Some(c.clone());
            }
        }
        Some(match &v {
            Value::Rec(inst) => {
                if eng.owner_of(inst).is_some() {
                    return Some(v);
                }
                let b = inst.borrow();
                let r = Rc::new(RefCell::new(RecInst {
                    type_name: b.type_name.clone(),
                    rt: b.rt.clone(),
                    path: b.path.clone(),
                    ps: RefCell::new(None),
                    parent: None,
                    slots: Vec::new(),
                    entry_order: b.entry_order.clone(),
                    extras: Vec::new(),
                    menv: b.menv.clone(),
                }));
                copies.insert((3, address(inst)), Value::Rec(r.clone()));
                let parent = match &b.parent {
                    Some(p) => match self.copy(&Value::Rec(p.clone()), eng, frozen, copies)? {
                        Value::Rec(p) => Some(p),
                        _ => return None,
                    },
                    None => None,
                };
                let mut slots = Vec::new();
                for (name, s) in &b.slots {
                    if !matches!(s.state, SlotState::Ok | SlotState::Absent) {
                        return None;
                    }
                    slots.push((
                        name.clone(),
                        Slot {
                            kind: s.kind,
                            hidden: s.hidden,
                            state: s.state,
                            value: self.copy(&s.value, eng, frozen, copies)?,
                            compute: None,
                        },
                    ));
                }
                let extras: Option<Vec<_>> = b
                    .extras
                    .iter()
                    .map(|(k, v)| Some((k.clone(), self.copy(v, eng, frozen, copies)?)))
                    .collect();
                {
                    let mut r = r.borrow_mut();
                    r.parent = parent;
                    r.slots = slots;
                    r.extras = extras?;
                }
                Value::Rec(r)
            }
            Value::Arr(a) => {
                let b = a.borrow();
                let r = Rc::new(RefCell::new(ArrV {
                    items: vec![],
                    path: b.path.clone(),
                }));
                copies.insert((1, Rc::as_ptr(a) as usize), Value::Arr(r.clone()));
                let items: Option<Vec<_>> = b
                    .items
                    .iter()
                    .map(|v| self.copy(v, eng, frozen, copies))
                    .collect();
                r.borrow_mut().items = items?;
                Value::Arr(r)
            }
            Value::Map(m) => {
                let b = m.borrow();
                let r = Rc::new(RefCell::new(MapV {
                    entries: Default::default(),
                    path: b.path.clone(),
                }));
                copies.insert((2, Rc::as_ptr(m) as usize), Value::Map(r.clone()));
                let entries: Option<Vec<_>> = b
                    .entries
                    .iter()
                    .map(|(k, v)| Some((k.clone(), self.copy(v, eng, frozen, copies)?)))
                    .collect();
                r.borrow_mut().entries = entries?.into_iter().collect();
                Value::Map(r)
            }
            Value::Ref(p)
                if eng
                    .round_refs
                    .borrow()
                    .contains_key(&(Rc::as_ptr(p) as usize)) =>
            {
                // A frozen answer has its own identity: live cache rebasing must
                // never move it to the next round's owner.
                let path = Rc::new(p.as_ref().clone());
                if let Some((_, owner)) = eng.snap_refs.borrow().get(&(Rc::as_ptr(p) as usize)) {
                    frozen
                        .snap_refs
                        .borrow_mut()
                        .insert(Rc::as_ptr(&path) as usize, (path.clone(), owner.clone()));
                }
                frozen
                    .inverse_refs
                    .borrow_mut()
                    .insert(Rc::as_ptr(&path) as usize, path.clone());
                let result = Value::Ref(path);
                copies.insert((0, Rc::as_ptr(p) as usize), result.clone());
                result
            }
            Value::Ref(_)
            | Value::JObj(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Bool(_)
            | Value::Str(_)
            | Value::Null
            | Value::Absent
            | Value::Undef
            | Value::Q { .. }
            | Value::Range { .. } => v.clone(),
            _ => return None,
        })
    }
}
