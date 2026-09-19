//! Bounded, opt-in phase breadcrumbs which survive an interrupted evaluation.
//!
//! A caller supplies one empty, regular file and keeps its [`SinkGuard`] on the
//! evaluation thread. Each publication writes one fixed record directly, without
//! formatting, allocation-ledger access, or retained runtime values. This is
//! diagnostic I/O, not a source of uncontaminated latency measurements. Successful
//! writes reach the kernel; this module does not provide host-crash durability.
//!
//! Every record is [`RECORD_SIZE`] bytes, encoded little-endian: magic (0..8),
//! schema u16 (8..10), kind u8 (10), outcome u8 (11), stage u32 (12..16), sequence
//! u64 (16..24), monotonic ns u64 (24..32), span u64 (32..40), parent u64 (40..48),
//! ordinal u64 (48..56), root hash u64 (56..64), root UTF-8 length u64 (64..72),
//! installation generation u64 (72..80), process ID u32 (80..84), errno i32
//! (84..88), root kind u8 (88), stop reason u8 (89), two zero bytes (90..92), and
//! open-scope depth u32 (92..96). Header ordinal holds the record limit instead.
//! Stages are caller-defined IDs whose dictionary must accompany the file.
//!
//! A complete prefix confirms published boundaries only. It cannot identify the
//! exact instruction at process termination. Missing terminal records, partial
//! tails, and reported publication failures must remain distinguishable from a
//! successfully closed stream. The final record slot is reserved for a terminal.

use serde::Serialize;
use std::cell::Cell;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::marker::PhantomData;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Magic bytes repeated at the start of every record.
pub const MAGIC: [u8; 8] = *b"DCLPHEV1";
/// Version of the fixed little-endian record layout.
pub const SCHEMA_VERSION: u16 = 1;
/// Bytes per record, including its schema and magic.
pub const RECORD_SIZE: usize = 96;
/// Hard limit including the header and reserved terminal: at most 1.5 MiB.
pub const MAX_RECORDS: u64 = 16_384;
const EINTR_RETRIES: u32 = 3;

/// Wire record category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[repr(u8)]
pub enum EventKind {
    /// Installation header; its ordinal contains the configured record limit.
    Header = 1,
    /// Explicit entry into a new, uniquely numbered nested attempt.
    Enter = 2,
    /// Explicit start of a new stage within an existing attempt.
    Transition = 3,
    /// End of a scope; the outcome describes how it ended.
    End = 4,
    /// Last record of a closed or intentionally truncated stream.
    Terminal = 5,
}

/// Caller outcome or guard/stream termination classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[repr(u8)]
pub enum Outcome {
    /// No ending outcome applies to this event.
    None = 0,
    /// Caller explicitly completed the scope, or explicitly closed the sink.
    Ok = 1,
    /// Caller reported deferred work.
    Defer = 2,
    /// Caller reported a tainted result.
    Taint = 3,
    /// Caller reported an evaluation failure.
    Eval = 4,
    /// Caller rejected the attempted operation.
    Rejected = 5,
    /// A scope was dropped without an explicit outcome.
    Incomplete = 6,
    /// A scope or sink ended while its thread was unwinding.
    Unwound = 7,
    /// The configured record limit ended publication.
    Truncated = 8,
    /// A sink was dropped without explicit closure.
    Abandoned = 9,
}

/// Origin category for a root's scalar identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[repr(u8)]
pub enum RootKind {
    /// No root identity was supplied.
    Unknown = 0,
    /// Root production evaluates a source expression.
    Expression = 1,
    /// Root production binds a supplied document.
    Document = 2,
}

/// Scalar root label, with no borrowed text or runtime ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RootIdentity {
    /// FNV-1a 64-bit hash of the UTF-8 name; not a collision-free identifier.
    pub hash: u64,
    /// Number of UTF-8 bytes hashed.
    pub utf8_len: u64,
    /// Expression, document, or unspecified origin.
    pub kind: RootKind,
}
impl RootIdentity {
    /// Unspecified root. An actual empty name instead has the FNV offset hash.
    pub const NONE: Self = Self {
        hash: 0,
        utf8_len: 0,
        kind: RootKind::Unknown,
    };

    /// Hash a name without retaining it or allocating; dictionaries must check
    /// collisions before treating hash and byte length as exact name identity.
    pub fn new(name: &str, kind: RootKind) -> Self {
        let mut hash = 14_695_981_039_346_656_037_u64;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        Self {
            hash,
            utf8_len: name.len() as u64,
            kind,
        }
    }
}

/// First reason publication was disabled; language evaluation is unaffected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[repr(u8)]
pub enum StopReason {
    /// No telemetry failure; an explicit terminal may still have closed it.
    None = 0,
    /// The bounded stream used its reserved terminal slot.
    Cap = 1,
    /// The monotonic clock failed, overflowed, or moved backwards.
    Clock = 2,
    /// A write failed with an error other than an exhausted EINTR allowance.
    Write = 3,
    /// A write produced fewer bytes than one complete record.
    ShortWrite = 4,
    /// EINTR persisted beyond the bounded retry allowance.
    Interrupted = 5,
    /// A scope transitioned or ended while another scope was current.
    ScopeOrder = 6,
    /// Publication was recursively attempted while already publishing.
    Reentrant = 7,
    /// The sink guard was dropped without explicit closure.
    Abandoned = 8,
    /// Explicit sink closure occurred with scopes still open.
    OpenScopes = 9,
    /// A sequence, scope ID, or depth counter could not be advanced safely.
    CounterOverflow = 10,
}

/// Scalar snapshot of one installation, serializable after measured work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Status {
    /// Unique process-local installation ID, also carried in every record.
    pub generation: u64,
    /// Configured count including header and terminal.
    pub max_records: u64,
    /// Complete records successfully written; failed partial tails are excluded.
    pub records_written: u64,
    /// Actual bytes written, including any partial final record.
    pub bytes_written: u64,
    /// Last successfully written sequence, zero before the header succeeds.
    pub last_sequence: u64,
    /// Most recently published monotonic clock sample, or zero initially.
    pub last_monotonic_ns: u64,
    /// Whether another nonterminal publication is currently allowed.
    pub enabled: bool,
    /// Whether a complete terminal record was successfully written.
    pub terminal_written: bool,
    /// Open scopes at the last successful structural boundary.
    pub open_scopes: u32,
    /// Reason publication stopped; a failed terminal reports its publication
    /// failure instead of the intended cap, abandonment, or open-scope reason.
    pub stop_reason: StopReason,
    /// OS error associated with a clock or write failure, otherwise zero.
    pub errno: i32,
}

/// Installation failure before an owning sink guard can be returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallError {
    /// The requested count is outside 2..=MAX_RECORDS.
    InvalidLimit,
    /// This thread already has an installed sink, including a disabled one.
    AlreadyInstalled,
    /// Installation was attempted recursively during publication.
    Busy,
    /// Thread-local state was no longer accessible.
    ThreadUnavailable,
    /// The supplied file was not an empty regular file.
    NotEmptyRegularFile,
    /// File metadata or initial seeking failed with this OS error.
    File(i32),
    /// All process-local installation IDs were consumed.
    GenerationExhausted,
}

#[derive(Clone, Copy)]
struct Payload {
    kind: EventKind,
    outcome: Outcome,
    stage: u32,
    span: u64,
    parent: u64,
    ordinal: u64,
    root: RootIdentity,
    depth: u32,
}
#[derive(Clone, Copy)]
struct State {
    fd: i32,
    pid: u32,
    status: Status,
    next_span: u64,
    current_span: u64,
}
struct Local {
    state: Cell<Option<State>>,
    busy: Cell<bool>,
    reentered: Cell<bool>,
}
thread_local! {
    static LOCAL: Local = const { Local {
        state: Cell::new(None), busy: Cell::new(false), reentered: Cell::new(false),
    } };
}
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn os_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}
fn clock_ns() -> Result<u64, i32> {
    #[cfg(test)]
    if test_io::clock_fails() {
        return Err(libc::EIO);
    }
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: writable local timespec and the supported monotonic clock ID.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return Err(os_errno());
    }
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        return Err(0);
    }
    (ts.tv_sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(ts.tv_nsec as u64))
        .ok_or(0)
}
fn write_record(fd: i32, record: &[u8; RECORD_SIZE]) -> Result<usize, i32> {
    #[cfg(test)]
    if let Some(result) = test_io::intercept(fd, record) {
        return result;
    }
    // SAFETY: the owning !Send SinkGuard keeps this descriptor open on this
    // thread. No user callback runs while this local byte array is borrowed.
    let written = unsafe { libc::write(fd, record.as_ptr().cast(), RECORD_SIZE) };
    if written < 0 {
        Err(os_errno())
    } else {
        Ok(written as usize)
    }
}
fn encode(state: &State, p: Payload, time: u64) -> [u8; RECORD_SIZE] {
    let mut out = [0_u8; RECORD_SIZE];
    out[..8].copy_from_slice(&MAGIC);
    out[8..10].copy_from_slice(&SCHEMA_VERSION.to_le_bytes());
    out[10] = p.kind as u8;
    out[11] = p.outcome as u8;
    out[12..16].copy_from_slice(&p.stage.to_le_bytes());
    out[16..24].copy_from_slice(&(state.status.records_written + 1).to_le_bytes());
    out[24..32].copy_from_slice(&time.to_le_bytes());
    out[32..40].copy_from_slice(&p.span.to_le_bytes());
    out[40..48].copy_from_slice(&p.parent.to_le_bytes());
    out[48..56].copy_from_slice(&p.ordinal.to_le_bytes());
    out[56..64].copy_from_slice(&p.root.hash.to_le_bytes());
    out[64..72].copy_from_slice(&p.root.utf8_len.to_le_bytes());
    out[72..80].copy_from_slice(&state.status.generation.to_le_bytes());
    out[80..84].copy_from_slice(&state.pid.to_le_bytes());
    out[84..88].copy_from_slice(&state.status.errno.to_le_bytes());
    out[88] = p.root.kind as u8;
    out[89] = state.status.stop_reason as u8;
    out[92..96].copy_from_slice(&p.depth.to_le_bytes());
    out
}
impl State {
    fn stop(&mut self, reason: StopReason, errno: i32) {
        if matches!(
            self.status.stop_reason,
            StopReason::None | StopReason::Cap | StopReason::Abandoned | StopReason::OpenScopes
        ) {
            self.status.stop_reason = reason;
            self.status.errno = errno;
        }
        self.status.enabled = false;
    }
    fn terminal(&self, outcome: Outcome) -> Payload {
        Payload {
            kind: EventKind::Terminal,
            outcome,
            stage: 0,
            span: self.current_span,
            parent: 0,
            ordinal: 0,
            root: RootIdentity::NONE,
            depth: self.status.open_scopes,
        }
    }
    fn publish(&mut self, payload: Payload) -> bool {
        if !self.status.enabled {
            return false;
        }
        if payload.kind != EventKind::Terminal
            && self.status.records_written >= self.status.max_records - 1
        {
            self.status.stop_reason = StopReason::Cap;
            self.publish(self.terminal(Outcome::Truncated));
            self.status.enabled = false;
            return false;
        }
        if self.status.records_written >= self.status.max_records {
            self.stop(StopReason::Cap, 0);
            return false;
        }
        let time = match clock_ns() {
            Ok(time) if time >= self.status.last_monotonic_ns => time,
            Ok(_) => {
                self.stop(StopReason::Clock, 0);
                return false;
            }
            Err(errno) => {
                self.stop(StopReason::Clock, errno);
                return false;
            }
        };
        let record = encode(self, payload, time);
        let mut retries = 0;
        loop {
            match write_record(self.fd, &record) {
                Ok(bytes) => {
                    self.status.bytes_written += bytes as u64;
                    if bytes != RECORD_SIZE {
                        self.stop(StopReason::ShortWrite, 0);
                        return false;
                    }
                    self.status.records_written += 1;
                    self.status.last_sequence = self.status.records_written;
                    self.status.last_monotonic_ns = time;
                    self.status.terminal_written = payload.kind == EventKind::Terminal;
                    if self.status.terminal_written {
                        self.status.enabled = false;
                    }
                    return true;
                }
                Err(errno) if errno == libc::EINTR && retries < EINTR_RETRIES => retries += 1,
                Err(errno) => {
                    self.stop(
                        if errno == libc::EINTR {
                            StopReason::Interrupted
                        } else {
                            StopReason::Write
                        },
                        errno,
                    );
                    return false;
                }
            }
        }
    }
}

fn with_state<T>(generation: Option<u64>, f: impl FnOnce(&mut State) -> T) -> Option<T> {
    LOCAL
        .try_with(|local| {
            if local.busy.replace(true) {
                local.reentered.set(true);
                return None;
            }
            let result = local.state.get().and_then(|mut state| {
                if generation.is_some_and(|id| id != state.status.generation) {
                    return None;
                }
                let result = f(&mut state);
                if local.reentered.replace(false) {
                    state.stop(StopReason::Reentrant, 0);
                }
                // Detachment always wins, including an out-of-order guard drop.
                if local
                    .state
                    .get()
                    .is_some_and(|s| s.status.generation == state.status.generation)
                {
                    local.state.set(Some(state));
                }
                Some(result)
            });
            local.busy.set(false);
            result
        })
        .ok()
        .flatten()
}

/// An owning, thread-bound installation. Drop detaches TLS before closing the
/// file; surviving scopes cannot write to its descriptor or a later installation.
pub struct SinkGuard {
    file: File,
    generation: u64,
    last_status: Cell<Status>,
    attached: Cell<bool>,
    thread_bound: PhantomData<Rc<()>>,
}

/// Install an owned empty regular file on this thread, then attempt its header.
///
/// The limit includes header and terminal. Nested installation is rejected.
/// Header I/O or clock failure returns an installed but disabled guard, exposing
/// the failure through [`SinkGuard::status`]; it does not fail evaluation.
/// Callers must ensure no other file handle writes to the same file.
pub fn install(mut file: File, max_records: u64) -> Result<SinkGuard, InstallError> {
    if !(2..=MAX_RECORDS).contains(&max_records) {
        return Err(InstallError::InvalidLimit);
    }
    LOCAL
        .try_with(|local| {
            if local.busy.get() {
                return Err(InstallError::Busy);
            }
            if local.state.get().is_some() {
                return Err(InstallError::AlreadyInstalled);
            }
            let metadata = file
                .metadata()
                .map_err(|e| InstallError::File(e.raw_os_error().unwrap_or(0)))?;
            if !metadata.is_file() || metadata.len() != 0 {
                return Err(InstallError::NotEmptyRegularFile);
            }
            file.seek(SeekFrom::Start(0))
                .map_err(|e| InstallError::File(e.raw_os_error().unwrap_or(0)))?;
            let generation = NEXT_GENERATION
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
                .map_err(|_| InstallError::GenerationExhausted)?;
            let status = Status {
                generation,
                max_records,
                records_written: 0,
                bytes_written: 0,
                last_sequence: 0,
                last_monotonic_ns: 0,
                enabled: true,
                terminal_written: false,
                open_scopes: 0,
                stop_reason: StopReason::None,
                errno: 0,
            };
            let state = State {
                fd: file.as_raw_fd(),
                pid: std::process::id(),
                status,
                next_span: 1,
                current_span: 0,
            };
            local.state.set(Some(state));
            let guard = SinkGuard {
                file,
                generation,
                last_status: Cell::new(status),
                attached: Cell::new(true),
                thread_bound: PhantomData,
            };
            with_state(Some(generation), |s| {
                s.publish(Payload {
                    kind: EventKind::Header,
                    outcome: Outcome::None,
                    stage: 0,
                    span: 0,
                    parent: 0,
                    ordinal: max_records,
                    root: RootIdentity::NONE,
                    depth: 0,
                })
            });
            Ok(guard)
        })
        .unwrap_or(Err(InstallError::ThreadUnavailable))
}
impl SinkGuard {
    /// Copy current scalar status without publishing or retaining runtime data.
    pub fn status(&self) -> Status {
        if let Some(status) = with_state(Some(self.generation), |s| s.status) {
            self.last_status.set(status);
        }
        self.last_status.get()
    }
    fn close(&self, explicit: bool) -> Status {
        if self.attached.replace(false) {
            with_state(Some(self.generation), |s| {
                if s.status.enabled {
                    let outcome = if !explicit {
                        s.status.stop_reason = StopReason::Abandoned;
                        if std::thread::panicking() {
                            Outcome::Unwound
                        } else {
                            Outcome::Abandoned
                        }
                    } else if s.status.open_scopes != 0 {
                        s.status.stop_reason = StopReason::OpenScopes;
                        Outcome::Incomplete
                    } else {
                        Outcome::Ok
                    };
                    s.publish(s.terminal(outcome));
                    s.status.enabled = false;
                }
                self.last_status.set(s.status);
            });
            let _ = LOCAL.try_with(|local| {
                if local
                    .state
                    .get()
                    .is_some_and(|s| s.status.generation == self.generation)
                {
                    local.state.set(None);
                }
            });
        }
        self.last_status.get()
    }
    /// Attempt the terminal, detach, and close the owned file. No fsync occurs.
    /// An open scope makes closure explicitly incomplete; publication failures
    /// leave their last valid prefix and scalar failure status intact.
    pub fn finish(self) -> Status {
        self.close(true)
    }
}
impl Drop for SinkGuard {
    fn drop(&mut self) {
        self.close(false);
        // The owned File's normal field drop follows detachment, and never
        // invokes user evaluation code.
        let _ = &self.file;
    }
}

/// One thread-bound nested attempt. It retains scalar labels only. End and
/// transition methods borrow immutably, allowing use inside native callbacks.
pub struct EventScope {
    generation: u64,
    span: u64,
    parent: u64,
    ordinal: u64,
    root: RootIdentity,
    stage: Cell<u32>,
    active: Cell<bool>,
    thread_bound: PhantomData<Rc<()>>,
}
impl EventScope {
    /// Publish an explicit entry, or return an inert guard when no sink is active.
    /// The ordinal is caller metadata; each attempt receives a separate span ID.
    pub fn begin(stage: u32, ordinal: u64, root: RootIdentity) -> Self {
        let ids = with_state(None, |s| {
            if !s.status.enabled {
                return None;
            }
            let Some(next_span) = s.next_span.checked_add(1) else {
                s.stop(StopReason::CounterOverflow, 0);
                return None;
            };
            let Some(depth) = s.status.open_scopes.checked_add(1) else {
                s.stop(StopReason::CounterOverflow, 0);
                return None;
            };
            let span = s.next_span;
            let parent = s.current_span;
            if !s.publish(Payload {
                kind: EventKind::Enter,
                outcome: Outcome::None,
                stage,
                span,
                parent,
                ordinal,
                root,
                depth,
            }) {
                return None;
            }
            s.next_span = next_span;
            s.current_span = span;
            s.status.open_scopes = depth;
            Some((s.status.generation, span, parent))
        })
        .flatten();
        let (generation, span, parent) = ids.unwrap_or((0, 0, 0));
        Self {
            generation,
            span,
            parent,
            ordinal,
            root,
            stage: Cell::new(stage),
            active: Cell::new(ids.is_some()),
            thread_bound: PhantomData,
        }
    }
    /// Publish entry into the next stage. This labels subsequent work, not the
    /// interval just completed. A noncurrent scope disables telemetry.
    pub fn transition(&self, stage: u32) {
        if !self.active.get() {
            return;
        }
        let published = with_state(Some(self.generation), |s| {
            if !s.status.enabled {
                return false;
            }
            if s.current_span != self.span {
                s.stop(StopReason::ScopeOrder, 0);
                return false;
            }
            s.publish(self.payload(
                EventKind::Transition,
                Outcome::None,
                stage,
                s.status.open_scopes,
            ))
        })
        .unwrap_or(false);
        if published {
            self.stage.set(stage);
        }
    }
    fn payload(&self, kind: EventKind, outcome: Outcome, stage: u32, depth: u32) -> Payload {
        Payload {
            kind,
            outcome,
            stage,
            span: self.span,
            parent: self.parent,
            ordinal: self.ordinal,
            root: self.root,
            depth,
        }
    }
    /// Publish the explicit outcome once and restore the parent as current.
    /// Repeated calls and drops after this call have no effect.
    pub fn finish(&self, outcome: Outcome) {
        if !self.active.replace(false) {
            return;
        }
        with_state(Some(self.generation), |s| {
            if !s.status.enabled {
                return;
            }
            if s.current_span != self.span || s.status.open_scopes == 0 {
                s.stop(StopReason::ScopeOrder, 0);
                return;
            }
            let depth = s.status.open_scopes - 1;
            if s.publish(self.payload(EventKind::End, outcome, self.stage.get(), depth)) {
                s.current_span = self.parent;
                s.status.open_scopes = depth;
            }
        });
    }
}
impl Drop for EventScope {
    fn drop(&mut self) {
        self.finish(if std::thread::panicking() {
            Outcome::Unwound
        } else {
            Outcome::Incomplete
        });
    }
}

#[cfg(test)]
#[path = "../tests/private/phase_events_test.rs"]
mod tests;

#[cfg(test)]
mod test_io {
    use super::*;
    #[derive(Clone, Copy)]
    pub(super) enum Mode {
        Real,
        Short(usize),
        Error(i32, u32),
        Reenter,
    }
    thread_local! {
        pub(super) static MODE: Cell<Mode> = const { Cell::new(Mode::Real) };
        pub(super) static CLOCK_FAIL: Cell<bool> = const { Cell::new(false) };
    }
    pub(super) fn clock_fails() -> bool {
        CLOCK_FAIL.get()
    }
    pub(super) fn intercept(fd: i32, record: &[u8; RECORD_SIZE]) -> Option<Result<usize, i32>> {
        match MODE.get() {
            Mode::Real => None,
            Mode::Short(bytes) => {
                MODE.set(Mode::Real);
                let bytes = bytes.min(RECORD_SIZE - 1);
                // SAFETY: same owned descriptor and array as the production call;
                // this test-only branch deliberately creates a partial tail.
                let n = unsafe { libc::write(fd, record.as_ptr().cast(), bytes) };
                Some(if n < 0 {
                    Err(os_errno())
                } else {
                    Ok(n as usize)
                })
            }
            Mode::Error(errno, remaining) => {
                MODE.set(if remaining <= 1 {
                    Mode::Real
                } else {
                    Mode::Error(errno, remaining - 1)
                });
                Some(Err(errno))
            }
            Mode::Reenter => {
                MODE.set(Mode::Real);
                let _scope = EventScope::begin(999, 0, RootIdentity::NONE);
                None
            }
        }
    }
}
