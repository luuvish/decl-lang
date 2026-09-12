# Reproducible process comparisons

`compare.py` runs correctness-checked commands serially, with a frozen manifest,
one discarded warmup and an odd number of fresh-process samples. It is a Python
standard-library tool for POSIX systems. It runs no build and invokes no shell.
Keep external workloads and their reports outside this repository.

```sh
python3 tests/benchmarks/compare.py /absolute/path/experiment.json --output /absolute/path/results
python3 tests/benchmarks/compare.py /absolute/path/experiment.json --output /absolute/path/results --resume
```

The output directory must be new or empty. Resume requires the same normalized
manifest, harness, host, runtime and effective-environment hashes and rechecks
every frozen file. Completed attempts
are never repeated; their captured files are checked again. A partial attempt
without its JSON record requires a new destination. A `.running` lock prevents
two writers. SIGINT/SIGTERM triggers child-group cleanup. If the harness itself
was killed with SIGKILL, use the last incomplete attempt's `.launch.json` receipt
to inspect its child PID, process group, command and launch time; first confirm
that the PID has not been reused, then stop that child group before removing the
stale lock. The child runs in a different process group from the harness.
Never edit a completed experiment
to insert a different candidate or change its sample selection.

Maintainers can exercise the harness independently of the language parity gate:

```sh
python3 tests/benchmarks/compare_test.py -v
```

These standard-library tests use temporary files and tiny Python subprocesses;
they require POSIX `wait4` and `ps` for checking terminated descendants. They
cover correctness contracts, invalid configurations, interrupted process groups,
immutable resume and artifact/environment changes. Run them before performance
measurement, with no primary benchmark running alongside them.

## Manifest

```json
{
  "schema_version": 1,
  "cwd": "/absolute/path/checkout",
  "warmups": 1,
  "samples": 3,
  "timeout_s": 120,
  "env": { "LANG": "C" },
  "artifacts": [
    { "path": "/absolute/path/before", "sha256": "<64 lowercase hex digits>" },
    { "path": "/absolute/path/after", "sha256": "<64 lowercase hex digits>" },
    { "path": "/absolute/path/input.json", "sha256": "<64 lowercase hex digits>" }
  ],
  "variants": [
    { "id": "before", "argv": ["/absolute/path/before", "{input}", "{output}"] },
    { "id": "after", "argv": ["/absolute/path/after", "{input}", "{output}"] }
  ],
  "cases": [
    {
      "id": "small",
      "vars": { "input": "/absolute/path/input.json" },
      "expected": { "exit": 0, "output_sha256": "<64 lowercase hex digits>" }
    }
  ]
}
```

The placeholders above are illustrative, not runnable hashes. Obtain SHA-256
values from independently validated baseline artifacts; never bless candidate
output merely because it differs. Every case must specify an expected exit code
and at least one of `stdout_sha256`, `stderr_sha256`, or `output_sha256`. Freeze
all relevant binaries, scripts, models, input files, lockfiles and runtime
executables in `artifacts`; executable entries are mandatory. The tool verifies
those files before and after each new attempt, outside the timed region.

Every `argv` element is passed literally, after replacing case variables such
as `{input}` and the reserved `{output}`. The latter becomes a unique absolute
path. Commands must write an output file only when their contract uses it.
Standard output and error always go to separate files. Variables do not invoke
shell expansion, and shell redirection characters have no special meaning.

The process inherits its environment. Global `env` overrides it; optional
variant `env` overrides global values. Pin all performance-relevant settings in
the manifest, including heap limits, runtime paths, allocator settings and
engine switches. Effective inherited environments are fingerprinted without
writing their values; only shell bookkeeping (`PWD`, `OLDPWD`, `SHLVL`, `_`) is
excluded. A changed environment requires a new experiment directory. A case can
override `timeout_s`. Resolve libraries and imported
source trees explicitly: hashing one CLI entry does not freeze its dependencies.

For two variants the order is AB, BA, AB, BA, including warmup. With more
variants, each round rotates the starting variant and each complete rotation
reverses direction. The first variant is the ratio reference. Run no builds,
tests or other heavy benchmarks alongside the commands.

Serial execution controls only this runner's children; another terminal or
agent can still start a competing benchmark. Coordinate the host's measurement
window before running and observe known competing workloads through completion,
including their harness's output-processing phase. A quiet check at launch alone
does not establish isolation for the whole campaign. Keep any host monitor
consistent across variants and outside request instrumentation.

If competition is detected, preserve completed and partial attempts, the process
identities and observed overlap interval. Interrupt only the current campaign's
children, coordinate with the other job, and use a new experiment directory for
the confirmation. Do not remove just the slower observations or label host
contention as a candidate correctness defect. Reports should separate output
validity from whether a timing series supports an isolated performance claim.

## What is measured

The primary timer covers process launch through process exit, including startup,
requested writes and teardown. Hashing and output inspection happen afterward.
The runner writes a small launch receipt while the child runs; this consistent
harness bookkeeping is inside the process observation boundary.
`wait4` supplies CPU time and peak RSS; RSS is converted to bytes on macOS and
Linux. This is child-process accounting, not a sampled process-tree measurement.
There is no RSS ceiling or memory monitor. Peak RSS is neither total allocations
nor live bytes, and compressed memory can affect its relation to wall time.

Timeouts kill the process group. The harness also cleans up descendants after
the leader exits. A timeout, signal, unexpected exit, launch error or output
mismatch receives no accepted latency. After a variant/case fails, its remaining
planned attempts are recorded as skipped; other variant/case pairs continue.
Capacity failures therefore do not become artificial fast observations.

Every attempt has an immutable JSON record and captured stdout/stderr/output
files. `summary.json` can be regenerated on resume. It reports accepted sample
count, planned count, median/min/max, peak RSS and per-round candidate/reference
ratios. Groups and ratio entries carry observed/planned counts and completion
status. A group with fewer than the planned successful samples is explicitly
incomplete. Do not claim a speedup from its partial median or paired ratio. A nonzero exit is
accepted only when explicitly part of a hash-verified diagnostic contract;
signals and timeouts are always failures.

Exit codes: 0 means all attempts matched their contracts; 1 means an attempted
command failed a contract or resource deadline; 2 means setup, identity or
artifact validation failed. This tool sets no CI speed threshold and computes
no confidence interval from three samples.

## Efficient iteration

1. Write one falsifiable hypothesis: the target stage, work counter or retained
   byte count, expected direction, behavior invariants and regression cases.
   Use one-variable candidates before combining successful changes.
2. Run the shared correctness cases and a small causal diagnostic. Inspect
   operation counts separately from latency; instrumented binaries are a
   different series. Reject the mechanism if its intended work does not fall.
3. Compare only affected variants at the smallest realistic size. Three paired
   fresh processes screen material effects; warm edits inside one process are
   not independent process replicates. Preserve small regressions and all raw
   observations. Extend a noisy result with a separately declared confirmation
   experiment, not by discarding inconvenient samples.
4. Promote a supported candidate to the next representative size and record
   both time and memory. Refresh one unchanged control to check host drift; do
   not rerun every language for a representation change in only one language.
5. Run expensive capacity cases when a change addresses scaling or storage,
   or at a release checkpoint. A short diagnostic deadline locates work; it
   does not replace the established completion deadline. Finish the repository
   parity and lint gates after the candidate is fixed.

Keep a short experiment ledger outside the repository for private work. Each
entry links its hypothesis, parent result, source patch and hashes, manifest,
raw observations, correctness evidence and decision (`retain`, `reject`,
`inconclusive`, or `needs_capacity_check`). Record the measured boundary and
whether each conclusion is causal, observational or an untested proposal.
Generate tables from the raw records and retain historical results by link.
Join before/after observations by explicit case and pair IDs, never by directory
enumeration order or independently sorted latency arrays. Reconstruct derived
tables independently before reporting; retain and label superseded summaries if
an analysis error is repaired. Raw observations remain unchanged.

This runner compares full processes. A Session API timing series still needs
its own driver and request boundary; do not compare the process timer around
that driver with a single request timer. Cross-engine comparisons need a
separate semantic preflight when full output bytes differ: verify the common
payload, required diagnostics and output scope before choosing a shared
contract. Matching one successful output does not establish equivalence of
all constraints or negative-input diagnostics.

## Freeze builds and protocols

Build before starting a measurement window. For each frozen Rust variant, use
a new dedicated Cargo target directory, including for helper projects whose
path dependency points at that variant. Do not reuse a target directory across
checkouts, copied source trees, or differing probes, and do not treat a binary
filename or an embedded revision alone as proof of its complete source inputs.
A package clean is weaker evidence than a fresh isolated build directory.

Capture Cargo's `--message-format=json-render-diagnostics` stream separately
from stderr and require a successful build exit. Select the expected package,
target name/kind, and profile from its `compiler-artifact` record. Verify the
reported executable is inside the dedicated target directory, and record that
exact path and its SHA-256. Require the measured target to be newly built in
that directory. Store the command, effective build settings, Cargo manifest and
lock hashes, source-manifest hash, and build-time hashes of every probe/helper
input in the build receipt. A probe hash first observed after a build cannot
retroactively prove which probe source was compiled.

Freeze a complete path inventory and content hashes for the relevant source
roots, including transitive modules, runtime scripts, manifests, lockfiles,
generated inputs, and the wrappers that produce and validate results. Declare
excluded build/cache directories explicitly. Comparing hashes for only listed
files misses newly added source files, so compare the exact included path set
as well. Revalidate this inventory before and after accepted attempts. The
runner's flat `artifacts` checks do not discover transitive imports or unlisted
files; a campaign wrapper must supply this inventory check where needed.

Run a small protocol preflight before timing. Verify operation names and order,
request boundaries, output destinations, JSON schema, diagnostics, and the
meaning and ownership of counters. A successful preflight is a correctness
receipt, not a latency sample. Helpers should reject missing or extra fields,
duplicate JSON keys, nonfinite numbers, and incorrect value types where their
contract excludes them; in Python, `True` must not pass an integer-count check.
Check every required diagnostic source, including empty-success diagnostics,
rather than inferring success only from an exported scalar.

For a Session driver, validate every newly emitted output at its own unique
path and record its byte length and digest. Use an independently validated
oracle for that interface's exact output. Compact Session JSON and CLI renderer
bytes can differ even for the same value; neither one is automatically the
other's byte oracle. When comparison requires a typed common payload, validate
the complete agreed schema, field presence, numeric distinctions, order where
required, and diagnostics before sealing the expected digest. A matching sum
or output count is not a full-document check.

Keep parsed stdout rows, per-operation artifacts, and the final summary
consistent in schema, values, and order. Check the declared complete operation
inventory, not only whichever rows happen to exist. Lifetime and storage
observations are separate diagnostic operations: confirm owner identity and
that observation itself does not prune or mutate the measured graph. A counter
reset caused by a new owner is not a negative amount of work; cumulative
counters require per-request deltas within the same retained owner.

Create receipts and observation files exclusively, preserve raw outputs on
failure, and close files and reap owned child processes on every exit path.
Write a campaign completion marker only after the full planned inventory and
all contracts pass. A validated partial series remains incomplete. Extend or
repair a protocol through a newly frozen campaign; do not rewrite completed
observations or silently reuse output from an earlier attempt.

## Attribute combined interventions explicitly

A candidate can change canonical key storage, dependency snapshots, ID lookup,
cleanup frequency, and the per-key cost of scratch prefix checks together. Its end-to-end difference is the effect of
that bundle. Logical operation counts can support the proposed mechanism,
but they do not allocate the measured time or RSS among components. A graph
scan can visit the same keys and execute the same queries while spending less
on each prefix check. Record scan scope and per-visit allocation separately;
query-count equality alone cannot distinguish those costs. Use isolated
variants when a causal component estimate matters; otherwise report the bundle
and keep component effects as source-derived expectations or profile evidence.

Report graph/storage diagnostics independently of uninstrumented latency.
Shared-body counts need an explicit owner scope, and retained old runs can
change pool population without changing the current Engine's read count.
Record the request sequence, old-owner retention/drop points, and maintenance
policy alongside each diagnostic observation. This makes later repetitions
comparable without assuming that a lower process peak proves a smaller live
graph or that a faster scratch request proves narrower invalidation.

When repairing a regression, keep both the pre-regression baseline and the
current version in the screening experiment. A candidate can improve on the
current version while remaining slower than the original baseline. For three
variants, schedule complete Latin blocks so each occupies every position;
discard warmups without advancing that rotation. The generic runner's
rotation plus reversal does not guarantee this for three measured blocks:
use an explicitly checked campaign schedule. Report candidate/baseline and
candidate/current ratios within each block separately, alongside wall time,
CPU time, and peak RSS. Prespecify a continuation gate for the small screen;
use a fresh, larger confirmation series when the result is marginal, rather
than treating a favorable small-sample median as proof of equivalence.

## Separate work removal, allocation, and memory pressure

For a follow-up with several changes, keep a component-only variant when it
answers a specific uncertainty. For example, compare the previous version,
parent query-ID reuse alone, and the complete candidate in the same CLI blocks.
Use the previous/full pair for a retained Session sequence and a focused scratch
workload. A complete historical matrix is unnecessary for every screening step;
promote only after exact-output and correctness gates pass. State which sizes,
implementations, and reference systems were actually rerun.

Collect work/allocation diagnostics in separate source-bound executables. Count
eligible slot states and capture guards, key construction and interner outcomes,
read-set mutation, maintenance, and scratch visits at their actual boundaries.
Distinguish visiting a map entry, cloning a key into a vector, and checking that
vector's prefixes; do not label their sum as independent keys or CPU time. Keep
the same logical counter definitions after changing the implementation, and
record new subset visits separately from removed full scans.

A diagnostic allocator may delegate to the production allocator with the same
build settings while counting successful requested sizes. Check each phase's
live-byte balance and partition of event categories. Reallocation contributes
the old size to freed requests and the full new size to allocated requests;
this is churn, not bytes copied. Its requested-live peak excludes allocator
size classes, metadata, reserved pages, C allocations, and overlap inside the
allocator's reallocation. Tags identify the innermost allocation call site,
not final ownership. Instrumentation changes timing and must stay outside the
primary latency comparison.

Report phase-end requested live bytes, phase peak, resident memory, and physical
footprint as distinct quantities. A compressed process can retain more live
allocation than its resident RSS suggests. Endpoint host compressor and swap
observations establish the measurement conditions, not exclusive causality or
a continuous pressure trace. A lower serialization peak does not establish a
smaller evaluation working set. Keep the performance decision separate from
claims about retained objects and allocator fragmentation.

Maintain one immutable campaign directory with a prespecified decision policy,
baseline and candidate source manifests, isolated build receipts, exact binary
pins, discarded warmup records, every measured block, independent fresh-output
validation, and a small analysis receipt. Reuse the existing runner and analyzer
when their protocol applies; freeze any necessary diagnostic adapter separately.
Record failures and revisions as new artifacts. Bind the final working tree's
native inputs to the measured snapshot after the full correctness gates, and
index the accepted artifacts so the next iteration can use them directly.

Complete the bounded ownership/drop-order review and targeted tests before the
candidate freeze. Cleanup can release user callback captures even when it does
not evaluate an expression, so graph-borrow scope and destructor ordering belong
in that review. Run the full correctness gate alongside independent isolated
builds, then release a single measurement lane only when both finish. This keeps
build/test load outside timings and avoids spending a full measurement cycle on
a candidate that still needs an ownership refinement. Superseded attempts remain
recorded, rather than being silently folded into the final candidate.
