# Rust runtime memory API migration

This guide covers the Rust source compatibility and ownership changes for shared
computation descriptors, retained container paths, and raw-literal cache entries.
The high-level library functions, language syntax, and CLI output contracts remain
the same. These representations may differ from TypeScript and Python while
preserving the language's behavior. The earlier query graph changes are covered in
the [query graph migration guide](QUERY_GRAPH_MIGRATION.md).

## Slot computation descriptors

`Slot.compute` changes from `Option<Compute>` to `Option<Rc<Compute>>`. A slot keeps
its mutable state and cached value separately from the descriptor captured when
it was bound. Retained rounds and callers can share that descriptor without
copying its fields.

Prefer the following accessors when working with an existing slot:

| Method | Result and ownership |
|---|---|
| `computation()` | `Option<&Compute>`: borrow the current descriptor |
| `computation_snapshot()` | `Option<Rc<Compute>>`: retain the current descriptor and its captured owners |
| `computation_mut()` | `Option<&mut Compute>`: use copy-on-write to isolate this slot's mutation from outstanding descriptor snapshots |
| `set_computation(Option<Compute>)` | Replace or remove the descriptor; a replacement receives a new `Rc` |

For direct `Slot` construction, change `compute: Some(descriptor)` to
`compute: Some(Rc::new(descriptor))`; `compute: None` remains valid. For pattern
matching, use `slot.computation()` or `slot.compute.as_deref()`.

Copy-on-write protects the descriptor's fields. It does not deep-copy or freeze
the `Rc` owners captured by those fields: existing `RefCell` contents and closure
captures keep their normal Rust sharing behavior. Keeping a descriptor snapshot
also keeps its captured owners alive. Dropping the originating slot does not
invalidate an independently held snapshot.

Replacing a descriptor does **not** reset `Slot.state` or `Slot.value`, and does
not invalidate retained queries. For example, a slot already forced to `7` still
returns its cached value after its descriptor is changed to compute `9`, until
the caller performs the required reset. Preserve the reset and dependent-query
invalidation policy of the embedding application. `Engine::reset_slot(key)`
resets an indexed computed slot; it is not a substitute for invalidating every
dependent query in a retained session. For an independently constructed slot,
the existing explicit state/value update remains available:

```rust
use decl_lang::semantics::{Compute, Slot, SlotState, Value};

fn replace_unforced(slot: &mut Slot, next: Compute) {
    slot.set_computation(Some(next));
    slot.state = SlotState::Unforced;
    slot.value = Value::Undef;
}
```

This example only updates the slot; session dependency handling belongs to its
caller. The accessors do not change binding-time capture into a later lookup.

## Retained array and map paths

`ArrV.path` and `MapV.path` change from `Rc<Vec<Seg>>` to `Rc<PrefixPath>`.
`PrefixPath` shares immutable prefix nodes and has no retained flat-vector cache.
`SegPath` remains the owned `Vec<Seg>` interchange type. Record paths
(`RecInst.path`) and reference paths (`Value::Ref`) retain their existing flat
representation.

| Previous operation | Supported replacement |
|---|---|
| Construct an array/map path from `segments` within an Engine | `engine.retained_path(&segments)` |
| Construct a standalone path from an owned `SegPath` | `Rc::new(PrefixPath::from(segments))` or `Rc::new(segments.into())` |
| Construct from a borrowed slice | `Rc::new(PrefixPath::from_segments(segments))` |
| Construct an empty array/map path | `Rc::new(PrefixPath::default())` |
| Inspect segments | `iter()`, `get(index)`, `first()`, `last()`, `len()`, `is_empty()` |
| Format a path | `format(relative_root)`; for example, `format(Some("output"))` |
| Pass an owned flat path or a contiguous slice | Export with `to_vec()`, then borrow the exported vector if a slice is required |
| Mutate a shared outer path | `Rc::make_mut(&mut path)`, followed by `push`, `pop`, or `clear` |

The same construction replacements apply to direct public `ArrV` and `MapV`
struct literals. `PrefixPath` does not provide `Vec` indexing, mutable segment
slices, or a contiguous borrowed `&[Seg]`. To edit an arbitrary segment, export a
vector, edit it, and construct a replacement path. For sequential reads, prefer
`iter()` to repeated indexed `get()` calls: indexed access walks ancestry.
Iteration borrows segments in canonical order; deep paths can allocate temporary
node-reference storage. `format()` uses borrowed iteration. `to_vec()` clones
segments into a new owned vector on each call, as does `Value::place()` for an
array or map.

Outer `Rc` snapshots retain their paths after the Engine is dropped. Mutating one
outer handle through copy-on-write preserves the other snapshot:

```rust
use decl_lang::engine::Engine;
use decl_lang::semantics::{Env, Seg};
use std::rc::Rc;

let engine = Engine::bare(Env::new());
let segments = vec![Seg::Name("output".into()), Seg::Name("items".into())];
let mut path = engine.retained_path(&segments);
let snapshot = path.clone();
Rc::make_mut(&mut path).push(Seg::Idx(3));
drop(engine);

assert_eq!(snapshot.format(None), "output.items");
assert_eq!(path.format(None), "output.items[3]");
```

Each call to `Engine::retained_path` returns a distinct outer `Rc`, even when its
prefix nodes are shared. Clearing the Engine's bounded weak prefix pool does not
invalidate existing paths. Prefix-node addresses exposed for diagnostics are not
language identities or reference identities. Existing `Value::Ref(Rc::new(flat))`
construction remains valid.

## Temporary record-path capture during forcing

The lazy diagnostic-path candidate keeps `RecInst.path` as `Rc<Vec<Seg>>`.
`Engine::force_slot` retains that handle before dependency verification and
computation, then formats the original path only when reporting a cycle or
evaluation error. A native callback may replace or mutate the current path;
the diagnostic still refers to the path at force entry. The temporary handle
is released when the force returns and is not kept in an Engine cache.

This changes temporary ownership observed by native code. An additional strong
owner can make `Rc::get_mut` return `None`, change `Rc::strong_count`, or keep a
`Weak` path upgradeable until the force returns. Do not assume unique ownership
during evaluation. If embedding code deliberately edits a shared path, use
replacement or `Rc::make_mut`; retain its existing path-cache and query-identity
invalidation policy. Copy-on-write alone does not reset those caches or retained
dependencies. The field type, canonical diagnostic path and language behavior
are unchanged; the candidate's full native performance acceptance is pending.

## Temporary child paths during collection binding

Array and map binding now lazily allocate one function-local flat path buffer.
The parent segments are copied once, and each child index/key is pushed before
binding and popped afterward. Each returned record or container still owns its
path. The buffer is not stored on Engine, so nested or reentrant binds have
independent scratch storage. Empty collections and maps with no accepted keys
create no scratch buffer.

Native code can observe a change in temporary segment ownership. After the
first accepted child, the copied parent Seg Rc owners survive between siblings
and through subsequent map-key checks until this collection bind returns or
unwinds. The current child segment is removed before result insertion and before
the next key check. Code inspecting instantaneous Rc counts or unique access must
not assume that each sibling starts with the former reference counts. This does
not add an outer record-path or PrefixPath handle; the borrowed input path already
keeps its original segment values alive. No scratch reference escapes in a Value.
Public signatures and field types are unchanged, and explicit native path edits
still require the existing path-cache and query-identity invalidation policy.

## Immutable collection inputs during binding

Rust array/map binding now borrows the immutable `JArr`/`JObj` input vector and
clones each item when visiting it. The original raw `Rc<Vec<...>>` remains alive
throughout the bind. Mutable `Arr`/`Map`, expanded `PreArr`/`PreObj`, and
record-to-map inputs still create eager owned snapshots; their items move into
binding without a second clone. No mutable `RefCell` borrow spans child callbacks.
These are membership/order snapshots, not recursive copies of nested mutable
values.

Native callbacks can observe fewer temporary owners for unvisited immutable
items. Their `Rc::strong_count` and uniqueness observations may differ, and early
return avoids cloning the remaining suffix. The original raw still owns its
contents until bind returns. An external outer `Rc::make_mut` handle separates
from that original snapshot, so changing it does not replace later input items
in the running bind. Keep explicit owners when embedding code requires a value's
lifetime; do not rely on an intermediate vector's former duplicate owners.

Visited object keys still copy their `String`, and some `Value` variants allocate
when cloned. The change removes the redundant whole-input vector; it does not
promise zero-copy binding. Public signatures, field types, key order, duplicate
replacement behavior and Decl diagnostics are unchanged.

## Immutable record inputs during binding

Record binding now borrows a JObj's immutable ordered entries for its repeated
passes. The original raw Rc remains alive for the whole call. PreObj, mutable
Map and record inputs keep their existing eager owned snapshots. The local
supplied lookup borrows entries; no borrowed value escapes into outputs. Lazy
Compute descriptors, open-record extras and Edits' retained input snapshot keep
their required owned copies.

Native diagnostic tagger callbacks can observe fewer transient owners of the
input Values because the initial full entries clone and the supplied lookup
Value clones are gone. Code must not rely
on that former duplicate ownership for Rc counts, uniqueness or lifetime. An
external Rc::make_mut continues to separate that handle from the running bind's
original vector. Nested or reentrant binds have independent local slices, and
mutable input membership snapshots still finish before later diagnostics.
These snapshots do not recursively freeze mutable descendants.

Entry order, duplicate handling and the positions of Edits' unchanged/bound
checks are unchanged. Retain explicit owners for embedding lifetimes; output
slots/extras and the existing Edits snapshot do not borrow the temporary Cow
slice. Public signatures and field types require no migration.

The temporary supplied lookup now borrows both key strings and Values from the
entries slice. The selected Value is cloned when its slot descriptor is created;
lookup itself adds no owner. For example, before a future lazy slot exists, a
PreVal can have only the raw entry's owner, where the preceding borrowed-key
implementation also held a supplied-table owner. After slot construction the
selected descriptor adds its own owner. The temporary lookup adds no owner for
unselected duplicates; existing source and Edits snapshots keep their original
ownership.

Code that reads Rc/Weak strong counts in a native tagger must account for this
change. Keep explicit owners for application lifetimes instead of depending on
the former temporary lookup clone. Safe callbacks still cannot uniquely mutate
a value that raw or the owned snapshot holds. A callback-triggered collector may
see a different root/owner count, but raw/snapshot reachability keeps borrowed
values alive, including cycles; slot owners keep selected values alive afterward.

Borrowed keys and values stay local and expire before raw or the owned entries
snapshot. When the input or schema has at most one entry or member, binding skips
the temporary table and finds the last matching entry by reverse search. With
multiple inputs and members, the hasher and incremental table growth stay the
same; raw entry count is not used to reserve space for potentially duplicate keys.
Zero-member records still process extras and Edits storage. This lookup bypass
adds no owner-count change from the preceding borrowed-Value implementation.
Selected slot captures and ordered Edits storage remain owned. No public
signatures or field types change.

## Raw-literal cache lifetime

The Engine's materialization cache now stores a typed weak identity for each raw
`PreArr` or `PreObj`, together with a strong materialized result. Previously the
cache also kept a strong raw owner. The weak identity reserves that raw `Rc`
allocation's address until its cache entry is released, preventing address reuse
from matching an unrelated literal. It does not keep the raw vector's contents
or their unused captures alive.

This changes observable Rust ownership behavior even though the cache's public
method signatures are unchanged:

- A raw `Weak` can stop upgrading while its cached result remains alive.
- Strong/weak reference counts and the outcome of `Rc::try_unwrap` can differ.
- Objects captured only by the raw literal can run `Drop` earlier. The cache is
  no longer a lifetime guarantee for those objects.
- `Rc::make_mut` on a cached raw owner obtains a different allocation identity,
  so the changed raw literal does not reuse its old materialization entry.

Keep an explicit raw `Rc` or `Value` if your application requires that raw value
or its otherwise unused captures to remain alive. Returned values still retain
the owners required by those values. Cached results remain strongly owned until
the existing cache-clear, transient-request, or Engine lifetime boundary.
`Engine::cached_literals()` counts entries, including entries whose raw strong
owner has expired; it is neither a live raw-object count nor a byte count.

The collector's flat adjacency storage and exact container reservations require
no additional public API migration. An independently retained computation
descriptor remains an owner for cycle-collection purposes.

## Optional runtime diagnostics

`runtime-diagnostics` is an opt-in Cargo feature for separate measurement helpers:

```bash
cargo build --release -p decl-lang --features runtime-diagnostics
```

It enables evaluation and collector phase records, requested-allocation support,
and scalar retention/path counters. The feature is disabled by default; its
instrumented builds collect extra data and must be distinguished from ordinary
builds when comparing performance. Enabling it does not add a CLI report mode.

Evaluation and collector timing currently require `libc::clock_gettime` with
`CLOCK_MONOTONIC` and `CLOCK_PROCESS_CPUTIME_ID`. The diagnostic measurement path
has been exercised on macOS; there is no Windows clock fallback. Leave this
feature disabled on targets that do not provide those clock APIs. Evaluation
clock failures assert; collector observations record clock errors explicitly.

Enabling the feature does **not** install a global allocator. A diagnostic helper
must explicitly select `allocation_diagnostics::CountingAllocator` as its global
allocator to populate its requested-allocation ledger. This allocator forwards
to MiMalloc. The normal binaries continue to select MiMalloc directly, and the
library sets no global allocator. The ledger describes successful Rust allocation
requests, frees, failures, and requested live/peak bytes; it does not measure
allocator-reserved bytes, foreign C allocations, or resident memory.

`evaluation_diagnostics::take_evaluation_diagnostics()` and
`semantics::take_gc_diagnostics()` drain scalar records on the calling thread.
Drain collector records after the owners
and guards of interest have dropped to include their cleanup. Retention and path
counters are also thread-local; path counters are cumulative, so compare
snapshots without resetting them under live paths. Allocation counters are
process-wide and cumulative, so an allocation delta can include work from other
threads. Their separately loaded snapshot fields are not a synchronized
transaction, and the peak covers the process rather than an individual phase.
`semantics::prefix_path_diagnostics()` returns zero counters without the feature;
its counter storage and hot counter updates are compiled out in that mode.

Reports carry measurement metadata rather than runtime owners. They still add
measurement work and storage. Compare matching report schemas, allocator setup,
feature flags, and phase boundaries, and keep diagnostic evidence separate from
ordinary CLI timing. This migration guide makes no latency or memory-saving
claim; measurement methodology is documented in the
[performance comparison guide](../tests/benchmarks/COMPARE.md).

### Initial Session attribution

The `session-initial` example runs one initial `Session.run(Full)` request,
serializes the complete `oid`, and writes scalar timing/allocation/GC records.
Build it once, then use a fresh output prefix for each invocation:

```sh
cargo build --release -p decl-lang --example session-initial --features runtime-diagnostics
DECL_QENGINE=1 DECL_QENGINE_STRICT=1 target/release/examples/session-initial MODEL INPUT PREFIX
```

`MODEL` declares the `oad` input and `oid` output. The helper writes
`PREFIX-oid.json` and `PREFIX-initial.json`, and prints the metrics to standard
output. It keeps the ordinary request owners through serialization, releases
them and the Session, then drains scalar diagnostics. It adds no explicit
collection. These instrumented times are supporting evidence, not native CLI
or Session speedup measurements. The example itself applies no host resource
limit; controlled comparisons need the same resource monitoring for both arms.

The `session_run` span separates reuse dispatch, the fresh run, and preparation
of the retained run. Its nested `session_fresh` span separates loading,
checking, binding setup, evaluation, result retention, validation, and diagnostic
finalization. Actual root binding is inside the evaluation round spans;
`Run.timing.bind` measures preparation before evaluation. The fresh span ends
before its return closure and local releases; its caller includes that residual.
Never add these parent and child spans together. Assign automatic GC to a
request interval only when its timestamps fall within that interval; Session
teardown is recorded separately. The hooks are absent from feature-empty builds.


### Proposed path cursor and phase traffic fields

The current uncommitted performance candidate preserves `PrefixPath` values,
canonical segments, fresh outer `Rc` identity, `Rc::get_mut` and copy-on-write
behavior. Its bounded pool adds one weak last-path tail for depths up to 16.
A weak-only tail can reserve one dead node allocation but does not retain the
node body or ancestry. Deep paths use the existing pool lookup.

Diagnostic structs gain scalar fields: `PrefixPathDiagnostics` adds pool-call,
input-segment and cursor counters; `PrefixPathPoolStats` adds cursor weak/dead
entry counts and depth. Existing pool-entry fields still describe the hash table.
These are additive returned-record fields, but exhaustive struct literals and
patterns in embedding code need adjustment; prefer returned records and `..`
patterns, or `Default` where the type provides it.

Optional evaluation `Stamp` records additionally expose `prefix_paths` and
`traffic`. `retention_diagnostics::traffic_snapshot()` copies fixed cumulative
constructor/cache counters without allocating or retaining owners. These samples
follow the existing clock/allocation reads and are not an atomic transaction.
Array indices follow the site order in `retention_diagnostics::snapshot()`.
Subtract endpoints for traffic; live counters can fall. The hooks and hot counter
updates remain absent from feature-empty native builds. Skipping instrumented
operations also skips their counter overhead, so native timing must be verified
separately.
