//! Feature-only root-binding boundaries, stored separately from evaluation spans.
//!
//! These records own bounded name text and scalar diagnostics, never runtime
//! values, scopes, engines or callbacks. A [`RootAttempt`] is the first local in
//! `bind_root`; its final boundary consequently follows later local destruction.
//! Function arguments and the attempt's own report storage are released later.
//! Name copying happens before the local span starts, but inside its parent.
//!
//! Like evaluation spans, the counters and clocks are process-wide and include
//! nested work. A cumulative requested peak is not a phase-local peak or RSS.
//! Recording allocates, and parent/child intervals must not be summed together.
//!
//! When a phase-event sink is installed, a scalar EventScope shares the local
//! span's ordinal. The local starting stamp precedes the event entry; each mark
//! precedes its next-stage event, and the final mark precedes the ending event.
//! Their clocks therefore bracket different diagnostic recording overhead.
//! Root records retain neither the EventScope nor the event sink.

use crate::evaluation_diagnostics::{EventStage, Span};
use crate::phase_events::{EventScope, Outcome as EventOutcome, RootIdentity, RootKind};
use serde::Serialize;
use std::cell::{Cell, RefCell};

/// Maximum retained UTF-8 bytes of a root's diagnostic name.
pub const ROOT_NAME_LIMIT: usize = 256;

/// Source supplied to `bind_root`, without retaining the source itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Evaluate a source expression before binding.
    Expr,
    /// Bind a supplied document value.
    Doc,
}

/// Result of dispatch, or the final reason an attempt did not return normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Dispatch returned a bound value, possibly through reuse.
    Bound,
    /// Dispatch requested later settlement.
    Defer,
    /// Dispatch returned an evaluation diagnostic.
    EvalError,
    /// Dispatch was tainted by an earlier failure.
    Taint,
    /// Retained-round selection omitted this root.
    Skipped,
    /// The attempt guard was released during unwinding.
    Unwound,
    /// The body did not record a completed handler action.
    Incomplete,
}

/// Action actually taken by the root result handler.
///
/// In particular, queueing is recorded directly, not inferred from the initial
/// phase: a native callback can change that phase before the result is handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    /// The value was installed in the environment.
    Published,
    /// A deferred source was saved for a later attempt.
    DeferredQueued,
    /// A deferred result required no further queue entry.
    DeferIgnored,
    /// An evaluation diagnostic was reported.
    EvalError,
    /// Taint required no additional diagnostic.
    Taint,
    /// The root was skipped before dispatch.
    Skipped,
}

/// One root attempt, submitted in completion order to this thread's buffer.
#[derive(Serialize)]
pub struct RootRecord {
    /// UTF-8 root name prefix bounded by [`ROOT_NAME_LIMIT`].
    pub root_name: String,
    /// Byte length of the original name before capping.
    pub root_name_original_bytes: usize,
    /// Whether the stored name omits an original suffix.
    pub root_name_truncated: bool,
    /// Engine phase at entry; callbacks may change the phase later.
    pub initial_phase: u8,
    /// Origin of the source supplied to this attempt.
    pub source_kind: SourceKind,
    /// Number of actual producer calls; zero includes a successful Edits reuse.
    pub producer_calls: usize,
    /// Source evaluation returned successfully, rather than deferring or failing.
    pub source_successes: usize,
    /// Recursive binding returned a Result, including an error Result.
    pub binding_returns: usize,
    /// Result observed immediately after dispatch; absent for skip or unwind.
    pub dispatch_outcome: Option<Outcome>,
    /// Handler action, absent if it did not finish normally.
    pub completion: Option<Completion>,
    /// Final outcome; unwind overrides any earlier dispatch result.
    pub outcome: Outcome,
    /// Active interval when normal body completion was interrupted.
    pub interrupted_stage: Option<&'static str>,
    /// Local span only: it is never submitted to evaluation's global span list.
    pub span: Span,
}

struct State {
    record: RootRecord,
    active: &'static str,
    events: EventScope,
}

thread_local! {
    static ROOTS: RefCell<Vec<RootRecord>> = const { RefCell::new(Vec::new()) };
    static NEXT_ORDINAL: Cell<usize> = const { Cell::new(0) };
}

/// First-local guard for one root binding, including early return and unwind.
///
/// All methods release their internal borrow before returning to the caller.
/// No borrow is held across expression evaluation, binding or native callbacks.
pub struct RootAttempt {
    state: RefCell<Option<State>>,
}

impl RootAttempt {
    /// Begin an attempt with bounded name metadata and no runtime owners.
    /// Its local clock starts before the optional event entry is published.
    pub fn new(name: &str, initial_phase: u8, source_kind: SourceKind) -> Self {
        let mut end = name.len().min(ROOT_NAME_LIMIT);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let root_name = name[..end].to_owned();
        let ordinal = NEXT_ORDINAL
            .try_with(|next| {
                let ordinal = next.get();
                next.set(ordinal.saturating_add(1));
                ordinal
            })
            .unwrap_or(usize::MAX);
        let identity = RootIdentity::new(
            name,
            match source_kind {
                SourceKind::Expr => RootKind::Expression,
                SourceKind::Doc => RootKind::Document,
            },
        );
        Self {
            state: RefCell::new(Some(State {
                record: RootRecord {
                    root_name,
                    root_name_original_bytes: name.len(),
                    root_name_truncated: end != name.len(),
                    initial_phase,
                    source_kind,
                    producer_calls: 0,
                    source_successes: 0,
                    binding_returns: 0,
                    dispatch_outcome: None,
                    completion: None,
                    outcome: Outcome::Incomplete,
                    interrupted_stage: None,
                    span: Span::new("root_bind", ordinal),
                },
                active: "dispatch",
                events: EventScope::begin(
                    EventStage::RootDispatch as u32,
                    ordinal as u64,
                    identity,
                ),
            })),
        }
    }

    /// Immediately before source evaluation or document cloning in the producer.
    pub fn source_started(&self) {
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().unwrap();
        state.record.span.mark(state.active);
        state.record.producer_calls += 1;
        state.active = "source_eval";
        state.events.transition(EventStage::RootSourceEval as u32);
    }

    /// After successful source evaluation, before recursive type binding.
    pub fn source_evaluated(&self) {
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().unwrap();
        state.record.span.mark(state.active);
        state.record.source_successes += 1;
        state.active = "recursive_bind";
        state.events.transition(EventStage::RootTypeBind as u32);
    }

    /// After recursive binding returns either a value or an error.
    pub fn binding_returned(&self) {
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().unwrap();
        state.record.span.mark(state.active);
        state.record.binding_returns += 1;
        state.active = "dispatch_return";
        state
            .events
            .transition(EventStage::RootDispatchReturn as u32);
    }

    /// After step/Edits dispatch returns, before the existing result handler.
    /// Call with Bound, Defer, EvalError or Taint; this does not infer producer work.
    pub fn dispatch_returned(&self, outcome: Outcome) {
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().unwrap();
        state.record.span.mark(state.active);
        state.record.dispatch_outcome = Some(outcome);
        state.active = "publish_or_handle";
        state
            .events
            .transition(EventStage::RootPublishOrDefer as u32);
    }

    /// After the result handler's actual publication, queue, report or taint action.
    pub fn body_returned(&self, completion: Completion) {
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().unwrap();
        state.record.span.mark(state.active);
        state.record.completion = Some(completion);
        state.active = "scope_release";
        state.events.transition(EventStage::RootScopeRelease as u32);
    }

    /// The retained round skipped this root before dispatch or source production.
    pub fn skipped(&self) {
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().unwrap();
        state.record.span.mark(state.active);
        state.record.completion = Some(Completion::Skipped);
        state.active = "scope_release";
        state.events.transition(EventStage::RootScopeRelease as u32);
    }
}

impl Drop for RootAttempt {
    fn drop(&mut self) {
        let Some(mut state) = self.state.get_mut().take() else {
            return;
        };
        let unwound = std::thread::panicking();
        state.record.span.mark(state.active);
        state.record.outcome = if unwound {
            Outcome::Unwound
        } else if state.record.completion == Some(Completion::Skipped) {
            Outcome::Skipped
        } else if state.record.completion.is_some() {
            state.record.dispatch_outcome.unwrap_or(Outcome::Incomplete)
        } else {
            Outcome::Incomplete
        };
        if unwound || state.record.completion.is_none() {
            state.record.interrupted_stage = Some(state.active);
        }
        state.events.finish(match state.record.outcome {
            Outcome::Bound | Outcome::Skipped => EventOutcome::Ok,
            Outcome::Defer => EventOutcome::Defer,
            Outcome::EvalError => EventOutcome::Eval,
            Outcome::Taint => EventOutcome::Taint,
            Outcome::Unwound => EventOutcome::Unwound,
            Outcome::Incomplete => EventOutcome::Incomplete,
        });
        // During TLS destruction this optional diagnostic buffer may be gone.
        // No runtime owners are held by the record being discarded in that case.
        let _ = ROOTS.try_with(|roots| roots.borrow_mut().push(state.record));
    }
}

/// Drain completed root records without touching evaluation spans or live guards.
/// Ordinals and process counters are not reset. Records retain no runtime owners.
pub fn take_root_binding_diagnostics() -> Vec<RootRecord> {
    ROOTS.with(|roots| std::mem::take(&mut *roots.borrow_mut()))
}

#[cfg(test)]
#[path = "../tests/private/root_binding_diagnostics_test.rs"]
mod tests;
