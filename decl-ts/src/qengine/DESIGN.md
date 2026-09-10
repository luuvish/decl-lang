# Query engine

Implemented in TypeScript, Rust, and Python. The query evaluator is the default
for source reports and module universes. `DECL_QENGINE=0` selects the tree
walker; `DECL_QENGINE_STRICT=1` rejects an unsupported form instead of falling
back. Differential tests explicitly select fresh tree evaluation as their
oracle.

The language implementation uses the Engine's member slots as its single value
cache. `Programs` shares executable code, `Revisions` verifies invalidated
queries, `RoundCache` retains eligible reference rounds, and `Edits` connects
Session document changes to the same slot graph. The standalone generic `Db`
is independently tested; it is not the language universe's storage layer.

## Shared programs, separate instance state

`Programs` caches expression operations by AST identity and record plans by
resolved schema identity. An operation receives its Engine and runtime scope;
it does not capture a particular instance, root, or module environment. Record
plans share default/derived operations, conjunction type lists and member-name
indexes. Recursive schema construction may replace a member list, so the cache
checks its identity before reuse.

Document inputs, function-produced records and literal roots use the same
binder. Lazy literals retain their source scope until binding supplies the
instance context. Navigation, short-circuiting, closures and module lookups
preserve the tree evaluator's order. Trivial literals read directly and lazy
children compile only when demanded. The former per-instance compiler and its
second constant cache have been removed.

One Programs object survives both retained and fallback reference rounds, and
Session edits with an unchanged source universe. Source/schema changes may
rebuild the universe. Temporary expressions use request-local programs so
unique requests do not grow the source program cache.

## Query observations and cutoff

Dependencies include:

- Root and member reads, including cached values and absent optional members.
- Constant reads.
- Inverse-reference answers for a specific target, including empty answers.
- Frozen member values, container shapes and snapshot depth.
- Aggregate record observations for whole-value comparisons.

An invalidated query temporarily retains its old result, dependencies and
change stamp. Verification brings those dependencies up to date. If their
stamps remain unchanged, the old result returns without executing its program.
If execution produces an equal result, the query restores its old change stamp
so downstream readers can stop at verification.

Container paths, key order and membership participate in comparison. During a
Session edit, a retained record's shape includes its identity, type, owner,
member kinds, entry order and extras; ordinary member values remain separate
queries. Equal canonical paths alone do not prove equal snapshot ownership.
Frozen references and unsafe captured contexts therefore recompute.

Verification uses the normal forcing stack for cycles and deferral, but its
reads do not become new dependencies of the caller. Roots are verified before
queries in their former contents: removing a collection member must not force
that obsolete member just to verify an old reader.

## Retained reference rounds

A round still answers `$referrers` from the previous frozen universe and uses
the specification's stability test. Retention avoids reconstruction where it
is safe; it does not remove the fixed-point check or change its iteration limit.

At a transition, compare observed inverse answers per target. An unchanged
target avoids invalidation when another target of the same relationship
changes. Compare observed frozen values and container shapes too. Forwarded
inverse references, frozen record views and deeper snapshot reads account for
their owner and snapshot age.

A root/member owns the records constructed beneath its canonical path. An
invalidated producer drops its registered descendants before rebuilding;
unaffected records and clean subtrees remain live. TypeScript and Python share
frozen slot maps and replace live slots with copy-on-write. Rust copies owned
snapshot slots and carries frozen reference owners through navigation.

The initial materialization defers reference answers. Dependency capture for
one-shot evaluation starts at settlement, then covers both phases of a reused
round. Programs without reference queries skip this bookkeeping. Dependency
sets allocate on the first read; cached field identities avoid repeated keys.

Diagnostics, unfinished slots, duplicate paths, unsupported snapshot ownership,
invalidated constants and unsafe root ordering trigger fresh evaluation for
remaining rounds. The shared Programs object survives this fallback. Final
validation always visits the complete result.

Each round builds its inverse-reference index once. A cache hit does not
reacquire or deep-copy the source edge. Taking a new snapshot clears the index.
Module callers serialize selected outputs rather than unused internal roots.

## Session edits

An update copies only document containers along its path. `Edits` compares old
and new supplied data down to the changed members, follows reverse dependency
edges, and prepares only affected queries for verification. Unaffected slots
stay cached without being reset or verified. An expression that produces
records also makes its descendants candidates, because their captured inputs
may change when the producer executes.

Root binding reconciles records with the same canonical path, type and parent.
An unchanged supplied record reuses its existing slots without rebuilding their
closures. Changed member inputs force their own queries; unaffected siblings
retain their identities. Create/remove, bind/unbind and undo/redo use this same
revision lifecycle.

Reference settlement starts with the edited inputs, never the previous settled
universe as its initial snapshot. Later transitions use the ordinary RoundCache.
Removed producers lose their instances, dependency entries and change stamps.
Assertions run during final validation, so their old dependency entries are
discarded before that pass.

Record-input metadata is weakly keyed in TypeScript. Python and Rust explicitly
prune it to live records, avoiding captured-scope back references that would keep
removed records alive. Rust's auxiliary unbound-literal materialization cache is
scoped to an edit; temporary expressions save and restore it with their request
programs. Session's intentional undo/redo history is separate from these caches.

## Boundaries and evidence

Revision preparation still walks live records, dependency edges and supplied
document data. Final validation is also a full walk. Small recomputation counts
therefore do not imply constant-time edit latency.

Whole-record comparisons conservatively prepare all values on edits to preserve
the fresh evaluator's handling of unforced members. A previous runtime diagnostic
reruns query programs during recovery; fine-grained diagnostic retention is not
implemented. Frozen/context-bearing results require safe ownership to be reused.
These are explicit remaining performance boundaries, not omitted language cases.

The shared internal data prove the optimized path as well as correctness:

- `programs.json`: 1 versus 128 instances leaves compilation counts unchanged;
  document and function cases match fresh values and diagnostics.
- `rounds.json`: additions/removals, constants, computed roots, nested snapshots,
  cyclic/unstable cases and result cutoff match fresh evaluation. Selected cases
  require several reused transitions and surviving records.
- `edits.json`: every edit matches fresh evaluation, including optional absence,
  captured contexts, aggregate comparison, reference rounds, diagnostic recovery
  and removal of obsolete dependencies. Work limits cover both executed programs
  and prepared queries.
- `temporary.json`: repeated expressions/errors and creation/removal at unique
  paths preserve results while auxiliary caches stay bounded by live work.

`make verify` runs the language tests and byte-for-byte CLI/REPL/LSP parity.
`qbench` measures preloaded batch work; the shared Session benchmark measures
apply, evaluate, validate and scalar serialization after initial loading.
See [performance results](../../../docs/PERFORMANCE.md) and the
[completion plan](../../../docs/OPTIMIZATION_PLAN.md).
