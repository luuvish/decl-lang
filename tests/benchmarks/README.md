# Session performance workload

For frozen fresh-process comparisons and an efficient promotion protocol, see
[Reproducible process comparisons](COMPARE.md). The Session drivers below time a
different boundary: one request inside an already initialized process.

Harness maintainers can run `python3 tests/benchmarks/compare_test.py -v` before
measurement; these isolated subprocess tests are separate from language parity.

`session.json` is an independently authored workload shared by three benchmark
drivers. Each Session binds its document and evaluates once before timing.
Two edits warm it; nine more yield a median at each size.

Every timed operation applies one field update, evaluates and validates the
Session, and serializes the scalar output. Parsing the edit expression and
Session bookkeeping are included. Process/parser startup and initial source
loading are excluded. Every sample's output and diagnostics are checked.

The `equal` case changes a positive supplied integer while its derived bucket
stays one. The other case alternates positive and negative inputs, changing the
bucket and final output. Both retain the rest of the document. Run from the
repository root, without other builds or benchmarks running:

```sh
node decl-ts/scripts/session-bench.ts
decl-py/.venv/bin/python decl-py/scripts/session_bench.py
cargo run --locked --release --example sbench
```

The drivers accept the JSON configuration path as their first argument. The
TypeScript and Python drivers accept `DECL_BENCH_REPO` to load another checkout;
the Rust driver must be compiled against that checkout. Use the same workload,
runtimes and sampling procedure for both revisions. Recompute counters from
older Session implementations may omit binding work, so compare elapsed time
and verified results instead of treating those counters as equivalent.

## Aggregate-observation diagnostic

`aggregate_control.json` and `aggregate_null.json` keep the same three-member
items, scalar sum, and unrelated record. Only the additional boolean output
changes: a constant `true`, or the unrelated record compared with null. Both
produce the same exported values for every benchmark edit. They isolate the
preparation caused by an aggregate observation outside the edited input.

The existing three drivers accept either file. Run each command separately,
then repeat with `aggregate_control.json`:

```sh
node decl-ts/scripts/session-bench.ts tests/benchmarks/aggregate_null.json
decl-py/.venv/bin/python decl-py/scripts/session_bench.py tests/benchmarks/aggregate_null.json
cargo run --locked --release --example sbench -- tests/benchmarks/aggregate_null.json
```

These cases use two warmups and five measured edits at each size. The drivers
check the scalar sum and runtime diagnostics; the separate diagnostic probe also
checks the boolean result and records prepared/executed query counts. See
[the performance record](../../docs/PERFORMANCE.md#3-aggregate-observations-have-two-independent-problems)
for the distinction between aggregate scope and repeated subtree traversal. Timings are diagnostic evidence,
not CI thresholds.
