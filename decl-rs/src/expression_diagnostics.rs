//! Bounded, source-selected comprehension observations for diagnostic builds.
//! Records contain scalars only. Inclusive nested intervals are not additive;
//! returned lazy heads can be forced after the recorded expression has ended.
use crate::ast::{self, DeclBody, Expr, Loc};
use crate::module::Module;
use crate::semantics::{Fail, R};
use serde::Serialize;
use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::{Rc, Weak};

/// Maximum registered syntax sites on one evaluation thread.
pub const MAX_SITES: usize = 2;
/// Maximum clauses counted separately in one selected comprehension.
pub const MAX_CLAUSES: usize = 8;
/// Maximum retained completed attempts; overflow invalidates attribution.
pub const MAX_RECORDS: usize = 4096;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, Default, Serialize)]
struct Stamp {
    wall_ns: Option<u64>,
    cpu_ns: Option<u64>,
    sampled_until_ns: Option<u64>,
    errno: [i32; 3],
    allocation: crate::allocation_diagnostics::Snapshot,
}
fn clock(id: libc::clockid_t) -> (Option<u64>, i32) {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the initialized timespec is writable and no pointer escapes.
    if unsafe { libc::clock_gettime(id, &mut ts) } != 0 {
        return (
            None,
            std::io::Error::last_os_error().raw_os_error().unwrap_or(-1),
        );
    }
    if ts.tv_sec < 0 || !(0..1_000_000_000).contains(&ts.tv_nsec) {
        return (None, libc::EOVERFLOW);
    }
    let value = (ts.tv_sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|n| n.checked_add(ts.tv_nsec as u64));
    (value, if value.is_some() { 0 } else { libc::EOVERFLOW })
}
impl Stamp {
    fn now() -> Self {
        let (wall_ns, a) = clock(libc::CLOCK_MONOTONIC);
        let (cpu_ns, b) = clock(libc::CLOCK_PROCESS_CPUTIME_ID);
        let (sampled_until_ns, c) = clock(libc::CLOCK_MONOTONIC);
        Self {
            wall_ns,
            cpu_ns,
            sampled_until_ns,
            errno: [a, b, c],
            allocation: crate::allocation_diagnostics::snapshot(),
        }
    }
    fn valid(self) -> bool {
        self.errno == [0; 3]
            && self.wall_ns.is_some()
            && self.cpu_ns.is_some()
            && self.sampled_until_ns >= self.wall_ns
    }
}

#[derive(Clone, Copy, Default, Serialize)]
struct Work {
    // Columns: iterator attempts, visited items, filter attempts, false filters,
    // candidates passing every filter. A failed filter increments attempts only.
    clauses: [[u64; 5]; MAX_CLAUSES],
    emitted_heads: u64,
    values_attempts: u64,
    values_returns: u64,
    values_items: u64,
    values_capacity: u64,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok,
    Defer,
    EvalError,
    Taint,
    Unwound,
    Incomplete,
}
#[derive(Serialize)]
struct Record {
    id: u64,
    parent_id: Option<u64>,
    site_id: u8,
    compiled: bool,
    engine_phase: u8,
    start: Stamp,
    end: Stamp,
    outcome: Outcome,
    work: Work,
}
#[derive(Serialize)]
struct Site {
    site_id: u8,
    module: String,
    location: [usize; 4],
    clause_count: usize,
    compiled_programs: u64,
    #[serde(skip)]
    expression: Weak<Expr>,
}

/// Scalar report drained after the measured worker's runtime owners are released.
/// `valid` covers bookkeeping/clocks, not source quality, output or performance.
#[derive(Serialize)]
pub struct Report {
    schema: u8,
    clause_columns: [&'static str; 5],
    values_counter_scope: &'static str,
    generation: u64,
    valid: bool,
    max_records: usize,
    record_capacity_bytes: usize,
    frame_capacity_bytes: usize,
    attempts: u64,
    dropped_records: u64,
    maximum_depth: usize,
    open_attempts: usize,
    counter_overflow: bool,
    clock_failure: bool,
    stack_failure: bool,
    sites: Vec<Site>,
    records: Vec<Record>,
}
struct State {
    report: Report,
    frames: Vec<Record>,
}
thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// Installation guard. Closing removes the thread-local state before dropping it.
/// Old attempt/program tokens cannot write into a later installation.
/// The guard must be finished or dropped on its installing thread.
pub struct SessionGuard {
    generation: u64,
    closed: bool,
    thread: PhantomData<Rc<()>>,
}

/// Install a fresh bounded buffer before the measured timeline starts.
/// Nested installation and capacities outside `1..=MAX_RECORDS` are rejected.
pub fn install(max_records: usize) -> Result<SessionGuard, &'static str> {
    if !(1..=MAX_RECORDS).contains(&max_records) {
        return Err("invalid expression record limit");
    }
    STATE.with(|state| {
        if state.borrow().is_some() { return Err("expression diagnostics already installed"); }
        let generation = GENERATION.with(|g| {
            let next = g.get().checked_add(1).ok_or("expression generation exhausted")?;
            g.set(next); Ok::<u64, &'static str>(next)
        })?;
        let records = Vec::with_capacity(max_records);
        let frames = Vec::with_capacity(MAX_DEPTH);
        let report = Report { schema: 1,
            clause_columns: ["iterator_attempts", "visited_items", "filter_attempts", "false_filters", "passed_candidates"],
            values_counter_scope: "Nearest active watched attempt only; excludes nested watched attempts. Clock/allocation intervals are inclusive.",
            generation, valid: true, max_records,
            record_capacity_bytes: records.capacity() * std::mem::size_of::<Record>(),
            frame_capacity_bytes: frames.capacity() * std::mem::size_of::<Record>(),
            attempts: 0, dropped_records: 0, maximum_depth: 0, open_attempts: 0,
            counter_overflow: false, clock_failure: false, stack_failure: false,
            sites: Vec::with_capacity(MAX_SITES), records };
        *state.borrow_mut() = Some(State { report, frames });
        Ok(SessionGuard { generation, closed: false, thread: PhantomData })
    })
}
impl SessionGuard {
    /// Close this installation and return its records, with no runtime owners.
    /// Open attempts invalidate the report; later stale guard drops are inert.
    pub fn finish(mut self) -> Report {
        self.closed = true;
        let mut state = STATE
            .with(|s| s.borrow_mut().take())
            .expect("installed expression session");
        debug_assert_eq!(state.report.generation, self.generation);
        state.report.open_attempts = state.frames.len();
        state.report.valid &= state.frames.is_empty();
        state.report
    }
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        if !self.closed {
            let old = STATE
                .try_with(|s| {
                    let mut s = s.borrow_mut();
                    if s.as_ref()
                        .is_some_and(|s| s.report.generation == self.generation)
                    {
                        s.take()
                    } else {
                        None
                    }
                })
                .ok()
                .flatten();
            drop(old);
        }
    }
}

/// Register one Comp selected by its loaded module and zero-based start line.
/// Resolution must be unique, with the requested clause count. Full source
/// coordinates are saved; the caller binds module bytes in its frozen manifest.
/// Registration is allowed only before this installation's first attempt.
pub fn register(
    module: &Module,
    site_id: u8,
    start_line: usize,
    clauses: usize,
) -> Result<(), &'static str> {
    if site_id as usize >= MAX_SITES || !(1..=MAX_CLAUSES).contains(&clauses) {
        return Err("invalid expression site or clause count");
    }
    let path = module.path.to_str().ok_or("non-UTF8 expression module")?;
    if path.len() > 4096 {
        return Err("expression module path too long");
    }
    let mut found: Option<(Weak<Expr>, Loc)> = None;
    let mut ambiguous = false;
    let mut visit = |e: &Rc<Expr>| {
        if let (Expr::Comp { clauses: cs, .. }, Some(loc)) = (&**e, ast::expr_loc(e)) {
            if loc.sl == start_line && cs.len() == clauses {
                if found
                    .as_ref()
                    .is_some_and(|(w, _)| w.as_ptr() != Rc::as_ptr(e))
                {
                    ambiguous = true;
                }
                found = Some((Rc::downgrade(e), loc));
            }
        }
    };
    for d in &module.decls {
        match &d.body {
            DeclBody::Type { ty, .. } | DeclBody::Input { ty, .. } => {
                ast::walk_type_exprs(ty, true, &mut visit)
            }
            DeclBody::Const { ty, expr, .. } => {
                if let Some(ty) = ty {
                    ast::walk_type_exprs(ty, true, &mut visit);
                }
                ast::walk_expr_tree(expr, true, &mut visit);
            }
            DeclBody::Output { ty, expr, .. } => {
                ast::walk_type_exprs(ty, true, &mut visit);
                ast::walk_expr_tree(expr, true, &mut visit);
            }
            DeclBody::Func { body, .. } => ast::walk_expr_tree(body, true, &mut visit),
            _ => {}
        }
    }
    if ambiguous {
        return Err("ambiguous expression selector");
    }
    let (expression, loc) = found.ok_or("expression selector not found")?;
    let site = Site {
        site_id,
        module: path.to_owned(),
        location: [loc.sl, loc.sc, loc.el, loc.ec],
        clause_count: clauses,
        compiled_programs: 0,
        expression,
    };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut().ok_or("expression diagnostics not installed")?;
        if s.report.attempts != 0
            || s.report.sites.len() == MAX_SITES
            || s.report
                .sites
                .iter()
                .any(|x| x.site_id == site_id || x.expression.as_ptr() == site.expression.as_ptr())
        {
            return Err("duplicate or late expression registration");
        }
        s.report.sites.push(site);
        Ok(())
    })
}

#[derive(Clone, Copy)]
pub(crate) struct Token {
    generation: u64,
    site_id: u8,
    compiled: bool,
}
pub(crate) fn selected(e: &Rc<Expr>, compiled: bool) -> Option<Token> {
    STATE
        .try_with(|s| {
            let mut s = s.borrow_mut();
            let s = s.as_mut()?;
            let site = s
                .report
                .sites
                .iter_mut()
                .find(|x| x.expression.as_ptr() == Rc::as_ptr(e))?;
            let overflow = compiled && add(&mut site.compiled_programs, 1);
            let site_id = site.site_id;
            if overflow {
                s.report.valid = false;
                s.report.counter_overflow = true;
            }
            Some(Token {
                generation: s.report.generation,
                site_id,
                compiled,
            })
        })
        .ok()
        .flatten()
}
#[derive(Clone, Copy)]
pub(crate) struct Handle {
    generation: u64,
    id: u64,
}
pub(crate) struct Attempt {
    handle: Option<Handle>,
    done: bool,
}
impl Attempt {
    pub(crate) fn begin(token: Option<Token>, phase: u8) -> Self {
        let handle = token.and_then(|token| {
            STATE
                .try_with(|s| {
                    let mut s = s.borrow_mut();
                    let s = s.as_mut()?;
                    if token.generation != s.report.generation {
                        return None;
                    }
                    if s.frames.len() == MAX_DEPTH || s.report.attempts == u64::MAX {
                        s.report.valid = false;
                        s.report.stack_failure = true;
                        return None;
                    }
                    s.report.attempts += 1;
                    let id = s.report.attempts;
                    let parent_id = s.frames.last().map(|r| r.id);
                    let start = Stamp::now();
                    s.frames.push(Record {
                        id,
                        parent_id,
                        site_id: token.site_id,
                        compiled: token.compiled,
                        engine_phase: phase,
                        start,
                        end: start,
                        outcome: Outcome::Incomplete,
                        work: Work::default(),
                    });
                    s.report.maximum_depth = s.report.maximum_depth.max(s.frames.len());
                    Some(Handle {
                        generation: token.generation,
                        id,
                    })
                })
                .ok()
                .flatten()
        });
        Self {
            handle,
            done: false,
        }
    }
    pub(crate) fn handle(&self) -> Option<Handle> {
        self.handle
    }
    pub(crate) fn finish<T>(&mut self, result: &R<T>) {
        let outcome = match result {
            Ok(_) => Outcome::Ok,
            Err(Fail::Defer) => Outcome::Defer,
            Err(Fail::Eval(_)) => Outcome::EvalError,
            Err(Fail::Taint) => Outcome::Taint,
        };
        self.close(outcome);
    }
    fn close(&mut self, outcome: Outcome) {
        if self.done {
            return;
        }
        self.done = true;
        let Some(h) = self.handle else {
            return;
        };
        let end = Stamp::now();
        let _ = STATE.try_with(|s| {
            let mut s = s.borrow_mut();
            let Some(s) = s.as_mut() else {
                return;
            };
            if s.report.generation != h.generation {
                return;
            }
            if s.frames.last().map(|r| r.id) != Some(h.id) {
                s.report.valid = false;
                s.report.stack_failure = true;
                return;
            }
            let mut record = s.frames.pop().unwrap();
            record.end = end;
            record.outcome = outcome;
            if !record.start.valid()
                || !end.valid()
                || end.wall_ns < record.start.sampled_until_ns
                || end.cpu_ns < record.start.cpu_ns
            {
                s.report.valid = false;
                s.report.clock_failure = true;
            }
            if s.report.records.len() == s.report.max_records {
                s.report.valid = false;
                s.report.dropped_records = s.report.dropped_records.saturating_add(1);
            } else {
                s.report.records.push(record);
            }
        });
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        self.close(if std::thread::panicking() {
            Outcome::Unwound
        } else {
            Outcome::Incomplete
        });
    }
}
fn add(target: &mut u64, n: u64) -> bool {
    let (value, overflow) = target.overflowing_add(n);
    *target = if overflow { u64::MAX } else { value };
    overflow
}
fn update(handle: Option<Handle>, f: impl FnOnce(&mut Work) -> bool) {
    let _ = STATE.try_with(|s| {
        let mut s = s.borrow_mut();
        let Some(s) = s.as_mut() else {
            return;
        };
        let record = match handle {
            Some(h) if h.generation == s.report.generation => {
                s.frames.iter_mut().rev().find(|r| r.id == h.id)
            }
            Some(_) => None,
            None => s.frames.last_mut(),
        };
        if record.is_some_and(|r| f(&mut r.work)) {
            s.report.valid = false;
            s.report.counter_overflow = true;
        }
    });
}
pub(crate) fn clause(handle: Option<Handle>, index: usize, column: usize) {
    if let Some(h) = handle {
        update(Some(h), |w| add(&mut w.clauses[index][column], 1));
    }
}
pub(crate) fn emitted(handle: Option<Handle>) {
    if let Some(h) = handle {
        update(Some(h), |w| add(&mut w.emitted_heads, 1));
    }
}
pub(crate) fn values_attempt() {
    update(None, |w| add(&mut w.values_attempts, 1));
}
pub(crate) fn values_return(items: usize, capacity: usize) {
    update(None, |w| {
        let a = add(&mut w.values_returns, 1);
        let b = add(&mut w.values_items, items as u64);
        let c = add(&mut w.values_capacity, capacity as u64);
        a || b || c
    });
}

#[cfg(test)]
#[path = "../tests/private/expression_diagnostics_test.rs"]
mod tests;
