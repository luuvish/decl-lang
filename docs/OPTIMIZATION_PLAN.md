# Query evaluation optimization plan

Baseline: `65e5939` (retained reference rounds). The work preserves language
behavior in TypeScript, Rust and Python. The frozen specification is unchanged;
performance representations may differ by language.

## Scope and acceptance

| Step | Deliverable | Evidence | Status |
|---|---|---|---|
| 1 | Profile the committed baseline and preserve reproducible measurements | Correctness-checked CLI samples and separate diagnostic profiles | Complete |
| 2 | Share executable schema and expression programs | `programs.json`: 1 versus 128 instances, stable compilation counts and fresh-evaluation parity; shared cache survives rounds and unchanged Session universes | Complete |
| 3 | Stop propagation when recomputation produces the same result | `rounds.json` requires value and verification cutoffs; structural and snapshot cases compare against fresh evaluation | Complete |
| 4 | Retain queries through Session edits | `edits.json` covers update/create/remove, bind/unbind, undo/redo, references, absent reads, captured contexts and diagnostics; work counts require reuse and bounded preparation | Complete |
| 5 | Validate, measure and document the complete implementation | Full gates, cold/preloaded batch and Session measurements, cache lifetime and documented limitations | Complete |

## Delivered changes

- Share immutable operations by AST identity and record plans by schema identity,
  passing the instance and environment at execution time. Remove the former
  per-instance compiler and duplicate constant cache.
- Bypass compilation for trivial literals; compile lazy literal children on
  demand. Share Programs across retained and fallback rounds.
- Retain revision stamps for affected observations. Equal results keep their
  old stamp; unchanged dependencies skip the reader's program.
- Diff supplied document inputs down to changed members and follow their
  readers. Prepare only affected queries; unaffected slots remain cached without
  reset or verification. Reuse unchanged record bindings and their closures.
- Preserve structural ownership, optional absence, container order and snapshot
  depth. Verify roots before former contents so removed records cannot produce
  obsolete diagnostics while validating an old dependency list.
- Isolate temporary expression programs, prune removed query and input metadata,
  and bound Rust's auxiliary materializations to an edit. Repeated requests and
  unique-path create/remove cycles have shared lifetime regressions.

The REPL work counter now includes member programs executed while binding an
edited document. The `files` transcript reports two computations instead of
zero; the previous counter omitted those executions.

## Measurement and validation

Use the baseline and final code with identical input and runtimes. Separate
uninstrumented CLI timings, preloaded batch work, edit latency and diagnostic
profiles. Accept a measurement only after checking its output and diagnostics.
Builds and tests must finish before timing begins.

The public, independently authored workloads are `decl-rs/examples/qbench.rs`
and `tests/benchmarks/session.json`. The Session drivers time apply, evaluation,
validation and scalar serialization after initial loading. Two warmup edits
precede nine samples at each size. One case keeps a derived bucket unchanged;
the other changes the final result. Timings are reported, not CI thresholds.

External comparison sources, models, data and profiling artifacts remain
outside the repository. See [performance results](PERFORMANCE.md) for public
measurements and [query design](../decl-ts/src/qengine/DESIGN.md) for the current
architecture.

Final evidence on 2026-09-10:

- `make verify`: **1,277 identical comparisons, zero differences**; all language
  suites pass, including 360 Python tests and 33 Rust internal checks.
- `make lint`: clean. `make format`: no file changes on the final check.
- Shared Session measurements: 39–42% less time in TypeScript, 53–58% in Python
  and 46% in Rust at 5,000 records, relative to the prior incremental Session.
- Repeated preloaded Rust batch measurements: flat records improve 5.2%; tagged
  records cost 1.8% more and a reference ring costs 7.3% more. These costs are
  documented rather than generalized into a universal speedup.
- Fresh-process CLI samples in all three languages match the accepted canonical
  output. Comparison artifacts remain outside the repository; the Rust timing
  was repeated in alternating order after variability in its first baseline.
- Temporary expressions and unique-path edit churn preserve correct results
  without accumulating auxiliary cache entries for discarded work.

## Explicit performance boundaries

- Revision preparation, supplied-data comparison and final validation still
  walk the live graph. Edits are not constant-time in document size.
- Whole-record comparisons conservatively prepare all values to preserve fresh
  evaluation of previously unforced members.
- A previous runtime diagnostic reruns query programs during recovery. Retaining
  individual diagnostics is a future optimization.
- Frozen/context-bearing results recompute unless their ownership is safe.
  Ineligible reference transitions use fresh rounds with shared Programs.
- Source/schema changes may rebuild the universe. The Session operation log
  intentionally retains undo history; auxiliary query caches retain live work.

These boundaries preserve existing behavior. Further optimization requires new
work counts, fresh-evaluation comparisons and measurements of actual latency.
