use super::*;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;

static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

struct TempFile(PathBuf);
impl TempFile {
    fn new() -> (Self, File) {
        let path = std::env::temp_dir().join(format!(
            "decl-phase-events-{}-{}.bin",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        (Self(path), file)
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.0).unwrap()
    }
    fn records(&self) -> Vec<[u8; RECORD_SIZE]> {
        let bytes = self.bytes();
        assert_eq!(bytes.len() % RECORD_SIZE, 0);
        bytes
            .chunks_exact(RECORD_SIZE)
            .map(|v| v.try_into().unwrap())
            .collect()
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        test_io::MODE.set(test_io::Mode::Real);
        test_io::CLOCK_FAIL.set(false);
        let _ = fs::remove_file(&self.0);
    }
}
fn u64_at(record: &[u8; RECORD_SIZE], at: usize) -> u64 {
    u64::from_le_bytes(record[at..at + 8].try_into().unwrap())
}
fn u32_at(record: &[u8; RECORD_SIZE], at: usize) -> u32 {
    u32::from_le_bytes(record[at..at + 4].try_into().unwrap())
}

#[test]
fn records_preserve_explicit_nested_stages_root_identity_and_lifo_parent() {
    let (file, handle) = TempFile::new();
    let sink = install(handle, 32).unwrap();
    let root = RootIdentity::new("가😀", RootKind::Document);
    let outer = EventScope::begin(10, 7, root);
    let inner = EventScope::begin(20, 7, RootIdentity::NONE);
    inner.transition(21);
    inner.finish(Outcome::Defer);
    inner.finish(Outcome::Ok);
    outer.transition(11);
    outer.finish(Outcome::Ok);
    let status = sink.finish();
    assert_eq!(status.stop_reason, StopReason::None);
    assert!(status.terminal_written);
    assert!(!status.enabled);
    assert_eq!(status.open_scopes, 0);
    assert_eq!(status.records_written, 8);
    assert_eq!(status.bytes_written, 8 * RECORD_SIZE as u64);
    let records = file.records();
    assert_eq!(
        records.iter().map(|r| r[10]).collect::<Vec<_>>(),
        vec![1, 2, 2, 3, 4, 3, 4, 5]
    );
    for (i, record) in records.iter().enumerate() {
        assert_eq!(&record[..8], &MAGIC);
        assert_eq!(&record[8..10], &SCHEMA_VERSION.to_le_bytes());
        assert_eq!(u64_at(record, 16), i as u64 + 1);
        assert_eq!(u64_at(record, 72), status.generation);
        assert_eq!(u32_at(record, 80), std::process::id());
        assert_eq!(&record[90..92], &[0, 0]);
        if i != 0 {
            assert!(u64_at(record, 24) >= u64_at(&records[i - 1], 24));
        }
    }
    assert_eq!(u64_at(&records[0], 48), 32);
    let outer_id = u64_at(&records[1], 32);
    let inner_id = u64_at(&records[2], 32);
    assert_ne!(outer_id, inner_id);
    assert_eq!(u64_at(&records[1], 40), 0);
    assert_eq!(u64_at(&records[2], 40), outer_id);
    assert_eq!(u64_at(&records[4], 40), outer_id);
    assert_eq!(records[4][11], Outcome::Defer as u8);
    assert_eq!(u32_at(&records[3], 12), 21);
    assert_eq!(u32_at(&records[5], 12), 11);
    assert_eq!(u64_at(&records[1], 56), root.hash);
    assert_eq!(u64_at(&records[1], 64), 7);
    assert_eq!(records[1][88], RootKind::Document as u8);
    assert_eq!(u32_at(&records[2], 92), 2);
    assert_eq!(u32_at(&records[4], 92), 1);
    assert_eq!(u32_at(&records[6], 92), 0);
    assert_eq!(records[7][11], Outcome::Ok as u8);
    assert_eq!(
        RootIdentity::new("", RootKind::Expression).hash,
        0xcbf2_9ce4_8422_2325
    );
    assert_eq!(
        RootIdentity::new("a", RootKind::Expression).hash,
        0xaf63_dc4c_8601_ec8c
    );
    assert_ne!(RootIdentity::new("", RootKind::Unknown), RootIdentity::NONE);
}

#[test]
fn unwind_and_unfinished_scope_ends_do_not_claim_normal_completion() {
    let (file, handle) = TempFile::new();
    let sink = install(handle, 32).unwrap();
    let caught = std::panic::catch_unwind(|| {
        let _outer = EventScope::begin(1, 0, RootIdentity::NONE);
        let _inner = EventScope::begin(2, 0, RootIdentity::NONE);
        panic!("synthetic phase unwind");
    });
    assert!(caught.is_err());
    drop(EventScope::begin(3, 1, RootIdentity::NONE));
    let status = sink.finish();
    assert_eq!(status.open_scopes, 0);
    assert_eq!(status.stop_reason, StopReason::None);
    let ends: Vec<_> = file
        .records()
        .into_iter()
        .filter(|r| r[10] == EventKind::End as u8)
        .collect();
    assert_eq!(
        ends.iter().map(|r| r[11]).collect::<Vec<_>>(),
        vec![
            Outcome::Unwound as u8,
            Outcome::Unwound as u8,
            Outcome::Incomplete as u8
        ]
    );
    assert_eq!(u32_at(&ends[0], 12), 2);
    assert_eq!(u32_at(&ends[1], 12), 1);
}

#[test]
fn capped_stream_reserves_terminal_and_never_exceeds_record_budget() {
    let (file, handle) = TempFile::new();
    let sink = install(handle, 3).unwrap();
    let scope = EventScope::begin(1, 0, RootIdentity::NONE);
    scope.transition(2);
    for _ in 0..20 {
        scope.transition(3);
    }
    scope.finish(Outcome::Ok);
    let status = sink.finish();
    assert_eq!(status.stop_reason, StopReason::Cap);
    assert!(status.terminal_written);
    assert_eq!(status.records_written, 3);
    assert_eq!(status.bytes_written, 3 * RECORD_SIZE as u64);
    let records = file.records();
    assert_eq!(records[2][10], EventKind::Terminal as u8);
    assert_eq!(records[2][11], Outcome::Truncated as u8);
    assert_eq!(records[2][89], StopReason::Cap as u8);
    assert_eq!(u32_at(&records[2], 92), 1);
}

#[test]
fn partial_write_disables_stream_and_preserves_a_detectable_partial_tail() {
    let (file, handle) = TempFile::new();
    let sink = install(handle, 16).unwrap();
    test_io::MODE.set(test_io::Mode::Short(17));
    let scope = EventScope::begin(1, 0, RootIdentity::NONE);
    scope.transition(2);
    scope.finish(Outcome::Ok);
    let status = sink.finish();
    assert_eq!(status.stop_reason, StopReason::ShortWrite);
    assert_eq!(status.records_written, 1);
    assert_eq!(status.bytes_written, RECORD_SIZE as u64 + 17);
    assert!(!status.terminal_written);
    assert_eq!(file.bytes().len(), RECORD_SIZE + 17);
}

#[test]
fn failed_reserved_terminal_exposes_io_failure_instead_of_claiming_a_cap_record() {
    let (file, handle) = TempFile::new();
    let sink = install(handle, 2).unwrap();
    test_io::MODE.set(test_io::Mode::Error(libc::ENOSPC, 1));
    let _scope = EventScope::begin(1, 0, RootIdentity::NONE);
    let status = sink.finish();
    assert_eq!(status.stop_reason, StopReason::Write);
    assert_eq!(status.errno, libc::ENOSPC);
    assert!(!status.terminal_written);
    assert_eq!(status.records_written, 1);
    assert_eq!(file.records().len(), 1);
}

#[test]
fn eintr_is_retried_only_within_the_fixed_allowance() {
    let (file, handle) = TempFile::new();
    let sink = install(handle, 16).unwrap();
    test_io::MODE.set(test_io::Mode::Error(libc::EINTR, EINTR_RETRIES));
    let scope = EventScope::begin(1, 0, RootIdentity::NONE);
    assert_eq!(sink.status().records_written, 2);
    test_io::MODE.set(test_io::Mode::Error(libc::EINTR, EINTR_RETRIES + 1));
    scope.transition(2);
    let status = sink.finish();
    assert_eq!(status.stop_reason, StopReason::Interrupted);
    assert_eq!(status.errno, libc::EINTR);
    assert!(!status.terminal_written);
    assert_eq!(file.records().len(), 2);
}

#[test]
fn clock_and_write_failures_are_scalar_failures_without_a_false_terminal() {
    let (file, handle) = TempFile::new();
    test_io::CLOCK_FAIL.set(true);
    let sink = install(handle, 16).unwrap();
    test_io::CLOCK_FAIL.set(false);
    let status = sink.finish();
    assert_eq!(status.stop_reason, StopReason::Clock);
    assert_eq!(status.errno, libc::EIO);
    assert_eq!(status.records_written, 0);
    assert!(file.bytes().is_empty());
    let (file2, handle2) = TempFile::new();
    let sink2 = install(handle2, 16).unwrap();
    test_io::MODE.set(test_io::Mode::Error(libc::ENOSPC, 1));
    let _scope = EventScope::begin(1, 0, RootIdentity::NONE);
    let status2 = sink2.finish();
    assert_eq!(status2.stop_reason, StopReason::Write);
    assert_eq!(status2.errno, libc::ENOSPC);
    assert_eq!(file2.records().len(), 1);
    assert!(!status2.terminal_written);
}

#[test]
fn old_scopes_cannot_write_after_guard_drop_or_into_a_reinstalled_sink() {
    let inactive = EventScope::begin(90, 0, RootIdentity::NONE);
    let (file_a, handle_a) = TempFile::new();
    let sink_a = install(handle_a, 16).unwrap();
    let old_generation = sink_a.status().generation;
    let old_scope = EventScope::begin(1, 0, RootIdentity::NONE);
    let (nested_file, nested_handle) = TempFile::new();
    assert!(matches!(
        install(nested_handle, 16),
        Err(InstallError::AlreadyInstalled)
    ));
    assert!(nested_file.bytes().is_empty());
    drop(sink_a);
    let old_bytes = file_a.bytes();
    let (file_b, handle_b) = TempFile::new();
    let sink_b = install(handle_b, 16).unwrap();
    assert_ne!(sink_b.status().generation, old_generation);
    old_scope.transition(2);
    old_scope.finish(Outcome::Ok);
    inactive.finish(Outcome::Ok);
    let current = EventScope::begin(3, 0, RootIdentity::NONE);
    current.finish(Outcome::Ok);
    let status = sink_b.finish();
    assert_eq!(status.records_written, 4);
    assert_eq!(status.stop_reason, StopReason::None);
    assert_eq!(file_a.bytes(), old_bytes);
    let records = file_a.records();
    assert_eq!(records.last().unwrap()[11], Outcome::Abandoned as u8);
    assert_eq!(records.last().unwrap()[89], StopReason::Abandoned as u8);
    assert_eq!(file_b.records()[1][12], 3);
}

#[test]
fn closing_with_live_scopes_and_non_lifo_use_are_explicitly_incomplete() {
    let (file, handle) = TempFile::new();
    let sink = install(handle, 16).unwrap();
    let scope = EventScope::begin(1, 0, RootIdentity::NONE);
    let status = sink.finish();
    scope.finish(Outcome::Ok);
    assert_eq!(status.stop_reason, StopReason::OpenScopes);
    assert!(status.terminal_written);
    assert_eq!(
        file.records().last().unwrap()[11],
        Outcome::Incomplete as u8
    );
    let (file2, handle2) = TempFile::new();
    let sink2 = install(handle2, 16).unwrap();
    let outer = EventScope::begin(1, 0, RootIdentity::NONE);
    let inner = EventScope::begin(2, 0, RootIdentity::NONE);
    outer.transition(3);
    inner.finish(Outcome::Ok);
    let status2 = sink2.finish();
    assert_eq!(status2.stop_reason, StopReason::ScopeOrder);
    assert!(!status2.terminal_written);
    assert_eq!(file2.records().len(), 3);
}

#[test]
fn reentrant_publication_is_disabled_without_borrow_panics_or_nested_writes() {
    let (file, handle) = TempFile::new();
    let sink = install(handle, 16).unwrap();
    test_io::MODE.set(test_io::Mode::Reenter);
    let _scope = EventScope::begin(1, 0, RootIdentity::NONE);
    let status = sink.finish();
    assert_eq!(status.stop_reason, StopReason::Reentrant);
    assert!(!status.terminal_written);
    assert_eq!(file.records().len(), 2);
}

#[test]
fn invalid_limits_and_nonempty_files_do_not_install_a_sink() {
    let (file, handle) = TempFile::new();
    assert!(matches!(
        install(handle, 1),
        Err(InstallError::InvalidLimit)
    ));
    assert!(file.bytes().is_empty());
    fs::write(&file.0, b"existing").unwrap();
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&file.0)
        .unwrap();
    assert!(matches!(
        install(handle, 16),
        Err(InstallError::NotEmptyRegularFile)
    ));
    assert_eq!(file.bytes(), b"existing");
    let (file2, handle2) = TempFile::new();
    assert!(matches!(
        install(handle2, MAX_RECORDS + 1),
        Err(InstallError::InvalidLimit)
    ));
    assert!(file2.bytes().is_empty());
    drop(EventScope::begin(1, 0, RootIdentity::NONE));
}
