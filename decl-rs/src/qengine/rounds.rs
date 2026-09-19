//! Reference-round reuse over the value layer; mirrors qengine/rounds.ts.
use crate::ast::{walk_expr_tree, walk_type_exprs, Expr};
use crate::engine::{Edge, Engine, Inst};
use crate::qengine::revisions::Revisions;
use crate::semantics::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

fn address(inst: &Inst) -> usize {
    Rc::as_ptr(inst) as usize
}

type InvertedEdges<'a> = FxHashMap<&'a str, FxHashMap<String, Vec<&'a String>>>;

// Edge maps stay immutable throughout classification. Only canonical target
// spellings need owned storage; source paths and edge keys already have owners.
fn invert_edges(all: &HashMap<String, Edge>) -> InvertedEdges<'_> {
    let mut result = FxHashMap::default();
    for (key, edge) in all {
        let mut inv: FxHashMap<String, Vec<&String>> = FxHashMap::default();
        for (source, (_, refs)) in edge {
            for target in refs
                .iter()
                .map(|p| path_str(p, None))
                .collect::<FxHashSet<_>>()
            {
                inv.entry(target).or_default().push(source);
            }
        }
        for paths in inv.values_mut() {
            paths.sort();
        }
        result.insert(key.as_str(), inv);
    }
    result
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

// Freeze memo values have one fixed variant per source kind. Store their thin
// owners directly instead of repeating a kind tag and a full Value per entry.
// This scratch owns copied bodies only; it never changes the source graph.
#[derive(Default)]
struct CopyMemo {
    records: FxHashMap<usize, Inst>,
    arrays: FxHashMap<usize, Rc<RefCell<ArrV>>>,
    maps: FxHashMap<usize, Rc<RefCell<MapV>>>,
    refs: FxHashMap<usize, Rc<SegPath>>,
}

impl CopyMemo {
    fn get(&self, value: &Value) -> Option<Value> {
        match value {
            Value::Rec(record) => self.records.get(&address(record)).cloned().map(Value::Rec),
            Value::Arr(array) => self
                .arrays
                .get(&(Rc::as_ptr(array) as usize))
                .cloned()
                .map(Value::Arr),
            Value::Map(map) => self
                .maps
                .get(&(Rc::as_ptr(map) as usize))
                .cloned()
                .map(Value::Map),
            Value::Ref(path) => self
                .refs
                .get(&(Rc::as_ptr(path) as usize))
                .cloned()
                .map(Value::Ref),
            _ => None,
        }
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
    /// Once evaluation has cleared prev, value() bypasses this epoch's memo.
    /// A later evaluation installs a new RoundCache. Release both old value
    /// owners and table capacity, retaining the clean and revision state.
    pub(crate) fn release_rebased(&self) {
        let rebased = std::mem::take(&mut *self.rebased.borrow_mut());
        // Cached values can contain native captures whose destructors reenter
        // evaluation. No memo borrow may survive into their destruction.
        drop(rebased);
    }
    fn rebase_child<'a>(&self, v: &'a Value, eng: &Engine) -> Cow<'a, Value> {
        match v {
            Value::Ref(_) | Value::Arr(_) | Value::Map(_) => Cow::Owned(self.value(v, eng)),
            // These variants pass through value unchanged. Keep the input's
            // owner until a replacement container actually needs a clone.
            _ => Cow::Borrowed(v),
        }
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
                let mut replacement: Option<Vec<Value>> = None;
                for (index, original) in a.items.iter().enumerate() {
                    let next = self.rebase_child(original, eng);
                    if replacement.is_none() && !identical(&next, original) {
                        let mut items = Vec::with_capacity(a.items.len());
                        items.extend_from_slice(&a.items[..index]);
                        replacement = Some(items);
                    }
                    if let Some(items) = &mut replacement {
                        items.push(next.into_owned());
                    }
                }
                if let Some(items) = replacement {
                    #[cfg(feature = "runtime-diagnostics")]
                    crate::retention_diagnostics::array(
                        crate::retention_diagnostics::ArraySite::Rebased,
                        items.len(),
                        items.capacity(),
                    );
                    Value::Arr(Rc::new(RefCell::new(ArrV {
                        items,
                        path: a.path.clone(),
                    })))
                } else {
                    v.clone()
                }
            }
            Value::Map(m) => {
                let m = m.borrow();
                if m.entries.is_shared() {
                    let mut replacement: Option<Vec<Value>> = None;
                    for (index, original) in m.entries.values().enumerate() {
                        let next = self.rebase_child(original, eng);
                        if replacement.is_none() && !identical(&next, original) {
                            let mut values = Vec::with_capacity(m.entries.len());
                            values.extend(m.entries.values().take(index).cloned());
                            replacement = Some(values);
                        }
                        if let Some(values) = &mut replacement {
                            values.push(next.into_owned());
                        }
                    }
                    if let Some(values) = replacement {
                        Value::Map(Rc::new(RefCell::new(MapV {
                            entries: m.entries.with_values(values),
                            path: m.path.clone(),
                        })))
                    } else {
                        v.clone()
                    }
                } else {
                    let mut replacement: Option<OrderedMap<Value>> = None;
                    for (index, (key, original)) in m.entries.iter().enumerate() {
                        let next = self.rebase_child(original, eng);
                        if replacement.is_none() && !identical(&next, original) {
                            let mut entries = OrderedMap::with_capacity_and_hasher(
                                m.entries.len(),
                                Default::default(),
                            );
                            entries.extend(
                                m.entries
                                    .iter()
                                    .take(index)
                                    .map(|(k, v)| (k.clone(), v.clone())),
                            );
                            replacement = Some(entries);
                        }
                        if let Some(entries) = &mut replacement {
                            entries.insert(key.clone(), next.into_owned());
                        }
                    }
                    if let Some(entries) = replacement {
                        Value::Map(Rc::new(RefCell::new(MapV {
                            entries: entries.into(),
                            path: m.path.clone(),
                        })))
                    } else {
                        v.clone()
                    }
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
        // Declared first so the terminal boundary includes destruction of the
        // existing registry and invalidation scratch on success or fallback.
        // The ordinal names the next reused transition, not a unique attempt.
        #[cfg(feature = "runtime-diagnostics")]
        let mut advance_diagnostic = crate::evaluation_diagnostics::AdvanceSpan::new(
            self.reused_rounds.get().saturating_add(1),
        );
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
        // Each prefix is a slice of its record's one canonical spelling. Store
        // segment boundaries while formatting; parsing delimiters afterwards
        // would be wrong for quoted keys and native bare root names.
        let registry_paths: Vec<_> = registry
            .iter()
            .map(|r| path_str_prefixes(&r.borrow().path))
            .collect();
        let records: FxHashSet<&str> = registry_paths
            .iter()
            .map(|(path, _)| path.as_str())
            .collect();
        if records.len() != registry.len() {
            return None;
        }
        drop(records);
        let snap = eng.snap.borrow();
        let old_edges = snap
            .as_ref()
            .map(|s| invert_edges(&s.edges))
            .unwrap_or_default();
        let new_edges = invert_edges(edges);
        // Classification and invalidation only inspect the dependency graph.
        // Its key text can serve the temporary reverse index without copies.
        let reads = eng.reads.borrow();
        let mut reverse: FxHashMap<&str, FxHashSet<&str>> = FxHashMap::default();
        let mut pending: Vec<Cow<'_, str>> = Vec::new();
        for (reader, deps) in reads.iter() {
            for dep in deps.iter() {
                reverse
                    .entry(dep.as_str())
                    .or_default()
                    .insert(reader.as_str());
                if let Some(s) = dep.strip_prefix("edge:") {
                    let mut parts = s.splitn(3, '|');
                    let key = format!("{}|{}", parts.next()?, parts.next()?);
                    let target = parts.next()?;
                    let a = old_edges.get(key.as_str()).and_then(|i| i.get(target));
                    let b = new_edges.get(key.as_str()).and_then(|i| i.get(target));
                    if a.map(Vec::as_slice).unwrap_or(&[]) != b.map(Vec::as_slice).unwrap_or(&[]) {
                        pending.push(Cow::Borrowed(dep.as_str()));
                    }
                } else if dep.as_str() == "round:nested" {
                    pending.push(Cow::Borrowed(dep.as_str()));
                } else if dep.as_str() == "round:reference" {
                    let prefix = reader.strip_prefix("root:").unwrap_or(reader.as_str());
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
                        pending.push(Cow::Borrowed(reader.as_str()));
                    }
                } else if let Some(key) = dep.strip_prefix("snapshot:") {
                    let current = eng.query_slot_shared(key);
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
                        pending.push(Cow::Borrowed(dep.as_str()));
                    }
                }
            }
        }
        // Classification is complete; these indices must not overlap the
        // descendant index and the copied frozen value layer below.
        drop(prior_records);
        drop(old_edges);
        drop(new_edges);
        drop(snap);
        let mut subtrees: FxHashMap<&str, Vec<Inst>> = FxHashMap::default();
        for (inst, (path, ends)) in registry.iter().zip(&registry_paths) {
            for &end in ends {
                subtrees.entry(&path[..end]).or_default().push(inst.clone());
            }
        }
        let forced = pending.iter().map(|key| key.as_ref().to_owned()).collect();
        let mut invalid = FxHashSet::default();
        let mut dropped = FxHashSet::default();
        let mut roots = HashSet::new();
        let mut dirty = FxHashSet::default();
        while let Some(key) = pending.pop() {
            // Duplicate insertions can still grow the set before lookup. Keep
            // that behavior: its later iteration determines reset/Drop order.
            if !invalid.insert(key.as_ref().to_owned()) {
                continue;
            }
            if key.starts_with("const:") {
                return None;
            }
            if let Some(readers) = reverse.get(key.as_ref()) {
                pending.extend(readers.iter().map(|key| Cow::Borrowed(*key)));
            }
            let prefix = if let Some(name) = key.strip_prefix("root:") {
                roots.insert(name.to_string());
                Some(path_str(&[Seg::Name(name.into())], None))
            } else {
                eng.query_slot_shared(&key).map(|(inst, name)| {
                    let mut current = Some(inst.clone());
                    while let Some(i) = current {
                        dirty.insert(address(&i));
                        current = i.borrow().parent.clone();
                    }
                    let member = Seg::Name(name);
                    path_str_iter(
                        inst.borrow().path.iter().chain(std::iter::once(&member)),
                        None,
                    )
                })
            };
            if let Some(insts) = prefix.as_ref().and_then(|p| subtrees.get(p.as_str())) {
                for inst in insts {
                    if dropped.insert(address(inst)) {
                        for (n, _) in &inst.borrow().slots {
                            pending.push(Cow::Owned(Engine::slot_key(inst, n)));
                        }
                    }
                }
            }
        }
        // Keep the registry owner, but release classification scratch before
        // freezing or replacing any dependencies for the next round.
        drop(subtrees);
        drop(registry_paths);
        drop(pending);
        drop(reverse);
        drop(reads);
        let mut rebound = false;
        for (name, _) in eng.env.roots_vec() {
            rebound |= roots.contains(&name);
            if rebound && !roots.contains(&name) {
                return None;
            }
        }
        #[cfg(feature = "runtime-diagnostics")]
        advance_diagnostic.classified();
        let snapshot = self.freeze(eng, &registry)?;
        #[cfg(feature = "runtime-diagnostics")]
        advance_diagnostic.frozen();
        self.revisions.begin(eng, &invalid, &forced, &dropped);
        for inst in &registry {
            if dropped.contains(&address(inst)) {
                for (n, _) in &inst.borrow().slots {
                    let key = Engine::slot_key(inst, n);
                    eng.remove_query_reads(&key);
                    eng.remove_query_slot(&key);
                }
            }
        }
        for key in &invalid {
            if let Some((inst, name)) = eng.query_slot_shared(key) {
                let mut i = inst.borrow_mut();
                let s = i.slot_mut(&name)?;
                if s.compute.is_some() {
                    s.state = SlotState::Unforced;
                    s.value = Value::Undef;
                    self.invalidated_slots.set(self.invalidated_slots.get() + 1);
                }
            }
            eng.remove_query_reads(key);
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
        #[cfg(feature = "runtime-diagnostics")]
        advance_diagnostic.accepted();
        Some(snapshot)
    }

    fn freeze(&self, eng: &Rc<Engine>, registry: &[Inst]) -> Option<Rc<Engine>> {
        let tagger = eng.env.tagger.borrow().clone();
        let frozen = Engine::bare_with_queries(eng.env.clone(), eng.query_pool());
        *eng.env.tagger.borrow_mut() = tagger;
        *frozen.programs.borrow_mut() = eng.programs.borrow().clone();
        *frozen.prev.borrow_mut() = eng.prev.borrow().clone();
        *frozen.snap_refs.borrow_mut() = eng.snap_refs.borrow().clone();
        *frozen.inverse_refs.borrow_mut() = eng.inverse_refs.borrow().clone();
        let mut copies = CopyMemo::default();
        let registry: Option<Vec<_>> = registry
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
        copies: &mut CopyMemo,
    ) -> Option<Value> {
        let v = self.value(raw, eng);
        if let Some(c) = copies.get(&v) {
            return Some(c);
        }
        Some(match &v {
            Value::Rec(inst) => {
                if eng.owner_of(inst).is_some() {
                    return Some(v);
                }
                let b = inst.borrow();
                let r = record_instance(RecInst {
                    type_name: b.type_name.clone(),
                    rt: b.rt.clone(),
                    path: b.path.clone(),
                    ps: RefCell::new(None),
                    parent: None,
                    slots: Vec::new(),
                    entry_order: b.entry_order.clone(),
                    extras: Vec::new(),
                    menv: b.menv.clone(),
                });
                copies.records.insert(address(inst), r.clone());
                let parent = match &b.parent {
                    Some(p) => match self.copy(&Value::Rec(p.clone()), eng, frozen, copies)? {
                        Value::Rec(p) => Some(p),
                        _ => return None,
                    },
                    None => None,
                };
                let mut slots = Vec::with_capacity(b.slots.len());
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
                #[cfg(feature = "runtime-diagnostics")]
                let diagnostic_array = crate::retention_diagnostics::ArrayAttempt::start(
                    crate::retention_diagnostics::ArraySite::SnapshotCopy,
                    Some(b.items.len()),
                );
                let r = Rc::new(RefCell::new(ArrV {
                    items: vec![],
                    path: b.path.clone(),
                }));
                copies.arrays.insert(Rc::as_ptr(a) as usize, r.clone());
                let mut items = Vec::with_capacity(b.items.len());
                for value in &b.items {
                    items.push(self.copy(value, eng, frozen, copies)?);
                }
                r.borrow_mut().items = items;
                #[cfg(feature = "runtime-diagnostics")]
                {
                    let a = r.borrow();
                    diagnostic_array.finish(a.items.len(), a.items.capacity());
                }
                Value::Arr(r)
            }
            Value::Map(m) => {
                let b = m.borrow();
                let r = Rc::new(RefCell::new(MapV {
                    entries: Default::default(),
                    path: b.path.clone(),
                }));
                copies.maps.insert(Rc::as_ptr(m) as usize, r.clone());
                if b.entries.is_shared() {
                    let mut values = Vec::with_capacity(b.entries.len());
                    for value in b.entries.values() {
                        values.push(self.copy(value, eng, frozen, copies)?);
                    }
                    r.borrow_mut().entries = b.entries.with_values(values);
                } else {
                    let entries: Option<Vec<_>> = b
                        .entries
                        .iter()
                        .map(|(k, v)| Some((k.clone(), self.copy(v, eng, frozen, copies)?)))
                        .collect();
                    r.borrow_mut().entries = entries?.into_iter().collect();
                }
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
                copies.refs.insert(Rc::as_ptr(p) as usize, path.clone());
                Value::Ref(path)
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
            | Value::Q(_)
            | Value::Range(_) => v.clone(),
            _ => return None,
        })
    }
}

// Rust-native snapshot, callback and lifetime boundaries. Language behavior
// remains covered by the shared reference-round corpus.
#[cfg(test)]
#[path = "../../tests/private/round_advance_test.rs"]
mod tests;
