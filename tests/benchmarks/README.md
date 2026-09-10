# Session performance workload

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
