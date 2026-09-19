//! Revision verification over the Engine's existing slot cache. Previous values
//! live here only while a slot awaits verification or recomputation.
use crate::engine::Engine;
use crate::qengine::graph::{QueryId, ReadSet};
use crate::semantics::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
struct Pending {
    value: Value,
    deps: ReadSet,
    changed: usize,
    forced: Cell<bool>,
    matches: Option<Matches>,
}

pub(crate) type Matches = Rc<dyn Fn(&Value, &Engine) -> bool>;
type Resolve = fn(&Engine, &str) -> R<bool>;

pub(crate) fn comparable(v: &Value, eng: &Engine) -> bool {
    match v {
        Value::Null
        | Value::Absent
        | Value::Int(_)
        | Value::Float(_)
        | Value::Str(_)
        | Value::Bool(_)
        | Value::Q(_)
        | Value::Range(_) => true,
        Value::Ref(p) => {
            let id = Rc::as_ptr(p) as usize;
            !eng.snap_refs.borrow().contains_key(&id)
                && !eng.inverse_refs.borrow().contains_key(&id)
                && !eng.round_refs.borrow().contains_key(&id)
        }
        Value::Arr(a) => a.borrow().items.iter().all(|v| comparable(v, eng)),
        Value::Map(m) => m.borrow().entries.iter().all(|(_, v)| comparable(v, eng)),
        _ => false,
    }
}

fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Absent, Value::Absent) => true,
        (Value::Arr(a), Value::Arr(b)) => {
            let (a, b) = (a.borrow(), b.borrow());
            a.path == b.path
                && a.items.len() == b.items.len()
                && a.items.iter().zip(&b.items).all(|(a, b)| equal(a, b))
        }
        (Value::Map(a), Value::Map(b)) => {
            let (a, b) = (a.borrow(), b.borrow());
            a.path == b.path
                && a.entries.len() == b.entries.len()
                && a.entries
                    .iter()
                    .zip(b.entries.iter())
                    .all(|((ak, av), (bk, bv))| ak == bk && equal(av, bv))
        }
        _ => value_eq(a, b),
    }
}

/// Revision stamps for the retained slot graph, without a second value memo.
#[derive(Default)]
pub struct Revisions {
    /// Current input revision.
    pub revision: Cell<usize>,
    /// Slot programs skipped because every dependency kept its change stamp.
    pub verified_cutoffs: Cell<usize>,
    /// Recomputed slots whose equal result kept its previous change stamp.
    pub value_cutoffs: Cell<usize>,
    /// Pending slot programs actually recomputed.
    pub recomputed: Cell<usize>,
    changed: RefCell<FxHashMap<QueryId, usize>>,
    pending: RefCell<FxHashMap<QueryId, Rc<Pending>>>,
    pub(crate) resolve: Cell<Option<Resolve>>,
}

impl Revisions {
    /// Number of retained query change stamps (a diagnostic counter).
    pub fn tracked_queries(&self) -> usize {
        self.changed.borrow().len()
    }

    fn set_changed(&self, key: &QueryId, revision: usize) {
        let mut changed = self.changed.borrow_mut();
        if let Some(stamp) = changed.get_mut(key) {
            *stamp = revision;
        } else {
            changed.insert(key.clone(), revision);
        }
    }

    pub(crate) fn prune(&self, eng: &Engine) {
        let reads = eng.reads.borrow();
        let slots = eng.slots_by_key.borrow();
        self.changed
            .borrow_mut()
            .retain(|key, _| reads.contains_key(key) || slots.contains_key(key));
    }

    pub(crate) fn begin_queries(
        &self,
        eng: &Engine,
        invalid: &FxHashSet<String>,
        values: &FxHashMap<String, Value>,
        forced: &FxHashSet<String>,
        mut capture: impl FnMut(&Value) -> Matches,
    ) {
        self.pending.borrow_mut().clear();
        self.revision.set(self.revision.get() + 1);
        for key in invalid {
            let id = eng.query_id(key);
            if let Some(value) = values.get(key) {
                self.pending.borrow_mut().insert(
                    id.clone(),
                    Rc::new(Pending {
                        value: value.clone(),
                        deps: eng.query_reads_id(&id).unwrap_or_default(),
                        changed: self.changed.borrow().get(&id).copied().unwrap_or(0),
                        forced: Cell::new(forced.contains(key)),
                        matches: Some(capture(value)),
                    }),
                );
            }
            self.set_changed(&id, self.revision.get());
        }
    }

    pub(crate) fn force(&self, eng: &Engine, key: &str) {
        let id = eng.query_id(key);
        if let Some(prior) = self.pending.borrow().get(&id) {
            prior.forced.set(true);
        }
        self.set_changed(&id, self.revision.get());
    }

    pub(crate) fn accept(&self, eng: &Engine, key: &str, value: &Value) {
        let id = eng.query_id(key);
        self.accept_id(eng, &id, value);
    }

    fn accept_id(&self, eng: &Engine, key: &QueryId, value: &Value) {
        let prior = self.pending.borrow_mut().remove(key);
        if let Some(prior) = prior {
            let matches = prior
                .matches
                .as_ref()
                .map(|m| m(value, eng))
                .unwrap_or_else(|| comparable(value, eng) && equal(&prior.value, value));
            if matches {
                self.set_changed(key, prior.changed);
                self.value_cutoffs.set(self.value_cutoffs.get() + 1);
            }
        }
    }

    pub(crate) fn finish(&self) {
        self.pending.borrow_mut().clear();
    }

    pub(crate) fn begin(
        &self,
        eng: &Engine,
        invalid: &FxHashSet<String>,
        forced: &FxHashSet<String>,
        dropped: &FxHashSet<usize>,
    ) {
        self.pending.borrow_mut().clear();
        self.revision.set(self.revision.get() + 1);
        for key in invalid {
            let id = eng.query_id(key);
            if let Some((inst, name)) = eng.query_slot_id(&id) {
                let b = inst.borrow();
                if let Some(slot) = b.slot(&name) {
                    if !dropped.contains(&(Rc::as_ptr(&inst) as usize))
                        && slot.state == SlotState::Ok
                        && slot.compute.is_some()
                        && comparable(&slot.value, eng)
                    {
                        self.pending.borrow_mut().insert(
                            id.clone(),
                            Rc::new(Pending {
                                value: slot.value.clone(),
                                deps: eng.query_reads_id(&id).unwrap_or_default(),
                                changed: self.changed.borrow().get(&id).copied().unwrap_or(0),
                                forced: Cell::new(forced.contains(key)),
                                matches: None,
                            }),
                        );
                    }
                }
            }
            self.set_changed(&id, self.revision.get());
        }
    }

    pub(crate) fn compute_id(
        &self,
        eng: &Engine,
        id: &QueryId,
        run: impl FnOnce() -> R<Value>,
    ) -> R<Value> {
        let prior = self.pending.borrow().get(id).cloned();
        let Some(prior) = prior else {
            return run();
        };
        let unchanged = !prior.forced.get()
            && eng.verify_reads(|| -> R<bool> {
                let mut deps: Vec<_> = prior.deps.iter().collect();
                // Verify owners before their former contents; removed records
                // must not execute just to verify an old dependency list.
                deps.sort_by_key(|d| (!d.starts_with("root:"), *d));
                for dep in deps {
                    if self.pending.borrow().contains_key(dep) {
                        if let Some(resolve) = self.resolve.get() {
                            if !resolve(eng, dep)? {
                                return Ok(false);
                            }
                        } else {
                            let found = eng.query_slot_id(dep);
                            let Some((inst, name)) = found else {
                                return Ok(false);
                            };
                            eng.force_slot(&inst, &name)?;
                        }
                    }
                    if self.changed.borrow().get(dep).copied().unwrap_or(0) == self.revision.get() {
                        return Ok(false);
                    }
                }
                Ok(true)
            })?;
        if unchanged {
            // Keep pending membership during recursive verification, then move
            // the snapshot back without copying its dependency keys again.
            self.pending.borrow_mut().remove(id);
            let prior = Rc::unwrap_or_clone(prior);
            eng.replace_query_reads_id(id, prior.deps);
            self.set_changed(id, prior.changed);
            self.verified_cutoffs.set(self.verified_cutoffs.get() + 1);
            return Ok(prior.value);
        }
        let value = run()?;
        self.recomputed.set(self.recomputed.get() + 1);
        self.accept_id(eng, id, &value);
        Ok(value)
    }
}
