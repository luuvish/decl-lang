# Rust runtime memory API migration

This guide covers the Rust source compatibility and ownership changes for shared
computation descriptors and names, retained container paths, and raw-literal cache entries.
It also covers shared ordered map keys and independently mutable map values,
compact quantity, range, pattern, string and native-callable payloads, and the
released round rebase memo.
The high-level library functions, language syntax, and CLI output contracts remain
the same. These representations may differ from TypeScript and Python while
preserving the language's behavior. The earlier query graph changes are covered in
the [query graph migration guide](QUERY_GRAPH_MIGRATION.md).

## Slot computation descriptors

`Slot.compute` changes from `Option<Compute>` to `Option<Rc<Compute>>`. A slot keeps
its mutable state and cached value separately from the descriptor captured when
it was bound. Retained rounds and callers can share that descriptor without
copying its fields.

Compiled member plans also reuse complete `Default` and unsupplied `Derived`
descriptors across records when the declaration, captured root-name allocation
and effective environment allocation match. Each plan retains one recent weak
reference. `Check`, supplied/restated `Derived`, and bindings without compiled
programs keep their independent descriptors. Every record still has independent
slot state, value and navigation scope.

Descriptor pointer identity and strong/weak counts can therefore differ between
bindings. In particular, `Rc::get_mut` can return `None` for a single strong owner
while the plan retains a weak reference. Use `computation_mut()` or
`Rc::make_mut`: a shared descriptor is copied, and a lone descriptor's cached
weak reference is dissociated before mutation. The weak cache keeps no captured
value alive after the last strong owner drops, but may reserve one dead
descriptor allocation per plan until replacement or plan destruction. The
collector already traces each shared descriptor once and treats an external
descriptor snapshot as a real owner.

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

## Shared supplied-member order

`RecInst.entry_order` changes from `Vec<String>` to `Rc<Vec<String>>`. Records and
frozen snapshots can share the ordered key text while keeping their mutable slots
and values independent. Compiled binding reuses the most recent equal input order
for each schema through one weak entry. Equality includes every key and duplicate
in order; it does not depend on values. The weak entry retains no record, input
value or live order payload and does not cache every arbitrary input shape.

Use `record.entry_order()` to borrow the names, `entry_order_snapshot()` to retain
an order, and `entry_order_mut()` to edit with copy-on-write. Direct construction
or replacement uses `entry_order: names.into()`; direct field mutation uses
`Rc::make_mut(&mut record.entry_order)`. Iteration uses `record.entry_order.iter()`.
Copy-on-write preserves the order captured by other records, snapshots and native
callbacks. Changing order does not automatically invalidate query dependencies.

The elements are still owned `String` values. This metadata sharing does not make
record values immutable or permit snapshots to share mutable arrays/maps/slots.

## Shared member and root names

The following public Rust fields now use shared text:

| Field | Previous type | Current type |
|---|---|---|
| `Compute::Check`, `Compute::Default`, and `Compute::Derived`: `name`, `root_name` | `String` | `Rc<str>` |
| `Scope.root_name` | `String` | `Rc<str>` |
| `RecInst.slots` | `Vec<(String, Slot)>` | `Vec<(Rc<str>, Slot)>` |

For struct literals and replacement assignments, use `"name".into()`,
`Rc::from(text.as_str())`, or `owned_string.into()`. Read or compare the text
through `name.as_ref()`; use `name.to_string()` when an API requires an owned
`String`. Cloning the field now retains the same text allocation. `Member.name`,
the names inside `RecInst.entry_order`, and extra-member keys remain `String`.

`RecInst::slot(&str)`, `slot_mut(&str)`, and `has_slot(&str)` keep their signatures
and content-based lookup. Slots retain declaration order, including duplicate
names constructed by native callers. `Scope::new` and its scope-building methods
also keep their signatures. The public `Engine::query_slot(&str)` still returns
`Option<(Inst, String)>`; its internal shared-name lookup requires no caller
migration.

To edit a name, replace its `Rc<str>`. For an edit supported by mutable `str`,
`Rc::make_mut` isolates it from shared snapshots:

```rust
use decl_lang::semantics::Scope;
use std::rc::Rc;

let mut scope = Scope::new("root", None);
let captured = scope.clone();
Rc::make_mut(&mut scope.root_name).make_ascii_uppercase();
assert_eq!(scope.root_name.as_ref(), "ROOT");
assert_eq!(captured.root_name.as_ref(), "root");
```

`Rc::make_mut` yields `&mut str`, not `&mut String`. For length-changing edits,
create an owned `String`, edit it, and assign its `.into()` result. For descriptor
fields, first call `Slot::computation_mut()` to isolate the descriptor itself,
then replace or copy-on-write its name field. The existing state, query-index,
and dependency invalidation responsibilities still apply; changing a slot key,
descriptor name, or scope root does not automatically rename related metadata.

Compiled schemas can share member-name allocations across records, descriptors,
and internal query entries. Binding and scope snapshots keep their captured text
when a later schema or scope is replaced. Native code must not infer name identity
from pointer equality or depend on unique `Rc` ownership or fixed strong counts.
These text handles do not own record or environment graphs. Language names,
diagnostics, and CLI output contracts are unchanged.

## Shared ordered map keys

`MapV.entries` changes from `OrderedMap<Value>` to `MapEntries`. The
`OrderedMap<T>` alias remains available for ordinary insertion-ordered tables.
Completed small maps can share immutable key text and a lookup index. Each map
still owns an independent value vector; snapshots never share mutable value
storage merely because their keys match.

`MapV::get`, `has`, and `set` keep their signatures. The entry wrapper supports
ordered iteration, lookup, existing-value mutation, insertion, removal, and
clear. Replacing an existing value retains its position. New keys append;
`shift_remove` preserves the remaining order and `swap_remove` moves the last
entry to the removed position. Textual keys and values determine behavior, not
the identity of the shared key metadata.

For direct construction, convert an existing table explicitly:

```rust
use decl_lang::semantics::{MapV, OrderedMap, PrefixPath, Value};

let previous: OrderedMap<Value> = Default::default();
let map = MapV {
    entries: previous.into(),
    path: PrefixPath::default(),
};
assert!(map.entries.is_empty());
```

`Default::default()` and iterator collection into `MapEntries` also work.
Call `as_ordered_mut()` when a caller needs the full IndexMap API, such as
`entry()`. Call `into_ordered()` to consume the wrapper into the old table type.
Converting shared storage clones its key text and moves its values; it does not
clone or freeze the values. Structural changes perform the same conversion for
the affected map. Existing-key value updates preserve shared key storage.

The wrapper does not implicitly dereference to IndexMap. Concrete iterator
types and capacity observations differ from IndexMap; callers should prefer
borrowed iteration and `len()` when they do not need the compatibility table.
Keys in different maps may now have the same address. Their address and strong
counts are not language identities or stable ownership guarantees.

The Engine's bounded weak pool retains only key metadata handles, with complete
ordered-key comparison after a fingerprint match. It owns no runtime values.
Pool expiry or eviction may reduce sharing without changing map behavior.
Clearing a map removes its value edges independently of other maps sharing the
same keys, preserving cycle collection and frozen/live mutation isolation.

## Retained array and map paths

`ArrV.path` and `MapV.path` now hold `PrefixPath` directly. This replaces both
the original `Rc<Vec<Seg>>` representation and the intermediate `Rc<PrefixPath>`.
`PrefixPath` shares immutable prefix nodes and has no retained flat-vector cache.
`SegPath` remains the owned `Vec<Seg>` interchange type. Record paths
(`RecInst.path`) and reference paths (`Value::Ref`) retain their existing flat
representation.

| Previous operation | Supported replacement |
|---|---|
| Construct an array/map path from `segments` within an Engine | `engine.container_path(&segments)` |
| Construct a standalone path from an owned `SegPath` | `PrefixPath::from(segments)` or `segments.into()` |
| Construct from a borrowed slice | `PrefixPath::from_segments(segments)` |
| Construct an empty array/map path | `PrefixPath::default()` |
| Inspect segments | `iter()`, `get(index)`, `first()`, `last()`, `len()`, `is_empty()` |
| Format a path | `format(relative_root)`; for example, `format(Some("output"))` |
| Pass an owned flat path or a contiguous slice | Export with `to_vec()`, then borrow the exported vector if a slice is required |
| Mutate a container path | Call `push`, `pop`, or `clear` on its path field directly |

The same construction replacements apply to direct public `ArrV` and `MapV`
struct literals. `PrefixPath` does not provide `Vec` indexing, mutable segment
slices, or a contiguous borrowed `&[Seg]`. To edit an arbitrary segment, export a
vector, edit it, and construct a replacement path. For sequential reads, prefer
`iter()` to repeated indexed `get()` calls: indexed access walks ancestry.
Iteration borrows segments in canonical order; deep paths can allocate temporary
node-reference storage. `format()` uses borrowed iteration. `to_vec()` clones
segments into a new owned vector on each call, as does `Value::place()` for an
array or map.

Cloning a path copies its small outer handle and shares immutable prefix nodes.
Snapshots retain their paths after the Engine is dropped. Mutating one handle
preserves other snapshots without an outer `Rc::make_mut` operation:

```rust
use decl_lang::engine::Engine;
use decl_lang::semantics::{Env, Seg};

let engine = Engine::bare(Env::new());
let segments = vec![Seg::Name("output".into()), Seg::Name("items".into())];
let mut path = engine.container_path(&segments);
let snapshot = path.clone();
path.push(Seg::Idx(3));
drop(engine);

assert_eq!(snapshot.format(None), "output.items");
assert_eq!(path.format(None), "output.items[3]");
```

`Engine::retained_path` and `PrefixPathPool::retain` remain available for native
callers that deliberately need an outer `Rc<PrefixPath>` and its `Weak` handles.
Each such call still creates a distinct outer `Rc`; it is no longer a container
field. Use `Engine::container_path` or `PrefixPathPool::retain_value` for inline
storage. To convert an existing outer snapshot, clone its underlying path with
`snapshot.as_ref().clone()`. Container path lifetime can no longer be observed by
downgrading the field itself; retain an explicit snapshot when its lifetime matters.

Clearing the Engine's bounded weak prefix pool does not
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
the temporary table and finds the last matching entry by reverse search.
For larger inputs with a compiled schema whose member names are distinct, a
shared schema name-to-index map replaces the former membership set. It reuses
the compiled member-name `Rc<str>` handles. Each bind keeps up to eight distinct
supplied members in a local fixed-size array, then uses a sparse index table or
a dense value array. Dense storage is selected only after at least half the
schema's members have actually been supplied; duplicate occurrences and unknown
keys do not increase that count. Unknown keys still pass through the original
ordered extras/diagnostic processing.

Bare engines and native schemas with duplicate member names keep the prior
name-based table and its incremental growth. Input duplicates still select the
last supplied value, and duplicate schema slots keep declaration order. Nested
binds have independent scratch storage and retain their original schema/input
snapshots across callbacks. The fixed inline storage also contributes bounded
stack space to the lookup representation; avoiding a heap table does not mean
zero storage cost.

The lookup adds no Value owners; selected slot captures and ordered Edits storage
remain owned. Sharing schema index keys can change native member-name Rc counts.
Zero-member records still process extras and Edits storage. The schema index is
crate-private, and this lookup change adds no public signature or field-type
migration beyond the shared-name changes described above.

## Compact quantity, range and pattern storage

The Rust `Value` representation changes public payload storage. Language syntax,
evaluation rules and CLI output stay the same. Rust enum layout is not a stable
ABI; callers should use the accessors rather than depend on a particular size.

| Previous native payload | Current payload | Access |
| --- | --- | --- |
| `Value::Q { dim: Rc<String>, value: f64 }` | `Value::Q(Box<QuantityValue>)`, with shared dimension and independent magnitude | `Value::quantity`, `as_quantity`, `as_quantity_mut`, `quantity_dimension_mut` |
| `Value::Range { lo: Box<Value>, hi: Box<Value>, excl: bool }` | `Value::Range(Box<RangeValue>)`, whose endpoints are inline Values | `Value::range`, `as_range`, `as_range_mut` |
| `Value::Pat(Rc<str>)` | `Value::Pat(SharedText)` | `Value::pattern`, `as_pattern` |
| `Value::Str(Rc<str>)` | `Value::Str(SharedText)` | `SharedText::from_rc`, `as_rc`, `to_rc`, `into_rc` |
| `NatFn = Rc<dyn Fn(&[Value]) -> R<Value>>` | `Rc<Box<dyn Fn(&[Value]) -> R<Value>>>` | `Value::native` |

A quantity shares only its dimension string. Each clone owns a separate boxed
payload and copies the magnitude bits, so changing one clone's magnitude cannot
change another clone. Clones retain the dimension's `Rc<String>` without copying
its characters. The smaller Value payload adds a quantity allocation on creation
and clone; its effect depends on the workload.

Replace native construction with `Value::quantity("Time", 1.0)`. For direct
variant construction, use `Value::Q(Box::new(QuantityValue { dim, value }))`,
where `dim` is `Rc<String>`. Match `Value::Q(q)` and use `q.dim.as_str()` for text
lookup or `Rc::make_mut(&mut q.dim)` for an isolated mutable String. The
convenience accessors express the same boundary:

```rust
use decl_lang::semantics::Value;

let mut quantity = Value::quantity("Time", 1.0);
let retained = quantity.clone();
quantity.quantity_dimension_mut().unwrap().push_str("^-1");
quantity.as_quantity_mut().unwrap().value = 2.0;
assert_eq!(retained.as_quantity(), Some(("Time", 1.0)));
assert_eq!(quantity.as_quantity(), Some(("Time^-1", 2.0)));
```

Dimension equality still compares text, and magnitude equality retains ordinary
floating-point behavior, including NaN. Display units are resolved from the
current environment at serialization time; they are not cached in the quantity.
Shared dimension reference counts and `Rc::get_mut` availability are native
representation details. Retain a Value or explicit dimension Rc when its lifetime
matters.

Ranges now have one uniquely owned payload allocation. A range clone clones both
endpoint Values into a separate payload, preserving the prior clone rules of
those Values. Container or native-function endpoints still share their own Rc
owners; replacing an endpoint affects only the selected range. This distinction
also preserves the collector's incoming-edge accounting.

```rust
use decl_lang::semantics::Value;

let mut range = Value::range(Value::Int(1.into()), Value::Int(4.into()), true);
let retained = range.clone();
range.as_range_mut().unwrap().hi = Value::Int(9.into());
assert!(matches!(retained.as_range().unwrap().hi, Value::Int(ref n) if *n == 4.into()));
```

Match `Value::Range(bounds)` and borrow `bounds.lo`, `bounds.hi`, `bounds.excl`
instead of matching the former named enum fields. Constructors still accept any
endpoint Value; iteration and membership retain their existing validation and
error timing. Range and pattern equality remain non-reflexive, including when
two pattern handles share identical storage.

Pattern clones now share immutable text. Replace the text with
`value = Value::pattern(new_text)` instead of mutating a String inside `Pat`.
The constructor accepts owned strings, string slices, and shared string handles;
`as_pattern()` borrows the text without exposing its storage.

`SharedText` contains a thin shared descriptor around an existing `Rc<str>`.
String slices and owned strings still support `Value::Str(text.into())`.
Use explicit Rc conversion when moving between Value strings and path segments
so those operations continue to share the character allocation:

```rust
use decl_lang::semantics::{Seg, SharedText, Value};
use std::rc::Rc;

let characters: Rc<str> = Rc::from("clock");
let mut text = SharedText::from_rc(characters.clone());
let retained = Value::Str(text.clone());
let path_key = Seg::Key(text.to_rc());
assert!(Rc::ptr_eq(text.as_rc(), &characters));
text.make_mut().make_ascii_uppercase();
assert_eq!(&*text, "CLOCK");
let Value::Str(original) = retained else { unreachable!() };
assert_eq!(&*original, "clock");
let Seg::Key(key) = path_key else { unreachable!() };
assert!(Rc::ptr_eq(&key, &characters));
```

Equality, ordering, hashing and borrowed `str` access compare characters.
`make_mut` applies copy-on-write at both sharing levels, including dissociation
from weak-only inner owners. `as_rc` borrows the character owner, `to_rc`
clones it, and `into_rc` consumes a unique descriptor or clones the inner owner
when the descriptor remains shared. Character identity checks and weak handles
can use `Rc::ptr_eq(text.as_rc(), other.as_rc())` and
`Rc::downgrade(text.as_rc())`.

Value clones now increment the descriptor's Rc count instead of the inner
character Rc count. Consequently `Rc::strong_count(text.as_rc())` does not count
all retained Value clones, and old direct Rc count assumptions require migration.
Creating a new descriptor also adds an allocation; a smaller Value layout alone
does not prove lower total memory use. `Seg` and capture-cache keys retain their
existing `Rc<str>` representation.

Construct native functions with `Value::native(|args: &[Value]| {
Ok(args.first().cloned().unwrap_or(Value::Null)) })`. A direct `NatFn` constructor
now wraps the callable in a Box before the outer Rc. Clones share one callable
environment, and the last owner drops its captures once. `Value::Nat(f)` still
calls as `f(&args)`, and Rc/Weak operations on the outer `NatFn` remain available.
The collector continues to treat native captures as conservative external owners.

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
the owners required by those values. Results remain strongly cached while any
raw allocation alias survives, even if callers drop every returned result. This
preserves one-time evaluation and native mutations of the materialized result.

Insertion periodically removes entries whose raw strong count has reached zero.
The raw identity cannot be revived at that point. Removing its entry may release
the cached result and its otherwise unused captures earlier than Engine teardown;
keep an explicit returned `Value` when that lifetime is required. Returned values
and their mutable contents are not invalidated by removal. Results are released
after the cache borrow ends, so native destructors can reenter the Engine.

Sweeps are amortized by the surviving entry count. A table that loses most of its
entries also releases excess capacity, avoiding repeated scans of a formerly
large empty table. Transient requests keep their own sweep budget and restore the
original cache and budget together. Cache clearing resets the budget.
`Engine::cached_literals()` counts current entries; some expired raw identities
may remain until a subsequent sweep. It is neither a live raw-object count nor a
byte count, and applications must not depend on dead entries remaining present.

The collector's flat adjacency storage and exact container reservations require
no additional public API migration. An independently retained computation
descriptor remains an owner for cycle-collection purposes.

The collector reuses empty graph storage between passes of one active collection
call. Every graph-held strong and weak runtime handle is dropped before the next
root census, and address/incoming-edge/borrowed state is cleared. Native capture
destructors run at that boundary without a tracking-table borrow; recursive
collection stays suppressed. No graph capacity remains stored in the collector
after the call returns, including its terminating empty pass.

With `runtime-diagnostics`, a nonterminal `scratch_and_graph_drop` interval now
releases runtime owners while retaining empty graph capacity for the next pass.
A terminal interval releases all capacity before the call's final stamp. Phase
live bytes can therefore include reusable scratch after a nonterminal pass;
they are not an owner-only retained-value measure. Node/edge/root work must still
be compared independently from capacity growth and allocation traffic.

## Completed round rebase memo

After an evaluation settles and clears its previous-round pointer, its round
rebase memo is no longer queried. The Engine now releases that memo and its table
capacity at completion. Values or native captures owned only by the memo can
therefore be destroyed earlier. Independently returned values keep their owners
and aliases; the clean/revision state and reference-owner maps remain available
for Session replay and old-reference resolution. A subsequent evaluation installs
a new round cache.

Memo values are released outside the cache borrow so native destructors can
reenter evaluation. Embedding code must not rely on an obsolete memo to extend a
value's lifetime or preserve a particular strong-reference count.

## Optional runtime diagnostics

Collector pass work optionally includes `compute`, a scalar breakdown of unique
descriptor nodes into `check`, `default`, `derived`, `bridge`, and `expired`.
`derived_supplied` is a subset of `derived`; do not add it to the node total.
`body_size_bytes` reports the compiled inline `Compute` layout when a descriptor
was encountered, or zero otherwise. It excludes Rc headers, allocator rounding
and captured allocations. Aliased slot handles count once per graph pass, and
the diagnostic retains no descriptor or captured value. These teardown pass
counts are not a census at the evaluation memory peak. The fields are additive
to the optional diagnostics; existing node-kind and phase identities are unchanged.

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

### Snapshot advance attribution

Optional evaluation reports include a nested `advance` span with three disjoint
stages: `classify`, `freeze`, and `revision_reset`. Classification ends after its
temporary indices and dependency borrows are released. The terminal boundary is
recorded after the existing function-local owners and scratch have dropped,
without moving their destruction. These stages partition the advance body;
the enclosing round interval also includes the boundary instrumentation residual.
Do not add the parent and child durations or allocation deltas.

An unsuccessful attempt is labeled `advance_rejected`, and an unwinding attempt
is labeled `advance_unwound`. Their last stage is partial; earlier completed
boundaries can still be inspected. The ordinal names the next reused transition,
so rejected attempts may repeat it. Match nesting by timestamps rather than
treating the ordinal as a unique attempt ID. Reports keep only scalar metadata;
all hooks disappear when the feature is disabled.

### Root binding and interrupted execution attribution

`root_binding_diagnostics::take_root_binding_diagnostics()` drains a separate
thread-local list of root attempts. It does not add records to the existing
evaluation-span list. Each attempt separates dispatch, source evaluation,
recursive type binding, dispatch return, result handling, and local release.
The explicit completion distinguishes publication, deferred queueing, a deferred
result ignored in phase two, errors, taint, and a retained-round skip. A successful
Edits reuse can have zero producer calls; it must not be counted as a fresh bind.
Unwinding preserves the completed prefix and identifies the interrupted stage.

Root names are capped at 256 UTF-8 bytes, with the original byte length and a
truncation flag. Attempt ordinals persist across drains on their calling thread;
records are submitted in completion order, so native reentry can finish a later
ordinal first. Records retain no runtime values, engines, scopes, or callbacks.
The last boundary follows function-local destruction, but precedes argument
release and report-buffer insertion. Those remaining costs belong to the parent
interval. Local and enclosing spans overlap and must not be added together.
Requested live bytes and cumulative process peaks retain the allocation-ledger
limitations above; neither describes physical RSS or a phase-local high-water mark.

For a diagnostic that may be interrupted, `phase_events::install(file, limit)`
takes ownership of an empty regular file and returns a thread-bound `SinkGuard`.
The caller must keep it on the evaluation thread and prevent other writers to the
file. The limit is 2 through `MAX_RECORDS` (16,384), including header and a reserved
terminal slot. Each direct write publishes one 96-byte little-endian record,
without formatting, allocation snapshots, or runtime owners. The schema and
`evaluation_diagnostics::EVENT_STAGES` dictionary must accompany the stream.
A root event's ordinal matches its root record's ordinal; the full name is
represented by a hash and UTF-8 length, which alone are not collision-free identity.
The local clock stamp precedes the corresponding event publication slightly.

`Span::enter(stage)` publishes an explicit operation entry; `Span::mark(name)`
labels the interval that just finished. These are different boundaries. Engine
setup, round transitions, settlement, snapshot advance and root binding publish
entries before their work. A measurement helper can add surrounding command
stages with its own separately recorded dictionary. Other operations without an
entry hook have no implied event coverage.

Call `SinkGuard::finish()` after all event scopes end and retain its returned
status. Nested installation is rejected. A forgotten guard retains its file;
normal guard destruction detaches the sink before closing it, and scopes from an
old installation cannot write to a new one. Dropping a live scope records
incomplete or unwound work. Clock/write failures, short writes, ordering errors
and the cap disable telemetry without changing evaluation. This failure policy
belongs to the event stream; the existing evaluation-clock assertions still apply.

A complete event prefix establishes only the last successfully published entry,
not the exact instruction at termination. Missing terminals, partial tails,
truncation and clean closure remain distinct. Writes reach the kernel without
`fsync`, so the stream does not guarantee durability across a host crash. Event
I/O and root recording add diagnostic cost; instrumented results require their
own helper/source identity and cannot serve as ordinary CLI speed measurements.

### Session preparation query storage

Session preparation borrows temporary queue text from its seed sets, saved
values and dependency graph. Generated descendant keys stay owned until popped;
enqueueing them no longer interns text in the query pool. Its reverse index
continues to borrow thin reader handles. The borrowed-or-owned queue entry is
wider than a shared query handle, trading buffer space for fewer pool lookups.
Neither form retains Values or Engines. The String seed/invalid sets, reader
order, duplicate enqueues and LIFO traversal are preserved. A duplicate pop
checks invalid membership before copying text, and the drained queue buffer is
released before Pending capture.

Dependency borrows still end before revision preparation and native reset
callbacks. Public accessors and the conservative invalidation frontier are
unchanged; this representation change requires no API migration.

### Round classification scratch

Round classification formats each current record path once and records UTF-8
byte boundaries after each segment. The descendant index borrows these prefixes.
Boundaries come from the actual segments, because bare native root text and
quoted keys may contain delimiters. Current paths are read afresh rather than
using the slot-query spelling cache, which a native path mutation may invalidate.
Generated member prefixes also format borrowed segments without cloning a path
vector.

Inverse-edge indices borrow their outer keys and source paths from immutable
edge maps; canonical target keys remain owned, deduplicated and sorted as before.
The old snapshot borrow and both edge indices end before descendant traversal,
freezing and reset callbacks. Classification's dependency queue borrows existing
text and owns generated keys, while preserving the String sets and traversal
order. All prefix storage and dependency borrows end before freezing. These are
temporary representation changes, with no new public API or snapshot sharing.

Snapshot copying also uses separate temporary address maps for records, arrays,
maps and references. Each entry holds the corresponding shared handle instead
of a tagged address and a full Value. The maps preserve each kind's identity,
and a container's placeholder is inserted before copying its children so aliases
and cycles remain shared within the frozen graph. Mutable frozen records,
arrays and maps remain separate copies of the live objects. This reduces the
memo entry representation; its effect on peak memory requires measurement.

### Session edit preparation attribution

With `runtime-diagnostics`, `qengine::edits::Edits::diagnostics()` copies the
latest `SessionEditDiagnostics` record without reading clocks or retaining any
runtime owner. It contains nine disjoint preparation intervals (initial cache
release and root census, changed seeds, reverse index, saved values, retention
seeds, invalidation closure, Pending capture, reset, and final scratch release)
and six finish intervals (prune, graph prune, revision release, cache release,
query sweep, and final scope release).

The clocks are sampled only at phase boundaries. Intervals include diagnostic
bookkeeping and are supporting attribution, not uninstrumented Session timing.
Wall and process CPU readings are not atomic; check each phase's `recorded`,
`completed`, and `valid` fields. Clock failure produces invalid observations
rather than an evaluation error. A preparation that unwinds records its partial
last interval; the existing local destruction order is preserved.

Fixed counters record graph sizes and degree buckets, overlapping seed counts,
unknown value-owner fallback, queue traffic, Pending captures and reset/prune
work. They add no per-query names, reason maps or retained Values. Cumulative
forced-set sizes after seeding are not disjoint cause counts. No preparation
frontier is removed by these hooks.

`sequence` advances on preparation; `finish_sequence` advances on finish calls.
A hot or equal request can preserve the previous preparation record, so join it
to an actual revision change before attributing its cost to a request. An
inactive finish records only its prune phase in the latest-call snapshot.
`finish_aggregate` additionally retains all finish calls since the latest
preparation, so later inactive rounds do not erase earlier active cleanup costs.
It holds call, active-call, partial-call and superseded-call counts plus six
fixed phase sums. These wall/CPU sums cover separate intervals and exclude the
gaps between calls; they are not a continuous elapsed interval. Do not add the
latest finish times to the aggregate again. A new preparation resets the tally;
before the first preparation, sequence-zero finishes have their own tally.

A nested call supersedes an outer call's latest diagnostic publication. If the
calls belong to the same preparation, the aggregate counts the outer call as
superseded and omits its overlapping intervals, marking coverage invalid instead
of double-counting nested time. A call from an older preparation cannot add its
times to a newer preparation's tally. Partial calls, clock failures and integer
overflow also make the aggregate invalid; inspect those flags before using its
sums. No extra runtime owner is retained. All added storage and updates are
compiled out when the feature is disabled. These record types add no ordinary
CLI report mode.

### Serialization census

`serialization_diagnostics::census(value, settable_only)` counts what
`Engine::serialize` would emit for that value: visited values by emitted form,
written keys and member names, container depth, and the text values with their
bytes. It follows the serializer's order and omission rules, including hidden,
derived and unforced record members and the `null` a raw-document position
receives, and a Rust-only regression holds it to the serializer's actual output.
It is a separate traversal that the serializer never calls, so it adds no work
or storage to emission in either build.

The module has its own `serialization-census` Cargo feature, which
`runtime-diagnostics` implies. The evaluation hooks of `runtime-diagnostics`
record, and recording allocates, so a build with those hooks can place text
differently from an ordinary one. Enabled alone, `serialization-census`
compiles none of them: the engine it observes is the ordinary engine, and the
`serialization-census` example reads placement under the allocator the command
line uses.

The census evaluates nothing. A raw-document `PreVal` is counted in
`unevaluated_raw_values` and not entered, so whatever that expression would
produce is absent from every other count; a native callback therefore never
runs during a census. It retains no value, scope or engine, and it must not run
while a container of the value is mutably borrowed.

For each text value it also records the addresses of the two allocations behind
`SharedText`: the distance from the descriptor to its characters, and the
distances between the descriptors and between the characters of consecutively
emitted values, each in one of five fixed buckets from a cache line to beyond
2 MiB. `distinct_descriptors` and `distinct_characters` separate shared
descriptors from repeated imports of one character allocation. Addresses are
observations of one process's allocator at the time of the call: they are not
stable across runs, they say nothing about cache or page state, and a distance
is not a cost. Map keys and member names are borrowed `String` text without a
descriptor and carry no placement record. The call allocates two address sets
proportional to the distinct text allocations it meets; take it outside any
timed or ledgered interval.


### Proposed path cursor and phase traffic fields

The bounded path pool preserves canonical segments and adds one weak last-path
tail for depths up to 16. Its explicit `retain` compatibility API preserves fresh
outer `Rc` identity and copy-on-write behavior; container fields now use the inline
handle described above.
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
