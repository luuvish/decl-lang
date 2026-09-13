# Performance and optimization

Measured release history through `03d7a97` (2026-09-09), retained reference
rounds at `65e5939`, and shared programs with retained Session queries
(2026-09-10).
This document records runtime behavior and engineering priorities; the
[evaluation specification](specification/09_semantics.md) defines the language.

The largest measured gains come from avoiding repeated computation, sharing
data instead of copying it, and indexing reference lookups. These changes
benefit both evaluators. At `03d7a97`, the query layer had no demonstrated
overall batch advantage over the optimized tree walker. The current implementation shares executable
programs, retains eligible structures across rounds and Session edits, and
stops downstream recomputation when results stay equal. Section 5 separates
its edit improvements from one-shot evaluation costs.

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
procedure](DEVELOPMENT.md#performance-measurements) and the
[comparison build procedure](../tests/benchmarks/COMPARE.md#freeze-builds-and-protocols).
Use a fresh dedicated Cargo target directory for each frozen Rust variant and
record the executable emitted by Cargo with its source and artifact hashes.
The accepted historical measurements above used package cleans and embedded
commit identities; the stronger procedure applies to new comparisons.

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

## 4. Retained reference rounds at `65e5939` (2026-09-10)

At this checkpoint, the query evaluator retains a universe between eligible reference rounds.
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
stop further invalidation at this checkpoint. Section 5 adds this cutoff.

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

## 5. Shared programs and retained Session queries (2026-09-10)

This change completes the implementation steps in the
[query optimization plan](OPTIMIZATION_PLAN.md), relative to `65e5939`.

- **Shared schema and expression programs:** compile operations once per AST
  and record plans once per resolved schema. Instances pass their own runtime
  scope. The shared regression increases records from 1 to 128 without
  increasing compilation counts. Trivial literals bypass compilation; lazy
  literal children compile only when demanded.
- **Result and dependency cutoff:** recomputed equal values retain their
  previous change stamp. Dependent queries verify their reads and skip their
  programs. A shared multi-round reference case requires both kinds of cutoff.
- **Selective Session preparation and binding reuse:** compare changed supplied
  inputs down to member paths and follow their readers. Unaffected queries keep
  cached slots without reset or verification; unchanged records reuse their
  bindings. A single-member regression prepares at most 12 queries and executes
  at most four member programs, retaining sibling record identities.
- **Bounded auxiliary caches:** temporary requests have their own program cache;
  deleted records lose input metadata, dependencies and change stamps. Rust's
  unbound-literal materialization cache has an edit lifetime. Shared regressions
  run 400 temporary expressions and 100 unique-path create/remove cycles.

The measurement below covers the complete bundle. It does not assign isolated
milliseconds to individual mechanisms. Compilation/work counters prove which
work is removed; elapsed timings include remaining binding, graph traversal,
validation and Session bookkeeping.

### Session measurements

The same Apple M4, 16 GiB, macOS 26.6.2 machine ran both revisions, using
Node 24.18.1, Python 3.12.14 and Rust 1.97.1 release builds with mimalloc.
The shared [`session.json`](../tests/benchmarks/session.json) workload binds
and evaluates before timing. Each sample includes applying one edit, evaluation,
full validation and scalar serialization, with output and diagnostic checks.
Two warmup edits precede nine timed edits; the table reports medians. Builds
and tests were finished before timing. Source loading and process/parser startup
are excluded.

“Equal” changes a positive input while its derived bucket remains one. “Changed”
alternates positive/negative inputs so the bucket and final output change.

| Language | Records | Equal: baseline → current | Reduction | Changed: baseline → current | Reduction |
|---|---:|---:|---:|---:|---:|
| TypeScript | 100 | 1.03 → 1.02 ms | 0.7% | 0.60 → 0.66 ms | -9.9% |
| TypeScript | 1,000 | 8.21 → 4.72 ms | 42.5% | 5.94 → 4.59 ms | 22.7% |
| TypeScript | 5,000 | 39.75 → 24.25 ms | 39.0% | 40.62 → 23.40 ms | 42.4% |
| Python | 100 | 2.99 → 1.89 ms | 36.9% | 2.96 → 2.02 ms | 31.6% |
| Python | 1,000 | 36.86 → 18.01 ms | 51.1% | 36.22 → 19.76 ms | 45.4% |
| Python | 5,000 | 255.96 → 107.66 ms | 57.9% | 248.45 → 117.84 ms | 52.6% |
| Rust | 100 | 0.94 → 0.79 ms | 16.3% | 0.80 → 0.79 ms | 2.0% |
| Rust | 1,000 | 5.21 → 3.10 ms | 40.4% | 5.12 → 3.14 ms | 38.7% |
| Rust | 5,000 | 30.12 → 16.26 ms | 46.0% | 32.59 → 17.46 ms | 46.4% |

At 5,000 records the complete change cuts edit latency by **39–42% in
TypeScript, 53–58% in Python and 46% in Rust**. The smallest TypeScript changed
case instead costs about 0.06 ms more (9.9%); preparation overhead is not free.
The baseline is the previous incremental Session. They are workload-specific, not language-wide multipliers.

Reproduce with the [three benchmark drivers](../tests/benchmarks/README.md).

### Preloaded batch measurements

The release `qbench` harness loads, binds, evaluates, validates, serializes and
destroys the result, excluding parsing and static checking. One warmup and nine
samples yield each median; both evaluators must produce identical outputs.
Three process runs per revision, alternating their order, check repeatability;
the table reports the median of those three medians. The same machine and Rust
toolchain were used. An unusually slow first baseline sample is therefore not
treated as the whole improvement.

| Workload | Baseline query | Current query | Change in time | Current tree walker |
|---|---:|---:|---:|---:|
| Flat records, 2,000 | 7.52 ms | 7.13 ms | −5.2% | 7.10 ms |
| Tagged union, 2,000 | 15.21 ms | 15.49 ms | +1.8% | 15.55 ms |
| Reference ring, 1,600 | 10.38 ms | 11.14 ms | +7.3% | 9.29 ms |

The reference ring already settles without rebuilding changing structure, so
this change has no retained-edit work to eliminate there. Its remaining query
preparation and bookkeeping cost is visible. The timings do not establish a
single cause for that small regression. Shared programs are useful for repeated
schemas and edits; they do not make every batch program faster.

Final validation passed `make verify` (**1,277 identical comparisons, zero
differences**), including all implementation tests (360 Python tests), and
`make lint`. A final `make format` check leaves the working files unchanged.

### Remaining costs

Revision preparation, supplied-data comparison and final validation still walk
the live graph; editing one field is not constant-time in document size.
Whole-record comparisons prepare values conservatively to preserve fresh
handling of unforced members. A previous runtime error reruns query programs
for diagnostic recovery. Frozen/context-bearing results and ineligible
reference transitions use conservative recomputation. Source/schema changes
may rebuild the universe.

These are candidates for further optimization, with new correctness and work
criteria required before changing them. The current change preserves the
specification's reference rounds and final validation.

The [2026-09-12 diagnosis](PERFORMANCE_DIAGNOSIS.md) separates overlapping
invalidation walks from aggregate-observation granularity, identifies strong
ownership cycles beyond auxiliary caches, and records the invariant numeric
JSON regex compiled repeatedly in Rust. Its follow-up section records the
implemented regex cache, Rust cycle collection, scoped aggregate preparation,
deduplicated edit walks, and per-revision comparison snapshots, together with
the remaining granularity and representation limits.

The subsequent Rust follow-up defers descriptor cloning past cached slot returns
and reserves known record-member capacity. The diagnosis distinguishes these
bounded changes from remaining query-key, slot-layout and lifetime redesigns.
The [reproducible comparison runner](../tests/benchmarks/COMPARE.md) freezes
commands and inputs, validates every result, alternates process order, preserves
failure status and resumes completed observations without rerunning them.

The first query-storage follow-up borrowed temporary Rust reverse-graph text,
shared Pending during verification, and avoided replacing occupied owner/stamp
keys. Python's fixed-field Pending uses slotted storage. The subsequent Rust
migration stores the authoritative graph by canonical shared query IDs and
shares dependency-set bodies during preparation and verification. Revision
methods reuse IDs internally, and temporary Session requests coalesce identity-pool
cleanup while preserving a completion sweep after in-flight maintenance.
Scratch still scans the slot/read graph to save and restore temporary state.
Its Rust prefix-boundary predicate now checks a borrowed suffix instead of
constructing temporary prefix Strings for each visited key. This removes
per-check allocation without reducing the number of visited entries.

This migration restricts three formerly public Rust Engine fields and supplies
explicit inspection methods; see the [API migration guide](QUERY_GRAPH_MIGRATION.md).
It preserves query granularity and language behavior. The
[diagnosis](PERFORMANCE_DIAGNOSIS.md#10-canonical-query-identities-and-shared-read-sets)
records source-level work changes and storage/lifetime tradeoffs. These
mechanisms do not by themselves establish a new end-to-end speedup: distinguish
request latency, cumulative work, storage census, and whole-process peak RSS,
and attribute combined changes only at the scope actually measured.

The cold-execution follow-up removes work from two Rust paths: validation of
records with no assertions, and repeated interning within an actual member
computation. Empty validation keeps the existing absent/empty read-owner
semantics without constructing an execution scope. Member registration, step
entry, and revision computation pass one identity internally. Public APIs,
parent dependency recording, cached returns, and assertion error handling keep
their previous contracts. Compare the repair against both the original
pre-migration baseline and the migrated version; being faster than a regressed
version alone is insufficient evidence that the regression is resolved.

The interner follow-up stores one thin canonical ID per table entry and uses
its cached text hash during resizing. Normal lookup/insertion needs one text
hash and one entry probe. Pool-only entries can be reused without allocation;
borrowed ownership checks distinguish them from externally live identities.
The maintenance policy and logical graph contracts are unchanged, while the
pool element payload is smaller. The
[diagnosis](PERFORMANCE_DIAGNOSIS.md#10-canonical-query-identities-and-shared-read-sets)
details the ownership boundary and the remaining per-key allocations.

Selected-root compact JSON emission now reuses the engine's canonical text
and adds its trailing newline directly. This removes a full JSON decode and
re-encode from the plain output path; indented, YAML, template, and aggregate
output have separate processing. The
[emission diagnosis](PERFORMANCE_DIAGNOSIS.md#11-compact-output-emission)
explains the output boundary and the distinct evaluation costs that remain.

The engine's Rust serializer now writes each fresh result into one String,
removing intermediate subtree strings and container join buffers. It keeps
mutable public values fresh and preserves raw PreVal evaluation and diagnostic
order. A Session result cache would require a mutation/invalidation contract
that the current public ownership API does not provide. The
[serialization diagnosis](PERFORMANCE_DIAGNOSIS.md#12-streaming-canonical-serialization)
records the safety boundary, shared regression coverage, and measurement
procedure. Lower output allocation churn does not by itself reduce the retained
evaluation working set or establish an end-to-end latency improvement.

An isolated Rust snapshot-copy experiment reserved the known slot count before
filling its local vector. It reduced geometric capacity slack while retaining
the same initialized slots, captures and public APIs. The candidate was withdrawn
at that stage after large latency increases recurred in individual confirmation
pairs. The
[retained snapshot diagnosis](PERFORMANCE_DIAGNOSIS.md#14-retained-snapshot-slot-capacity)
describes how to distinguish that capacity effect from descriptor layout,
reachable-owner counts, allocator churn and process RSS. Earlier allocation
can affect failed-copy peaks, so retained bytes and end-to-end timing remain
separate acceptance measurements.

The subsequent [timing-control procedure](PERFORMANCE_DIAGNOSIS.md#15-timing-variation-and-complete-command-teardown)
uses a fixed same-binary comparison before reconsidering the capacity candidate.
It records child CPU and passive memory-pressure observations and extends cold
phase attribution through output-buffer release, runtime-owner release and the
final command sweep. These supporting intervals expose work omitted by an
emission-only helper; native CLI timings remain the end-to-end evidence.

The later [runtime-work diagnosis](PERFORMANCE_DIAGNOSIS.md#16-separating-evaluation-allocation-and-collector-work)
splits reference rounds, snapshot advance and nested collector passes, and records
array construction and materialization-cache traffic. Isolated GC, capacity,
capture, slot and path experiments retain separate correctness and adoption gates.
The combined implementation retains exact array and snapshot-slot capacities,
weak raw cache identities, shared computation descriptors, flat collector adjacency
and shared container-path prefixes under a prospective memory/correctness decision.
The earlier timing-control outcomes remain unchanged, and native speedup remains
unresolved. Rust embedding code should follow the
[runtime memory migration guide](RUNTIME_MEMORY_MIGRATION.md).
Reduced allocation work or smaller type layouts do not alone establish lower
command latency or a smaller peak working set.
