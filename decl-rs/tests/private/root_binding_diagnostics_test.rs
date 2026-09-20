//! Native diagnostic boundaries, independent draining, unwind and owner release.
use super::*;
use crate::phase_events::{self, EventKind};
use std::fs::{File, OpenOptions};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct EventFile(PathBuf);

impl EventFile {
    fn create() -> (Self, File) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "decl-root-binding-events-{}-{}.bin",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        (Self(path), file)
    }

    fn records(&self) -> Vec<Vec<u8>> {
        let bytes = std::fs::read(&self.0).unwrap();
        assert_eq!(bytes.len() % phase_events::RECORD_SIZE, 0);
        bytes
            .chunks_exact(phase_events::RECORD_SIZE)
            .map(<[u8]>::to_vec)
            .collect()
    }
}

impl Drop for EventFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn wire_u64(record: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(record[offset..offset + 8].try_into().unwrap())
}

fn wire_stage(record: &[u8]) -> u32 {
    u32::from_le_bytes(record[12..16].try_into().unwrap())
}

fn names(record: &RootRecord) -> Vec<&'static str> {
    record.span.stages.iter().map(|stage| stage.name).collect()
}

fn assert_adjacent(record: &RootRecord) {
    let mut previous = record.span.start.wall_ns;
    for stage in &record.span.stages {
        assert_eq!(stage.start.wall_ns, previous);
        assert!(stage.end.wall_ns >= stage.start.wall_ns);
        assert_eq!(stage.wall_ns, stage.end.wall_ns - stage.start.wall_ns);
        previous = stage.end.wall_ns;
    }
}

#[test]
fn successful_root_records_do_not_change_the_evaluation_span_sequence() {
    take_root_binding_diagnostics();
    crate::evaluation_diagnostics::take_evaluation_diagnostics();
    let (event_file, file) = EventFile::create();
    let sink = phase_events::install(file, 64).unwrap();
    let mut sentinel = Span::new("native_sentinel", 7);
    sentinel.mark("before_root");
    sentinel.finish();
    {
        let attempt = RootAttempt::new("report", 2, SourceKind::Expr);
        attempt.source_started();
        attempt.source_evaluated();
        attempt.binding_returned();
        attempt.dispatch_returned(Outcome::Bound);
        attempt.body_returned(Completion::Published);
        assert!(take_root_binding_diagnostics().is_empty());
    }
    let records = take_root_binding_diagnostics();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.outcome, Outcome::Bound);
    assert_eq!(record.completion, Some(Completion::Published));
    assert_eq!(
        (
            record.producer_calls,
            record.source_successes,
            record.binding_returns
        ),
        (1, 1, 1)
    );
    assert_eq!(
        names(record),
        [
            "dispatch",
            "source_eval",
            "recursive_bind",
            "dispatch_return",
            "publish_or_handle",
            "scope_release"
        ]
    );
    assert_adjacent(record);
    let evaluation = crate::evaluation_diagnostics::take_evaluation_diagnostics();
    assert_eq!(evaluation.len(), 1);
    assert_eq!(evaluation[0].kind, "native_sentinel");
    assert!(take_root_binding_diagnostics().is_empty());
    assert_eq!(sink.status().open_scopes, 0);
    assert!(sink.finish().terminal_written);
    let wire = event_file.records();
    let root_events: Vec<_> = wire
        .iter()
        .filter(|r| (500..=505).contains(&wire_stage(r)))
        .collect();
    assert_eq!(root_events.len(), 7);
    assert_eq!(
        root_events
            .iter()
            .map(|r| wire_stage(r))
            .collect::<Vec<_>>(),
        [500, 501, 502, 503, 504, 505, 505]
    );
    let identity = RootIdentity::new("report", RootKind::Expression);
    for event in &root_events {
        assert_eq!(wire_u64(event, 48), record.span.ordinal as u64);
        assert_eq!(wire_u64(event, 56), identity.hash);
        assert_eq!(wire_u64(event, 64), identity.utf8_len);
        assert_eq!(event[88], RootKind::Expression as u8);
    }
    assert_eq!(root_events[0][10], EventKind::Enter as u8);
    assert_eq!(root_events[6][10], EventKind::End as u8);
    assert_eq!(root_events[6][11], EventOutcome::Ok as u8);
    assert!(wire_u64(root_events[0], 24) >= record.span.start.wall_ns);
    assert!(wire_u64(root_events[6], 24) >= record.span.stages.last().unwrap().end.wall_ns);
}

#[test]
fn skipped_and_reused_roots_do_not_claim_source_or_binding_work() {
    take_root_binding_diagnostics();
    {
        let skipped = RootAttempt::new("document", 1, SourceKind::Doc);
        skipped.skipped();
    }
    {
        let reused = RootAttempt::new("design", 1, SourceKind::Expr);
        reused.dispatch_returned(Outcome::Bound);
        reused.body_returned(Completion::Published);
    }
    let records = take_root_binding_diagnostics();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].outcome, Outcome::Skipped);
    assert_eq!(records[0].dispatch_outcome, None);
    assert_eq!(records[1].outcome, Outcome::Bound);
    for record in &records {
        assert_eq!(
            (
                record.producer_calls,
                record.source_successes,
                record.binding_returns
            ),
            (0, 0, 0)
        );
        assert!(!names(record).contains(&"source_eval"));
        assert!(!names(record).contains(&"recursive_bind"));
        assert_adjacent(record);
    }
}

#[test]
fn partial_source_and_binding_failures_preserve_where_production_stopped() {
    take_root_binding_diagnostics();
    for (outcome, completion, source_ok) in [
        (Outcome::Defer, Completion::DeferredQueued, false),
        (Outcome::Defer, Completion::DeferIgnored, true),
        (Outcome::EvalError, Completion::EvalError, false),
        (Outcome::Taint, Completion::Taint, true),
    ] {
        let attempt = RootAttempt::new("result", 1, SourceKind::Expr);
        attempt.source_started();
        if source_ok {
            attempt.source_evaluated();
            attempt.binding_returned();
        }
        attempt.dispatch_returned(outcome);
        attempt.body_returned(completion);
    }
    let records = take_root_binding_diagnostics();
    assert_eq!(records.len(), 4);
    assert_eq!(records[0].completion, Some(Completion::DeferredQueued));
    assert_eq!(records[1].completion, Some(Completion::DeferIgnored));
    // The same initial phase is not used to infer whether a defer was queued.
    assert_eq!(records[0].initial_phase, records[1].initial_phase);
    for (index, record) in records.iter().enumerate() {
        let completed = usize::from(index % 2 == 1);
        assert_eq!(record.producer_calls, 1);
        assert_eq!(record.source_successes, completed);
        assert_eq!(record.binding_returns, completed);
        assert_eq!(record.interrupted_stage, None);
        assert_adjacent(record);
    }
}

#[test]
fn unwind_distinguishes_source_binding_handler_and_release_interruption() {
    take_root_binding_diagnostics();
    for point in 0..4 {
        assert!(catch_unwind(AssertUnwindSafe(|| {
            let attempt = RootAttempt::new("result", 2, SourceKind::Expr);
            attempt.source_started();
            if point >= 1 {
                attempt.source_evaluated();
            }
            if point >= 2 {
                attempt.binding_returned();
                attempt.dispatch_returned(Outcome::Bound);
            }
            if point >= 3 {
                attempt.body_returned(Completion::Published);
            }
            panic!("native boundary witness");
        }))
        .is_err());
    }
    let records = take_root_binding_diagnostics();
    assert_eq!(records.len(), 4);
    for (record, stage) in records.iter().zip([
        "source_eval",
        "recursive_bind",
        "publish_or_handle",
        "scope_release",
    ]) {
        assert_eq!(record.outcome, Outcome::Unwound);
        assert_eq!(record.interrupted_stage, Some(stage));
        assert_eq!(record.span.stages.last().unwrap().name, stage);
        assert_adjacent(record);
    }
    assert_eq!(records[0].dispatch_outcome, None);
    assert_eq!(records[3].dispatch_outcome, Some(Outcome::Bound));
    assert_eq!(records[3].completion, Some(Completion::Published));
}

#[test]
fn native_owner_release_can_reenter_and_drain_while_the_outer_attempt_is_live() {
    struct NativeOwner {
        token: Rc<()>,
        released: Rc<Cell<bool>>,
    }
    impl Drop for NativeOwner {
        fn drop(&mut self) {
            assert_eq!(Rc::strong_count(&self.token), 1);
            assert!(take_root_binding_diagnostics().is_empty());
            {
                let nested = RootAttempt::new("nested", 2, SourceKind::Doc);
                nested.skipped();
            }
            self.released.set(true);
        }
    }
    take_root_binding_diagnostics();
    let (event_file, file) = EventFile::create();
    let sink = phase_events::install(file, 64).unwrap();
    let released = Rc::new(Cell::new(false));
    let weak;
    {
        // Declaration order matches the production first-local guard contract.
        let outer = RootAttempt::new("outer", 2, SourceKind::Expr);
        let owner = NativeOwner {
            token: Rc::new(()),
            released: released.clone(),
        };
        weak = Rc::downgrade(&owner.token);
        outer.dispatch_returned(Outcome::Bound);
        outer.body_returned(Completion::Published);
    }
    assert!(released.get());
    assert!(weak.upgrade().is_none());
    let records = take_root_binding_diagnostics();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].root_name, "nested");
    assert_eq!(records[1].root_name, "outer");
    let release = records[1].span.stages.last().unwrap();
    assert_eq!(release.name, "scope_release");
    assert!(records[0].span.start.wall_ns >= release.start.wall_ns);
    assert!(records[0].span.stages.last().unwrap().end.wall_ns <= release.end.wall_ns);
    // Draining local RootRecords does not leave event scopes alive or append events.
    let status = sink.status();
    assert_eq!(status.open_scopes, 0);
    assert!(take_root_binding_diagnostics().is_empty());
    assert_eq!(sink.status().records_written, status.records_written);
    assert!(sink.finish().terminal_written);
    let wire = event_file.records();
    let enters: Vec<_> = wire
        .iter()
        .filter(|r| r[10] == EventKind::Enter as u8)
        .collect();
    assert_eq!(enters.len(), 2);
    assert_eq!(wire_u64(enters[0], 48), records[1].span.ordinal as u64);
    assert_eq!(wire_u64(enters[1], 48), records[0].span.ordinal as u64);
    assert_eq!(wire_u64(enters[1], 40), wire_u64(enters[0], 32));
    assert_eq!(enters[1][88], RootKind::Document as u8);
}

#[test]
fn root_name_capture_is_bounded_utf8_and_owns_no_source_string() {
    take_root_binding_diagnostics();
    let original = format!("{}한글", "x".repeat(ROOT_NAME_LIMIT - 1));
    let expected_bytes = original.len();
    let attempt = RootAttempt::new(&original, 1, SourceKind::Expr);
    drop(original);
    attempt.skipped();
    drop(attempt);
    let records = take_root_binding_diagnostics();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.root_name.len(), ROOT_NAME_LIMIT - 1);
    assert_eq!(record.root_name_original_bytes, expected_bytes);
    assert!(record.root_name_truncated);
    assert!(record.root_name.chars().all(|c| c == 'x'));
}

#[test]
fn event_endings_preserve_defer_errors_unwind_and_incomplete_outcomes() {
    take_root_binding_diagnostics();
    let (event_file, file) = EventFile::create();
    let sink = phase_events::install(file, 64).unwrap();
    for (outcome, completion) in [
        (Outcome::Defer, Completion::DeferredQueued),
        (Outcome::EvalError, Completion::EvalError),
        (Outcome::Taint, Completion::Taint),
    ] {
        let attempt = RootAttempt::new("result", 1, SourceKind::Expr);
        attempt.source_started();
        attempt.dispatch_returned(outcome);
        attempt.body_returned(completion);
    }
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let attempt = RootAttempt::new("result", 1, SourceKind::Expr);
        attempt.source_started();
        panic!("native source failure");
    }))
    .is_err());
    {
        let _incomplete = RootAttempt::new("result", 1, SourceKind::Expr);
    }
    let roots = take_root_binding_diagnostics();
    assert_eq!(roots.len(), 5);
    assert_eq!(sink.status().open_scopes, 0);
    assert!(sink.finish().terminal_written);
    let wire = event_file.records();
    let ends: Vec<_> = wire
        .iter()
        .filter(|r| r[10] == EventKind::End as u8)
        .collect();
    assert_eq!(ends.len(), 5);
    for ((event, root), outcome) in ends.iter().zip(&roots).zip([
        EventOutcome::Defer,
        EventOutcome::Eval,
        EventOutcome::Taint,
        EventOutcome::Unwound,
        EventOutcome::Incomplete,
    ]) {
        assert_eq!(wire_u64(event, 48), root.span.ordinal as u64);
        assert_eq!(event[11], outcome as u8);
    }
}
