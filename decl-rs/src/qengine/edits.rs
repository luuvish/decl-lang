//! Replay Session root binding over retained records and member revisions.
use crate::engine::{Engine, Inst, RootSrc};
use crate::qengine::revisions::{comparable, Matches, Revisions};
use crate::semantics::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

fn same_rc<T>(a: &Option<Rc<T>>, b: &Option<Rc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
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
        | (Value::Q { .. }, Value::Q { .. }) => value_eq(a, b),
        _ => false,
    }
}

fn capture(value: &Value, eng: &Engine) -> Matches {
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
                .map(|(k, v)| (k.clone(), capture(v, eng)))
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
            let items: Vec<_> = a.items.iter().map(|v| capture(v, eng)).collect();
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
                .map(|(k, v)| (k.clone(), capture(v, eng)))
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
    invalid: RefCell<FxHashSet<String>>,
    root_values: RefCell<FxHashMap<String, Value>>,
    root_reads: RefCell<FxHashMap<String, HashSet<String>>>,
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
    fn resolve(eng: &Engine, key: &str) -> R<bool> {
        let slot = eng.slots_by_key.borrow().get(key).cloned();
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

    fn collect(eng: &Engine, value: &Value, into: &mut FxHashSet<String>) {
        match value {
            Value::Rec(inst) => {
                let b = inst.borrow();
                for (name, slot) in &b.slots {
                    let key = format!("{}.{name}", path_str(&b.path, None));
                    eng.slots_by_key
                        .borrow_mut()
                        .insert(key.clone(), (inst.clone(), name.clone()));
                    into.insert(key);
                    if slot.state == SlotState::Ok {
                        Self::collect(eng, &slot.value, into);
                    }
                }
            }
            Value::Arr(a) => {
                for v in &a.borrow().items {
                    Self::collect(eng, v, into);
                }
            }
            Value::Map(m) => {
                for v in m.borrow().entries.values() {
                    Self::collect(eng, v, into);
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
                for name in names {
                    let av = a.iter().find(|(k, _)| k == name).map(|(_, v)| v);
                    let bv = b.iter().find(|(k, _)| k == name).map(|(_, v)| v);
                    if av.is_some() == bv.is_some()
                        && same_raw(av.unwrap_or(&Value::Undef), bv.unwrap_or(&Value::Undef))
                    {
                        continue;
                    }
                    let bound = inst.borrow();
                    if let Some(slot) = bound.slot(name) {
                        let key = format!("{}.{name}", path_str(&bound.path, None));
                        eng.slots_by_key
                            .borrow_mut()
                            .insert(key.clone(), (inst.clone(), name.clone()));
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
                let v = v.borrow();
                for name in names {
                    let av = a
                        .iter()
                        .find(|(k, _)| k == name)
                        .map(|(_, v)| v)
                        .unwrap_or(&Value::Undef);
                    let bv = b
                        .iter()
                        .find(|(k, _)| k == name)
                        .map(|(_, v)| v)
                        .unwrap_or(&Value::Undef);
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
            _ => Self::collect(eng, value, forced),
        }
    }

    pub(crate) fn begin(
        &self,
        eng: &Engine,
        changed: &[String],
        documents: &HashMap<String, Value>,
    ) {
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
                let deps = eng.reads.borrow().get(&key).cloned().unwrap_or_default();
                (key, deps)
            })
            .collect();
        let mut known = FxHashSet::default();
        for name in changed {
            let key = format!("root:{name}");
            forced.insert(key.clone());
            known.insert(key.clone());
            let previous = self.roots.borrow().get(name).cloned();
            let value = eng.env.root(name).unwrap_or(Value::Undef);
            if let (Some(RootInput::Doc(before)), Some(next)) = (previous, documents.get(name)) {
                Self::diff(eng, &before, next, &value, &key, &mut forced, &mut known);
            } else {
                Self::collect(eng, &value, &mut forced);
            }
        }
        let mut readers: FxHashMap<String, Vec<String>> = FxHashMap::default();
        let mut aggregate = false;
        for (key, deps) in eng.reads.borrow().iter() {
            for dep in deps {
                readers.entry(dep.clone()).or_default().push(key.clone());
                if ["edge:", "snapshot:", "round:", "value:"]
                    .iter()
                    .any(|p| dep.starts_with(p))
                {
                    forced.insert(key.clone());
                }
                aggregate |= dep.starts_with("value:");
            }
        }
        for (name, value) in eng.env.roots_vec() {
            values.insert(format!("root:{name}"), value);
        }
        for inst in &records {
            let b = inst.borrow();
            for (name, slot) in &b.slots {
                let key = format!("{}.{name}", path_str(&b.path, None));
                if matches!(slot.state, SlotState::Ok | SlotState::Absent) {
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
                    eng.slots_by_key
                        .borrow_mut()
                        .insert(key, (inst.clone(), name.clone()));
                }
            }
        }
        for (index, env) in eng.const_envs.borrow().iter().enumerate() {
            for (name, con) in env.consts.borrow().iter() {
                let key = format!("const:{index}|{name}");
                if con.state.get() {
                    values.insert(key.clone(), con.value.borrow().clone());
                }
                if errors {
                    forced.insert(key);
                }
            }
        }
        if !eng.queried.borrow().is_empty() {
            for (key, value) in &values {
                if !self.retain(eng, value, &mut FxHashSet::default()) {
                    forced.insert(key.clone());
                }
            }
        }
        if errors {
            forced.extend(eng.reads.borrow().keys().cloned());
        }
        let mut invalid = FxHashSet::default();
        let mut queue: Vec<_> = forced.iter().cloned().collect();
        if aggregate {
            queue.extend(values.keys().cloned());
        }
        while let Some(key) = queue.pop() {
            if !invalid.insert(key.clone()) {
                continue;
            }
            if !known.contains(&key) {
                if let Some(value) = values.get(&key) {
                    let mut descendants = FxHashSet::default();
                    Self::collect(eng, value, &mut descendants);
                    queue.extend(descendants.into_iter().filter(|k| !invalid.contains(k)));
                }
            }
            if let Some(next) = readers.get(&key) {
                queue.extend(next.iter().cloned());
            }
        }
        self.prepared_queries.set(invalid.len());
        self.revisions
            .begin_queries(eng, &invalid, &values, &forced, |v| capture(v, eng));
        for key in &invalid {
            if let Some((inst, name)) = eng.slots_by_key.borrow().get(key) {
                if let Some(slot) = inst.borrow_mut().slot_mut(name) {
                    if slot.compute.is_some() {
                        slot.state = SlotState::Unforced;
                        slot.value = Value::Undef;
                    }
                }
            }
        }
        for (index, env) in eng.const_envs.borrow().iter().enumerate() {
            for (name, con) in env.consts.borrow().iter() {
                if invalid.contains(&format!("const:{index}|{name}")) {
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
            self.revisions.force(&key);
        }
        if !self.invalid.borrow().contains(&key) {
            let value = self.root_values.borrow().get(name).cloned();
            if let Some(value) = value {
                eng.reads.borrow_mut().insert(
                    key.clone(),
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
            && entries.iter().all(|(k, v)| {
                before
                    .iter()
                    .find(|(n, _)| n == k)
                    .is_some_and(|(_, b)| same_raw(b, v))
            })
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
        old_slots: Vec<(String, Slot)>,
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
            for name in names {
                let key = format!("{}.{name}", path_str(&inst.borrow().path, None));
                let a = before
                    .and_then(|v| v.iter().find(|(n, _)| *n == name))
                    .map(|(_, v)| v);
                let n = entries.iter().find(|(n, _)| *n == name).map(|(_, v)| v);
                let same = match (a, n) {
                    (Some(a), Some(b)) => same_raw(a, b),
                    (None, None) => true,
                    _ => false,
                };
                let old_slot = old.remove(&name);
                let kind = inst.borrow().slot(&name).unwrap().kind;
                if before.is_none() || !same || old_slot.as_ref().is_none_or(|s| s.kind != kind) {
                    self.revisions.force(&key);
                } else if let Some(slot) = old_slot {
                    *inst.borrow_mut().slot_mut(&name).unwrap() = slot;
                }
                eng.slots_by_key
                    .borrow_mut()
                    .insert(key, (inst.clone(), name));
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
        let value = self.revisions.compute(eng, key, run)?;
        self.activate(eng, &value);
        Ok(value)
    }

    /// Number of retained record input descriptions (a diagnostic counter).
    pub fn cached_inputs(&self) -> usize {
        self.inputs.borrow().len()
    }

    pub(crate) fn prune(&self, eng: &Engine) {
        let live: FxHashSet<_> = eng
            .env
            .registry_snapshot()
            .iter()
            .map(|r| Rc::as_ptr(r) as usize)
            .collect();
        self.inputs
            .borrow_mut()
            .retain(|id, (weak, _)| live.contains(id) && weak.strong_count() > 0);
        self.roots
            .borrow_mut()
            .retain(|name, _| eng.env.root(name).is_some());
    }

    pub(crate) fn finish(&self, eng: &Engine) {
        self.prune(eng);
        if !self.active.get() {
            return;
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
            .retain(|name, _| eng.env.root(name).is_some());
        // Assertions run again after settling; their previous reads have no
        // cached result to retain, just like roots removed by this edit.
        eng.reads.borrow_mut().retain(|key, _| {
            !key.starts_with("assert:")
                && key
                    .strip_prefix("root:")
                    .is_none_or(|name| eng.env.root(name).is_some())
        });
        self.revisions.finish();
        self.revisions.prune(eng);
        self.pool.borrow_mut().clear();
        drop(live);
        self.live.borrow_mut().clear();
        self.invalid.borrow_mut().clear();
        self.root_values.borrow_mut().clear();
        self.root_reads.borrow_mut().clear();
        self.active.set(false);
    }
}
