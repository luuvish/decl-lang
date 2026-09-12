# Rust query graph API migration

The Rust Engine stores retained queries as shared textual identities and
dependency snapshots. Its former public graph fields are now crate-private.
This is a **Rust source API break**: callers that accessed those fields must
migrate to the methods below.

Language rules, CLI output and diagnostic formats are unchanged. TypeScript
and Python keep their own representations. The public `Engine` type remains
the type returned by Session and pipeline results; `Engine::record`, `step`,
`slot_key` and `reset_slot` retain their textual signatures.

## Restricted fields

| Former field | Former public type | Internal representation |
| --- | --- | --- |
| `Engine.reads` | `RefCell<HashMap<String, HashSet<String>>>` | `RefCell<FxHashMap<QueryId, ReadSet>>` |
| `Engine.slots_by_key` | `RefCell<HashMap<String, (Inst, String)>>` | `RefCell<FxHashMap<QueryId, (Inst, String)>>` |
| `Engine.computing` | `RefCell<Vec<String>>` | `RefCell<Vec<QueryId>>` |

Read-only access through a removed field also requires migration. These
fields belonged to the crate's public API; the
[package's public-module statement](../decl-rs/README.md) did not make them
internal merely because callers commonly used them for diagnostics.

## Replacement access methods

| Previous operation | New method | Result |
| --- | --- | --- |
| Count `reads` entries | `dependency_queries()` | `usize`; includes retained empty read sets. |
| Count `slots_by_key` entries | `indexed_slots()` | `usize`; counts indexed member slots. |
| Get one owner's read set | `query_dependencies_for(key: &str)` | `Option<Vec<String>>`; dependencies sorted lexically. |
| Inspect every read set | `query_dependencies()` | `Vec<(String, Vec<String>)>`; owners and dependencies sorted lexically. |
| Inspect indexed slot keys | `query_slot_keys()` | `Vec<String>`; keys sorted lexically. |
| Look up an indexed instance/member pair | `query_slot(key: &str)` | `Option<(Inst, String)>`; a shared live instance and owned member name. |
| Inspect the computation stack | `query_stack()` | `Vec<String>`; outermost first, innermost last. |
| Inspect query storage | `query_storage()` | `BTreeMap<&'static str, usize>`; numeric counters and payload layouts. |

Count-only callers do not need to export the graph:

```rust
let owners = engine.dependency_queries();
let slots = engine.indexed_slots();
```

For a single owner, preserve the distinction between an absent owner and a
retained empty dependency set:

```rust
match engine.query_dependencies_for("root:config") {
    None => println!("no retained owner"),
    Some(dependencies) if dependencies.is_empty() => println!("no dependencies"),
    Some(dependencies) => println!("{dependencies:?}"),
}
```

The dependency, slot-key and stack exports own their text. Changing or
retaining those returned collections does not change or retain the live graph;
they can outlive the Engine. Exporting the entire graph copies and sorts its
text. Use a count or single-owner lookup where sufficient, and keep full
exports outside timed evaluation boundaries when measuring performance.

`query_stack` preserves computation order and duplicates. It does not sort or
deduplicate keys. Inside a nested `step` callback it includes both the outer
and inner computation; after the outermost ordinary return, including a
returned error, it is empty. Copying it costs time proportional to the stack's
depth and text length.

## Live slot handles

`query_slot` differs from the owned text exports. It returns the same shared
instance previously available through the slot index:

```rust
if let Some((instance, member)) = engine.query_slot("config.port") {
    let record = instance.borrow();
    if let Some(slot) = record.slot(&member) {
        println!("{:?}", slot.state);
    }
}
```

`Inst` is an `Rc<RefCell<RecInst>>`. Retaining it retains the value, and
mutation through that handle affects the shared record under the existing
`RecInst` API. The member-name String is owned. Changing the returned tuple
does not register, remove or replace an entry in the private query index.
Iterating `query_slot_keys` and then looking up each key inspects a live index;
it does not create an atomic owned snapshot of all records.

## Storage counters

`query_storage` inspects storage without interning keys, copying query text or
pruning the pool. It walks the relevant structures and allocates a numeric
result plus temporary body-identity deduplication storage. It is not a free
operation or an RSS measurement.

- `pool_*` counts cover the shared evaluation lineage, including identities
  retained by old runs or snapshots.
- `read_*` and slot-index counts cover this Engine. Bodies held solely by
  Pending, root snapshots or another Engine are absent from its read-map body
  totals.
- `scratch_index_entries`, `scratch_index_capacity` and
  `scratch_index_entry_bytes` describe the pool's separate weak underscore-key
  index. These are raw Vec entry/capacity counts and the size of one weak handle,
  not current scratch graph membership. Pool-only keys and keys retained by old
  Engines can be present; the census does not upgrade or prune them.
- Capacities count usable elements, not physical hash buckets. Struct/text
  payloads exclude allocator metadata and do not sum to total retained memory.
- `computing_depth` gives the current stack depth, but obtaining it through
  the full census also walks other query storage.

## Temporary identity maintenance

The pool owns one canonical ID per spelling. Live queries, revision snapshots,
and older runs own additional handles. An entry is logically dead when the
pool is its only owner; pruning it never invalidates an external owner. A
storage census borrows handles and does not trigger pruning, so temporary dead
entries can be visible between cleanup boundaries. Lookup does not revive a
pool-only entry; interning can reuse its allocation and counts the revival
as a creation for maintenance.

The canonical thin-ID table stores no weak handle: `weak_key_bytes` is zero and
`pool_entry_bytes` is one ID (8 bytes on the measured 64-bit target). The separate
scratch subset uses weak handles, reported by `scratch_index_*` instead.
The key still contains an `Rc<str>` and cached hash; `query_key_bytes` and
`text_handle_bytes` describe those existing types. These are representation
counters, not a promise of one allocation per key or total heap size.

Small Session scratch requests coalesce cleanup. At completion, after temporary
read sets, slots, demanded fallback roots, and revision stamps have been removed
or restored, the pool sweeps if at least 1,024 identities were created or
recreated since the last sweep. An automatic sweep during interning also marks
a completion sweep as due. That second condition matters for one large request:
the automatic sweep can retain thousands of identities still in use by the
request, which must be reconsidered after it releases them.

Live-key intern hits do not trigger an explicit pruning sweep. At full
capacity, the insertion-oriented table entry API can still grow using cached
hashes. Missing or pool-only identities can trigger automatic maintenance at a pool-size threshold that adapts to the live
population, with a floor of 1,024. Explicit evaluation-completion sweeps remain
in place. Large excess hash-table capacity is reduced after pruning; small
occupancy changes do not force reallocation.

These thresholds are an internal maintenance policy, not a public promise that
all dead entries disappear after every expression or that total storage stays
below a fixed byte limit. Query text lengths, live old runs, and a formerly
larger live population affect retained storage. This pool maintenance is
separate from the runtime value/type cycle collector.

## Session scratch subset

Temporary Session queries snapshot and restore existing `_` graph entries.
The pool keeps a weak index only for spellings that this cleanup may inspect:
`_`, `_.…`, `_[…`, and the same forms after one `assert:` prefix. A merely similar
prefix such as `_other`, or a different owner kind such as `root:_`, is excluded.
The raw slot predicate and assertion-stripped read-owner predicate remain
different. Session always checks its current graph maps because the shared
lineage index can also contain another retained Engine's keys.

A fresh eligible identity adds one weak entry; live hits and pool-only revival
do not append duplicates. Pool sweeps and subset enumeration discard expired
weak entries and shrink substantially excess capacity. Weak ownership does not
keep an identity externally live or retain its text after its last strong owner
is gone. A weak entry can retain an expired key's Rc control allocation until
pruning; the weak vector is not included in canonical text-byte totals.

With no newly demanded fallback root, cleanup visits this subset and only newly
appended registry entries. It preserves the old numeric registry prefix even
when callers publicly replace the registry or mutate a record's path. A newly
demanded fallback root still triggers complete graph and registry scans to
remove all matching descendants. Existing underscore snapshots are restored,
ordinary errors follow the same cleanup path, and temporary enumeration handles
drop before completion maintenance. This introduces no panic-unwind guarantee.

This narrows common scratch work; it does not make every request constant-time.
A real `_` root or retained old Run can keep a large subset alive. Edits pruning
still scans the current registry and cached input descriptions, and the existing
identity-maintenance budget can still trigger a whole-pool sweep.

## Graph mutation

There is no mutable String-map compatibility view. The graph, revision stamps
and retained snapshots share query identity, and new computations replace
read sets at the existing dependency-capture boundary. Writers use
copy-on-write when another snapshot owns the previous set. Arbitrary external
mutation of a public `HashSet<String>` cannot be intercepted to maintain those
invariants.

The Engine therefore has one authoritative ID graph. It does not maintain a
second full legacy graph or ignore writes to a detached compatibility copy.
A type alias cannot preserve the old concrete RefCell contract after the
representation changes.

Use Session document operations through `apply` and `run`. Explicit Engine
computations can continue to use `step` and `record` while tracking is enabled.
Those methods are not general substitutes for arbitrary graph injection,
deletion or stack rewriting. `reset_slot` resets a computed slot by logical
key; it is not a general graph import/mutation API. Consumers that require
unsupported direct mutations must adapt their operation to a defined Engine
or Session interface.

## Compatibility and validation

Preserving the old mutable field API would require keeping the old String
graph as the sole authority, deferring the storage change, or maintaining a
separately named Engine/Session backend. Keeping both complete graphs in one
evaluation would retain the storage that this migration removes. The chosen
design privatizes the graph and explicitly documents the source break.

Local repository tests use the access methods. A bounded consumer inventory
is not proof that no external caller used the old fields. Byte-identical CLI
results also do not establish Rust source compatibility.

The shared edit corpus covers equal-result dependency switching and existing
error, reference-round and owner-removal behavior. Rust API checks cover
owned text, ordered nested stacks, ordinary-error read retention, empty versus
absent owners, temporary-key churn with live dependencies retained, and
large temporary requests that release identities after an in-flight sweep. The
full `make verify` gate remains required alongside Rust consumer compilation.
Release notes must identify the field restrictions and these replacements;
versioning and publication follow the [development procedure](DEVELOPMENT.md).

The migration does not change `RecInst.slots`, add schema ordinals, narrow
invalidation, or revise the language specification. See the
[implementation](../decl-rs/src/engine.rs),
[graph store](../decl-rs/src/qengine/graph.rs),
[Rust API checks](../decl-rs/tests/api_test.rs), and
[performance diagnosis](PERFORMANCE_DIAGNOSIS.md) for the corresponding
interfaces and analysis boundaries.
