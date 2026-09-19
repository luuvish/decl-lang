//! Scalar-only, separately compiled evaluation phase diagnostics.
//!
//! The optional `runtime-diagnostics` feature requires libc `clock_gettime` with
//! `CLOCK_MONOTONIC` and `CLOCK_PROCESS_CPUTIME_ID` on its target. These hooks are
//! excluded from feature-empty native builds. The optional backend's Windows
//! portability has not been established; this module supplies no alternate clock.
//!
//! Completed spans are stored on the calling thread without runtime owners.
//! Their clocks and allocation counters cover the whole process, however, so
//! concurrent threads can contribute to CPU and allocation observations.
//! Recording span vectors itself allocates; nested intervals and their parent
//! intervals must not be added together as independent costs.
use serde::Serialize;
use std::cell::RefCell;

macro_rules! event_stages {
    ($($variant:ident = $id:literal => $label:literal),+ $(,)?) => {
        /// Explicit operation entry IDs for the optional bounded event stream.
        #[derive(Clone, Copy)]
        #[repr(u32)]
        pub enum EventStage {
            $(#[doc = $label] $variant = $id,)+
        }
        /// Dictionary for this build's event IDs; save it with the binary stream.
        pub const EVENT_STAGES: &[(u32, &str)] = &[$(($id, $label),)+];
    };
}
event_stages! {
    EvaluateSetup = 100 => "evaluate.setup",
    RoundEngineSetup = 200 => "round.engine_setup",
    RoundBind = 201 => "round.bind",
    RoundForceRootsInitial = 202 => "round.force_roots_initial",
    RoundSettle = 203 => "round.settle",
    RoundEditsFinish = 204 => "round.edits_finish",
    RoundEdgeKeyAndCycle = 205 => "round.edge_key_and_cycle",
    RoundAdvance = 206 => "round.advance",
    RoundRetainedTransfer = 207 => "round.retained_round_transfer",
    RoundCompleteDropPrevious = 208 => "round.complete_drop_previous_and_identity_sweep",
    RoundFallbackFreezeReset = 209 => "round.fallback_freeze_reset",
    SettleTakeSnapshot = 300 => "settle.take_snapshot",
    SettleDeferredSnapshot = 301 => "settle.deferred_snapshot",
    SettleForceDeferredSlots = 302 => "settle.force_deferred_slots",
    SettleBindDeferredRoots = 303 => "settle.bind_deferred_roots",
    SettleForceRoots = 304 => "settle.force_roots",
    SettleLiveEdges = 305 => "settle.live_edges",
    SettleCompareEdges = 306 => "settle.compare_edges",
    AdvanceClassify = 400 => "advance.classify",
    AdvanceFreeze = 401 => "advance.freeze",
    AdvanceRevisionReset = 402 => "advance.revision_reset",
    RootDispatch = 500 => "root.dispatch",
    RootSourceEval = 501 => "root.source_eval",
    RootTypeBind = 502 => "root.recursive_bind",
    RootDispatchReturn = 503 => "root.dispatch_return",
    RootPublishOrDefer = 504 => "root.publish_or_handle",
    RootScopeRelease = 505 => "root.scope_release",
}

/// Ordered clock reads followed by a process-wide requested-allocation snapshot.
///
/// The two monotonic reads bracket the CPU read only. The allocation ledger is
/// read afterwards and is neither clock-bracketed nor an atomic transaction.
#[derive(Clone, Copy, Serialize)]
pub struct Stamp {
    /// Opening `CLOCK_MONOTONIC` reading in nanoseconds, with no civil-time epoch.
    pub wall_ns: u64,
    /// `CLOCK_PROCESS_CPUTIME_ID` nanoseconds consumed by all process threads.
    pub cpu_ns: u64,
    /// Closing `CLOCK_MONOTONIC` reading after the CPU clock read.
    pub sampled_until_ns: u64,
    /// Cumulative ledger read after `sampled_until_ns`; not a phase-local peak.
    pub allocation: crate::allocation_diagnostics::Snapshot,
    /// Thread-local prefix traffic sampled after the clock and allocation reads.
    pub prefix_paths: crate::semantics::PrefixPathDiagnostics,
    /// Existing constructor/cache counters, copied without allocating.
    pub traffic: crate::retention_diagnostics::TrafficSnapshot,
}
fn clock_ns(id: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid clock ID and initialized writable timespec, no pointers escape.
    assert_eq!(unsafe { libc::clock_gettime(id, &mut ts) }, 0);
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
impl Stamp {
    fn now() -> Self {
        let wall_ns = clock_ns(libc::CLOCK_MONOTONIC);
        let cpu_ns = clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID);
        let sampled_until_ns = clock_ns(libc::CLOCK_MONOTONIC);
        Self {
            wall_ns,
            cpu_ns,
            sampled_until_ns,
            allocation: crate::allocation_diagnostics::snapshot(),
            prefix_paths: crate::semantics::prefix_path_diagnostics(),
            traffic: crate::retention_diagnostics::traffic_snapshot(),
        }
    }
}
/// One interval between a span's previous boundary and its next explicit mark.
///
/// Durations use the opening clock samples at both boundaries. They include
/// nested runtime work and any recording overhead after the previous boundary.
#[derive(Serialize)]
pub struct Stage {
    /// Caller-provided label for the interval ending at this mark.
    pub name: &'static str,
    /// Previous mark boundary, or the span start for the first interval.
    pub start: Stamp,
    /// Boundary captured before this stage is appended to the report vector.
    pub end: Stamp,
    /// Difference between `end.wall_ns` and `start.wall_ns`.
    pub wall_ns: u64,
    /// Difference between process-wide `end.cpu_ns` and `start.cpu_ns`.
    pub cpu_ns: u64,
}
/// Caller-labeled sequence of explicit evaluation boundaries.
///
/// A span is submitted to thread-local storage only by [`Span::finish`]. Dropping
/// it without finishing discards its report; finishing adds no implicit boundary.
#[derive(Serialize)]
pub struct Span {
    /// Caller-provided category, such as setup, a round, or settlement.
    pub kind: &'static str,
    /// Caller-provided index; this type imposes no numbering convention.
    pub ordinal: usize,
    /// Clock and allocation readings captured when the span was constructed.
    pub start: Stamp,
    /// Intervals recorded in mark order; a span may contain no marks.
    pub stages: Vec<Stage>,
    previous: Stamp,
    #[serde(skip)]
    event_scope: Option<crate::phase_events::EventScope>,
}
thread_local! { static SPANS: RefCell<Vec<Span>> = const { RefCell::new(Vec::new()) }; }
impl Span {
    /// Capture the starting boundary and create an empty, unsubmitted span.
    ///
    /// Clock-query failure panics. The category and ordinal are retained as
    /// metadata only; no engine, scope or evaluated value is retained.
    pub fn new(kind: &'static str, ordinal: usize) -> Self {
        let start = Stamp::now();
        Self {
            kind,
            ordinal,
            start,
            stages: Vec::new(),
            previous: start,
            event_scope: None,
        }
    }
    /// Publish entry before the named operation. Marks instead label completed
    /// intervals and must never be interpreted as an operation's start.
    /// An installed sink adds bounded diagnostic I/O; without one this is inert.
    pub fn enter(&mut self, stage: EventStage) {
        match &self.event_scope {
            Some(scope) => scope.transition(stage as u32),
            None => {
                self.event_scope = Some(crate::phase_events::EventScope::begin(
                    stage as u32,
                    self.ordinal as u64,
                    crate::phase_events::RootIdentity::NONE,
                ));
            }
        }
    }
    /// Capture a boundary and append the interval since the preceding boundary.
    ///
    /// Appending may grow the stage vector after the captured endpoint; that
    /// recording work falls inside a later interval if another mark follows.
    /// Clock-query failure panics before appending a stage.
    pub fn mark(&mut self, name: &'static str) {
        let end = Stamp::now();
        self.stages.push(Stage {
            name,
            start: self.previous,
            end,
            wall_ns: end.wall_ns - self.previous.wall_ns,
            cpu_ns: end.cpu_ns - self.previous.cpu_ns,
        });
        self.previous = end;
    }
    /// Submit this span to the current thread's report buffer in finish order.
    ///
    /// This may grow the buffer but takes no final sample. Work after the last
    /// mark, including submission, has no additional interval in this span.
    pub fn finish(self) {
        if let Some(scope) = &self.event_scope {
            use crate::phase_events::Outcome;
            scope.finish(match self.kind {
                "advance_rejected" => Outcome::Rejected,
                "advance_unwound" => Outcome::Unwound,
                _ => Outcome::Ok,
            });
        }
        SPANS.with(|s| s.borrow_mut().push(self));
    }
}

/// Advance-only partition, submitted after the advance function's runtime
/// locals have dropped. It owns scalar diagnostics, never an Engine or Value.
/// Early returns and unwinding retain a partial prefix without claiming reuse.
pub(crate) struct AdvanceSpan {
    span: Option<Span>,
    active: &'static str,
    accepted: bool,
}
impl AdvanceSpan {
    pub(crate) fn new(ordinal: usize) -> Self {
        let mut span = Span::new("advance_rejected", ordinal);
        span.enter(EventStage::AdvanceClassify);
        Self {
            span: Some(span),
            active: "classify",
            accepted: false,
        }
    }
    pub(crate) fn classified(&mut self) {
        self.span.as_mut().unwrap().mark("classify");
        self.span.as_mut().unwrap().enter(EventStage::AdvanceFreeze);
        self.active = "freeze";
    }
    pub(crate) fn frozen(&mut self) {
        self.span.as_mut().unwrap().mark("freeze");
        self.span
            .as_mut()
            .unwrap()
            .enter(EventStage::AdvanceRevisionReset);
        self.active = "revision_reset";
    }
    pub(crate) fn accepted(&mut self) {
        self.accepted = true;
    }
}
impl Drop for AdvanceSpan {
    fn drop(&mut self) {
        let mut span = self.span.take().unwrap();
        span.kind = if std::thread::panicking() {
            "advance_unwound"
        } else if self.accepted {
            "advance"
        } else {
            "advance_rejected"
        };
        span.mark(self.active);
        span.finish();
    }
}

/// Drain completed spans from the calling thread in their submission order.
///
/// Other threads' buffers and unfinished spans are unaffected. Reports own
/// scalar metadata and their vectors, with no engine or evaluated-value owners.
/// Draining does not reset the process-wide clocks or allocation counters.
pub fn take_evaluation_diagnostics() -> Vec<Span> {
    SPANS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}
