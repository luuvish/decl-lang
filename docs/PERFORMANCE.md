# Performance and optimization

Measured release history through `03d7a97` (2026-09-09), followed by the
reference-round reuse implementation (2026-09-10).
This document records runtime behavior and engineering priorities; the
[evaluation specification](specification/09_semantics.md) defines the language.

The largest measured gains come from avoiding repeated computation, sharing
data instead of copying it, and indexing reference lookups. These changes
benefit both evaluators. At the last measured release, the query layer had no
demonstrated overall batch advantage over the optimized tree walker. The new
round cache retains eligible structures and computations; arbitrary session
edits and result-level cutoff remain future work.

## 1. What the optimizations remove

### Repeated computation and materialization

The initial TypeScript reference (`a670459`, 2026-08-31) already cached
successful member-slot values and resolved named types. Later changes closed
gaps around those caches:

- `78782b7`: remember that a slot is deferred until the next evaluation phase,
  instead of repeatedly attempting a computation whose reference answer is
  unavailable. TypeScript's internal deferral and taint signals also stop
  capturing stack traces.
- `8699ac8`: force a member once, returning absence with its value. Previously,
  presence testing and value access each forced it; a deferred chain could
  multiply attempts exponentially with depth.
- `285ba7f`, `06189d6`, `b932710`: cache materialized arrays, maps, and records,
  and flatten lazy spread chains with an explicit work stack. Consumers reuse
  an already constructed structure, and flattening no longer consumes host
  stack frames in proportion to chain depth.

`285ba7f` also added `sort`, `sort_by`, `unique`, and `reverse`, reducing the
need for expensive fold-and-spread implementations in Decl programs.
`sort_by` computes each key once; `unique` still has a quadratic worst case.

The accumulated tree-walker changes through `b932710` reduce the measured
reference-ring time from **1,133.33 to 668.68 ms (41%)** relative to v0.4.0.
This is the combined checkpoint effect, not an isolated cache measurement.

### Shared Rust values and cheaper allocation

`d74108d` shares strings and paths through `Rc`, and extends a local scope with
one linked frame instead of copying its entire variable map. Cached slot reads
avoid unnecessary path-key construction. Faster hot-table hashing, mimalloc
in the binaries, and release LTO were introduced in the same change.

This reduces allocation, copying, and destruction. The reference ring drops
from **668.68 to 126.62 ms (81%)**. The measurement does not separate the
allocator's contribution from the representation and compiler changes.

### An inverse reference index

`9c5192e` builds a table from each target path to its referrers once per frozen
evaluation round. Asking which records refer to a target then uses an index
lookup instead of scanning all candidates again. Returning and sorting the
matches still costs time proportional to the answer's size.

The reference ring drops from **126.62 to 101.00 ms (20%)**. A remaining copy
on the index's cache-hit path is removed in the final optimization below.

### Inline small integers and fewer array copies

`59ec30f` stores integers that fit in `i64` directly and promotes larger values
to an arbitrary-precision representation. Common indices and counts avoid
heap-backed integer storage without changing numeric semantics.

Array `all`, `any`, `filter`, and `fold` read one element at a time instead of
copying the entire vector before iteration. `count` had already changed to
borrow the materialized array's length in `9c5192e`.

For flat records, this checkpoint reduces **8.28 to 7.35 ms (11%)**. Its effect
on the reference-ring workload is much smaller.

### Remaining lookup, schema, and query overhead

`14b7085` combines five changes:

- Reuse an existing inverse reference index before acquiring its source edge.
  Rust previously deep-copied the full edge even on a cache hit.
- Store Rust map entries in an insertion-ordered index, replacing linear key
  lookup and replacement while preserving serialization order.
- Share resolved Rust record-member definitions, using copy-on-write during
  recursive type construction. Binding many values through one schema no
  longer clones its entire member list at each lookup.
- Store Rust compiled slot bodies in a round-owned arena with integer handles
  and weak evaluator references. Reuse the slot's existing value cache instead
  of maintaining a second string-keyed memo and unused dependency bookkeeping.
- Let module callers request values and diagnostics without first producing
  unused JSON for every internal root. Other roots are still evaluated and
  validated; this removes redundant serialization, not required validation.

The query-engine reference ring drops from **108.35 to 9.91 ms (91%)**;
tagged records improve from **26.82 to 15.58 ms (42%)**. The tree walker also
benefits from the shared runtime changes. These timings cover the bundle, not
the individual contribution of each item.

`03d7a97` then clears partial roots, instances, diagnostics, and constant caches
before fallback to the tree walker. It fixes duplicate or missing diagnostics
and is not counted as an additional normal-path speedup.

### Reuse in interactive sessions

`692ad57` added a bounded source-text parse cache for repeated module reads.
`badb126` added Session dependency tracking: unchanged questions reuse the last
run, while document changes invalidate readers transitively. Changes that
cannot be handled incrementally trigger a fresh evaluation.

`b932710` disables dependency recording for one-shot evaluation, where its
per-slot sets and keys are unused. Interactive reuse and batch execution thus
pay different costs. The batch measurements below do not quantify edit speed.

## 2. Measured history

Eight Rust checkpoints were rebuilt on an Apple M4 with 16 GiB RAM, macOS
26.6.2, and Rust 1.97.1. Each retained its locked dependencies and release
settings; the harness used the allocator of that checkpoint's CLI. Each
engine/workload received one warmup and nine timed samples; times below are
medians. Builds and tests were not run alongside measurement.

The common harness loads declarations, binds, evaluates, validates, serializes,
and destroys the result. Parsing, static checking, process startup, and file
writes are excluded. It uses the synthetic generators in
[`qbench.rs`](../decl-rs/examples/qbench.rs), adapted to tree-only checkpoints.
There are seven workloads at different sizes, 70 medians, and 630 timed runs.
Canonical outputs match byte for byte across all eight versions; checkpoints
with a query engine also check its output against the tree walker.

| Checkpoint | Evaluator / milestone | Flat records, 2,000 | Tagged union, 2,000 | Reference ring, 1,600 |
|---|---|---:|---:|---:|
| `v0.3.0` | Tree walker | 29.07 ms | 41.21 ms | 1,124.52 ms |
| `v0.4.0` | Tree walker | 29.17 ms | 40.24 ms | 1,133.33 ms |
| `b932710` | Accumulated tree-walker improvements | 17.58 ms | 44.85 ms | 668.68 ms |
| `d74108d` | Shared Rust values and allocation changes | 8.27 ms | 25.65 ms | 126.62 ms |
| `9c5192e` | Inverse reference index | 8.28 ms | 25.33 ms | 101.00 ms |
| `59ec30f` | Small integers and array iteration | 7.35 ms | 24.65 ms | 99.59 ms |
| `547d497` | Initial default query engine | 11.87 ms | 26.82 ms | 108.35 ms |
| `03d7a97` | Query engine at this checkpoint | 7.78 ms | 15.58 ms | 9.91 ms |

Relative to v0.3.0, these are **3.74×**, **2.65×**, and **113.50×** improvements
for the three workloads. They are not a universal language-speed multiplier.
The early prototype predates the tagged releases and has no compatible timing
series here. Specification revisions are not package release tags.

Scaling also improves: increasing the reference ring from 400 to 1,600 nodes
raises v0.3.0 time **14.79×**, versus **4.26×** for the query engine at `03d7a97`.
This sampled range shows the effect of removing superlinear lookup and copy
costs, not a complexity guarantee for arbitrary Decl programs.

For new measurements, use the [development handbook's measurement
procedure](DEVELOPMENT.md#performance-measurements). When comparing historical
builds, clean the package or isolate build directories and verify the binary's
commit identity. The accepted measurements above used package cleans and
embedded commit identities to exclude stale artifacts.

## 3. Query-engine contribution at `03d7a97`

The query database (`218cb4d`) provides memoization, dependency capture,
revision checks, and early cutoff when dependencies or derived results are
unchanged. The language layer compiles expressions into callable operations,
and became the default for source reports and module evaluation at `6859f5a`.

The tree walker already caches successful slot values. In one-shot work, the
query layer adds compilation and bookkeeping without retaining its database
across rounds or edits. At `547d497`, flat records take **11.87 ms** through the
query engine versus **7.55 ms** through the tree walker: about 57% overhead.
The comparison after the subsequent optimizations, at `03d7a97`, is:

| Workload | Tree walker | Query engine |
|---|---:|---:|
| Flat records, 2,000 | 7.21 ms | 7.78 ms |
| Tagged union, 2,000 | 15.60 ms | 15.58 ms |
| Reference ring, 1,600 | 9.54 ms | 9.91 ms |

There is no demonstrated overall batch advantage in these workloads. The
large cumulative gains should be attributed primarily to shared runtime
improvements, with additional work reducing the query layer's own overhead.

At this checkpoint, the generic database's incremental mechanisms were
implemented and unit-tested, but each language evaluation round created a fresh
evaluator. Session used the existing Engine dependency tracking. Document binding and some
computed structures also use the value-layer evaluator; strict query mode
proves absence of whole-engine fallback, not compilation of every expression.
See the [query-engine design](../decl-ts/src/qengine/DESIGN.md) for the current
implementation boundary.

## 4. Retained reference rounds (2026-09-10)

The query evaluator now retains a universe between eligible reference rounds.
The optimization has five parts:

1. **One value cache.** Compiled slots use the existing Engine slot cache and
   dependency graph. TypeScript and Python no longer store each field result
   again in `Db`; Rust already used this approach.
2. **Target-specific invalidation.** Track roots, members, constant reads,
   inverse-reference answers, and snapshot reads. Inverse answers invalidate
   readers only for targets whose result changes. Compare observed frozen
   values and container shapes with the next snapshot. Forwarded inverse
   references, frozen object results, and deeper snapshot reads also depend on
   snapshot age and recompute across transitions. Scalar reads whose observed
   value is unchanged remain cached.
3. **Structure ownership.** Invalidating a producer removes its registered
   descendants before rebuilding. Unchanged objects remain allocated, and
   clean subtrees skip repeated forcing.
4. **Cheaper snapshots.** TypeScript and Python share frozen slot maps until
   the live owner replaces a field. Frozen reads keep their previous-round
   owner; Rust retains its owned-slot representation. Snapshots avoid building
   an unused index of every field. Rust also preserves the owner when a frozen
   member returns a reference into an older snapshot.
5. **Less bookkeeping.** Skip tracking in programs without reference queries
   and in successful initial materialization. Allocate dependency sets on
   demand, cache field keys, and skip key construction on unobserved cache hits.

The TypeScript query path also uses the shared round driver for deferred
function-result roots and declares all sibling members before compiling their
expressions, matching the existing Rust and Python name-resolution behavior.

The stability rule and final validation are unchanged. Complex or incomplete
states switch to fresh-round evaluation for the remaining rounds. This is batch
reuse through the Engine's
slot graph, not adoption of the generic database's revision API by Session.
Transitive invalidation is conservative; unchanged recomputed values do not
currently stop further invalidation.

The shared structural regression case adds and removes collections over three
reused transitions. It checks outputs and diagnostics against fresh evaluation,
and requires retained records. Nested references check snapshot depth; cyclic
and unstable cases compare their errors as well. See the [query design](../decl-ts/src/qengine/DESIGN.md) for boundaries.

A release `qbench` run on 2026-09-10 (one warmup, nine samples, medians,
output equality checked) also checks the batch overhead on the repository's
synthetic programs:

| Workload | Fresh tree walker | Retained query path |
|---|---:|---:|
| Flat records, 2,000 | 7.06 ms | 7.43 ms |
| Tagged union, 2,000 | 15.27 ms | 15.28 ms |
| Reference ring, 1,600 | 9.31 ms | 10.36 ms |

These programs do not rebuild a changing structure across multiple rounds.
They show the remaining bookkeeping cost, especially for reference queries
that already settle in the first round. Retention targets repeated work; it
does not make every one-shot program faster.

## 5. Further optimization

1. **Compile a schema program once and share it across instances.** A type's
   expression such as `sum = x + y` needs one reusable operation, with the
   instance supplied at execution time. Sharing executable code goes beyond
   the current sharing of member definitions and reduces repeated preparation
   and storage for large collections of the same type.
2. **Add result-level cutoff to retained rounds.** Version supplied values,
   container membership, and reference answers independently. If recomputation
   returns an unchanged result, stop before invalidating downstream work. The
   current implementation conservatively invalidates all transitive readers.
3. **Connect the retained database to Session edits.** Reuse unchanged results
   after updates, create/remove, rebind, and undo/redo. Equal-result cutoff can
   stop downstream recomputation even when an upstream input changed. Existing
   Session partial recomputation is the baseline to compare against.

These are implementation priorities, not measured future speedups. Invalidation
must include absent and failed reads, structural changes, and diagnostics.
Before adoption, compare each edit against a fresh evaluation with identical
outputs and diagnostics. Measure cold CLI execution, preloaded batch work,
reference rounds, and edit latency separately.
