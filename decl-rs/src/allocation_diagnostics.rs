//! Rust requested-allocation counters for separate diagnostic helpers.
//! Counts requests to MiMalloc, not its capacity, C allocations or RSS.
//! This module is compiled only with `runtime-diagnostics`; feature-empty native
//! builds do not include these counters.
//!
//! Counters are cumulative across all threads and all [`CountingAllocator`]
//! instances; they are not reset by taking a snapshot. Each field is read with
//! relaxed atomics, so snapshots are not transactions across concurrent calls.
//! Compare ledger identities only at endpoints with other allocator users idle.
use serde::Serialize;
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static REALLOCS: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);
static FREED: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
static FAILURES: AtomicU64 = AtomicU64::new(0);
fn grow(bytes: u64) {
    let n = LIVE.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(n, Relaxed);
}
/// Requested-allocation ledger read from the process-wide counters.
///
/// A default value is an all-zero value, not a read of the current counters.
/// Individual fields can reflect different instants while allocations continue.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Snapshot {
    /// Successful `alloc` and `alloc_zeroed` calls; excludes reallocations.
    pub alloc_calls: u64,
    /// Successful reallocations, including those that retain the same address.
    pub realloc_calls: u64,
    /// Explicit `dealloc` calls; excludes the old side of a reallocation.
    pub free_calls: u64,
    /// Cumulative requested bytes of successful allocations and new realloc sizes.
    pub allocated_bytes: u64,
    /// Cumulative explicit deallocation sizes and old sizes of successful reallocations.
    pub freed_bytes: u64,
    /// Net requested bytes still accounted as live; excludes allocator rounding.
    pub live_bytes: u64,
    /// Largest net live-byte count recorded since counting began, across all threads.
    /// This is not a phase-local peak or the allocator's transient old/new
    /// storage overlap during a reallocation.
    pub global_peak_bytes: u64,
    /// Allocation or reallocation calls that returned a null pointer.
    /// Such calls do not update the successful-call or byte counters.
    pub failures: u64,
}
/// Read the cumulative ledger without allocation or resetting any counter.
///
/// Loads are individually atomic but not a coherent multi-field transaction.
/// With allocator activity quiescent, `allocated_bytes - freed_bytes` equals
/// `live_bytes`. Concurrent calls can temporarily violate that identity in a read.
pub fn snapshot() -> Snapshot {
    Snapshot {
        alloc_calls: ALLOCS.load(Relaxed),
        realloc_calls: REALLOCS.load(Relaxed),
        free_calls: FREES.load(Relaxed),
        allocated_bytes: ALLOCATED.load(Relaxed),
        freed_bytes: FREED.load(Relaxed),
        live_bytes: LIVE.load(Relaxed),
        global_peak_bytes: PEAK.load(Relaxed),
        failures: FAILURES.load(Relaxed),
    }
}
/// MiMalloc delegate that records successful Rust allocation requests.
///
/// A diagnostic executable must install this value as its global allocator for
/// ordinary Rust allocations to be counted. Merely enabling the diagnostics
/// feature does not replace an executable's allocator. Direct C allocations and
/// MiMalloc's internal capacity are outside this ledger.
///
/// Successful reallocations add the complete new and old requested sizes to
/// the gross byte counters, while changing live bytes only by their difference.
/// This avoids treating internal realloc overlap as a requested live-byte peak.
pub struct CountingAllocator;
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { mimalloc::MiMalloc.alloc(l) };
        if p.is_null() {
            FAILURES.fetch_add(1, Relaxed);
        } else {
            ALLOCS.fetch_add(1, Relaxed);
            ALLOCATED.fetch_add(l.size() as u64, Relaxed);
            grow(l.size() as u64);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { mimalloc::MiMalloc.alloc_zeroed(l) };
        if p.is_null() {
            FAILURES.fetch_add(1, Relaxed);
        } else {
            ALLOCS.fetch_add(1, Relaxed);
            ALLOCATED.fetch_add(l.size() as u64, Relaxed);
            grow(l.size() as u64);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { mimalloc::MiMalloc.dealloc(p, l) };
        FREES.fetch_add(1, Relaxed);
        FREED.fetch_add(l.size() as u64, Relaxed);
        LIVE.fetch_sub(l.size() as u64, Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { mimalloc::MiMalloc.realloc(p, l, n) };
        if q.is_null() {
            FAILURES.fetch_add(1, Relaxed);
        } else {
            REALLOCS.fetch_add(1, Relaxed);
            ALLOCATED.fetch_add(n as u64, Relaxed);
            FREED.fetch_add(l.size() as u64, Relaxed);
            if n >= l.size() {
                grow((n - l.size()) as u64)
            } else {
                LIVE.fetch_sub((l.size() - n) as u64, Relaxed);
            }
        }
        q
    }
}
