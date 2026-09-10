//! The incremental, memoized query core of the query engine (qengine/DESIGN.md);
//! the faithful port of decl-ts/src/qengine/db.ts.
//
//! A demand-driven database of queries. Some keys are *inputs* (set from
//! outside); the rest are *derived* — computed by a resolver-supplied function
//! that reads other queries through the database, so its dependencies are
//! recorded automatically. Results are memoized with a global revision and two
//! stamps per entry, giving early cutoff on both axes. The verify cutoff: if
//! none of an entry's dependencies changed since it was verified, it is reused
//! without recomputing. The value cutoff: a recompute that yields an equal
//! value does not advance the entry's changed-revision, so its dependents cut
//! off in turn. This is Salsa/Adapton-shaped, generic over the value it stores,
//! and
//! independent of the decl language; the decl layer (compiled expressions as
//! derived queries) is built on top.
use crate::semantics::{value_eq, EvalErr, Fail, Value};
use rustc_hash::FxHashMap;
use std::cell::{Cell, RefCell};

/// a query key — an opaque string; the decl layer defines the grammar of keys
pub type Key = String;

/// the failure a query may produce: a dependency cycle the core itself detects
/// (§7.6/§9.3), or a failure raised by a compute (a value-layer error/taint)
pub enum DbErr {
    /// a query that (transitively) reads itself, with the path it walked
    Cycle(Vec<Key>),
    /// a failure raised by a compute (a value-layer error or taint)
    Fail(Fail),
}
/// a query's result: its value or a [`DbErr`]
pub type DResult = Result<Value, DbErr>;

/// compute a derived query's value, reading other queries through the database
pub type Compute = Box<dyn Fn(&Db) -> DResult>;
/// map a derived key to its compute; `None` for an unknown key
pub type Resolve = Box<dyn Fn(&str) -> Option<Compute>>;

struct Entry {
    value: Value,
    deps: Vec<Key>,
    changed_rev: u64,  // last revision the value actually changed
    verified_rev: u64, // last revision the value was confirmed current
}

/// a query currently (re)computing: its key, and the deps it reads meanwhile
struct Frame {
    key: Key,
    deps: RefCell<Vec<Key>>,
}

/// the incremental, memoized query database (see the module comment)
pub struct Db {
    rev: Cell<u64>,
    memo: RefCell<FxHashMap<Key, Entry>>,
    inputs: RefCell<FxHashMap<Key, Value>>,
    input_rev: RefCell<FxHashMap<Key, u64>>,
    resolve: Resolve,
    // the queries currently (re)computing, innermost last — dependency capture
    // and cycle detection
    stack: RefCell<Vec<Frame>>,
}

impl Db {
    /// a fresh database whose derived keys are computed by `resolve`
    pub fn new(resolve: Resolve) -> Db {
        Db {
            rev: Cell::new(0),
            memo: RefCell::new(FxHashMap::default()),
            inputs: RefCell::new(FxHashMap::default()),
            input_rev: RefCell::new(FxHashMap::default()),
            resolve,
            stack: RefCell::new(Vec::new()),
        }
    }

    /// Drop memoized results when the owning value context advances.
    pub fn clear(&self) {
        self.memo.borrow_mut().clear();
        self.rev.set(self.rev.get() + 1);
    }

    /// the current revision (advances when an input changes)
    pub fn revision(&self) -> u64 {
        self.rev.get()
    }

    /// Set an input's value. If it differs from the current value the revision
    /// advances and dependents recompute on demand; an equal value is a no-op.
    pub fn set_input(&self, key: &str, value: Value) {
        if let Some(old) = self.inputs.borrow().get(key) {
            if value_eq(old, &value) {
                return;
            }
        }
        self.rev.set(self.rev.get() + 1);
        self.inputs.borrow_mut().insert(key.to_string(), value);
        self.input_rev
            .borrow_mut()
            .insert(key.to_string(), self.rev.get());
    }

    /// read a query's value, recording it as a dependency of the caller (if any)
    pub fn query(&self, key: &str) -> DResult {
        if let Some(top) = self.stack.borrow().last() {
            top.deps.borrow_mut().push(key.to_string());
        }
        self.evaluate(key)
    }

    /// bring a query up to date and return its value, recording no dependency
    fn evaluate(&self, key: &str) -> DResult {
        if let Some(v) = self.inputs.borrow().get(key) {
            return Ok(v.clone());
        }
        let cur = self.rev.get();
        // already current, or memoized with a known set of deps to re-verify
        let (has_memo, verified, deps) = {
            let memo = self.memo.borrow();
            match memo.get(key) {
                Some(m) if m.verified_rev == cur => return Ok(m.value.clone()),
                Some(m) => (true, m.verified_rev, m.deps.clone()),
                None => (false, 0, Vec::new()),
            }
        };
        if has_memo && self.deps_unchanged(&deps, verified)? {
            // verify cutoff: no dependency changed since this was verified
            if let Some(m) = self.memo.borrow_mut().get_mut(key) {
                m.verified_rev = cur;
            }
            return Ok(self.memo.borrow().get(key).unwrap().value.clone());
        }
        self.recompute(key, has_memo)
    }

    fn deps_unchanged(&self, deps: &[Key], verified: u64) -> Result<bool, DbErr> {
        for d in deps {
            self.evaluate(d)?; // bring the dependency current (may recompute it)
            if self.changed_rev_of(d) > verified {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn recompute(&self, key: &str, has_prev: bool) -> DResult {
        if self.stack.borrow().iter().any(|f| f.key == key) {
            let mut cyc: Vec<Key> = self.stack.borrow().iter().map(|f| f.key.clone()).collect();
            cyc.push(key.to_string());
            return Err(DbErr::Cycle(cyc));
        }
        let compute = (self.resolve)(key).ok_or_else(|| {
            DbErr::Fail(Fail::Eval(EvalErr {
                msg: format!("no such query: {key}"),
                code: None,
            }))
        })?;
        self.stack.borrow_mut().push(Frame {
            key: key.to_string(),
            deps: RefCell::new(Vec::new()),
        });
        let result = compute(self);
        let frame = self.stack.borrow_mut().pop().unwrap();
        let value = result?; // propagate after popping the frame
        let deps = frame.deps.into_inner();
        // value cutoff: an equal recompute keeps the old changed-revision, so
        // dependents that only read this query need not recompute
        let cur = self.rev.get();
        let prev_changed = if has_prev {
            self.memo
                .borrow()
                .get(key)
                .filter(|m| value_eq(&value, &m.value))
                .map(|m| m.changed_rev)
        } else {
            None
        };
        let changed_rev = prev_changed.unwrap_or(cur);
        self.memo.borrow_mut().insert(
            key.to_string(),
            Entry {
                value: value.clone(),
                deps,
                changed_rev,
                verified_rev: cur,
            },
        );
        Ok(value)
    }

    fn changed_rev_of(&self, key: &str) -> u64 {
        if let Some(r) = self.input_rev.borrow().get(key) {
            return *r;
        }
        self.memo
            .borrow()
            .get(key)
            .map(|m| m.changed_rev)
            .unwrap_or(0)
    }
}
