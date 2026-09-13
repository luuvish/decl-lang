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
        SPANS.with(|s| s.borrow_mut().push(self));
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
