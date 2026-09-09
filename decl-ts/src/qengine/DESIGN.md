# Query engine (a from-scratch incremental, memoized evaluator)

Status: **implemented in TypeScript, Rust, and Python; the default for source
reports and module-universe evaluation since 2026-09-09.** `DECL_QENGINE=0`
selects the tree walker. `DECL_QENGINE_STRICT=1` makes an unsupported form an
error instead of falling back. Differential tests explicitly select the tree
walker as their oracle; using the default source pipeline on both sides would
compare the query engine with itself.

The current implementation is a compiled evaluation layer over the existing
value model. Each `$referrers` round creates a fresh evaluator. `Db` implements
revision-based memoization, but the language layer does **not** yet preserve
that database across rounds or session edits. `Session` still uses `Engine`
and its existing dependency tracking. The persistent edit model below is a
design target, not a delivered execution path.

The Rust batch path stores compiled bodies in a round-owned arena; a record
slot addresses its body by an integer handle and a weak evaluator reference.
The slot already memoizes its value and handles cycles, deferral, and taint;
it does not need a second string-keyed memo of the same value. The arena also
avoids an ownership cycle between a record and a body capturing it. Constants
still use the query database. Map entries have an insertion-ordered index, like
the reference's `Map` and Python's dictionary. Resolved record members are
shared snapshots, with copy-on-write during recursive type construction;
binding a value or selecting a union arm does not copy the entire schema.
Performance representations may differ; observable behavior remains the shared
corpus and parity contract.

Every implementation builds a round's inverse reference index once and reuses
it for target lookups. Reacquiring the full edge on a cache hit is unnecessary
and, with an owned Rust edge, was a quadratic deep-copy cost. Taking a new
snapshot clears the index. Module callers request values and diagnostics and
serialize only their selected outputs; the query layer does not also build
unused JSON for every internal root.

Gates: `decl-ts/tests/qengine/`, the Rust and Python query tests, the shared
corpora, and `make verify`. The synthetic `decl-rs/examples/qbench.rs` compares
equal work (load, bind, evaluate, validate, serialize), checks output equality,
warms both paths, and reports medians. Parsing and static checking are excluded.

## Why

The tree walker dispatches over the expression AST on every force. Each
`$referrers` round then constructs a new universe and repeats pure work as well
as reference-dependent work. The current query layer compiles literal record
slots, but document binding and records produced by other expressions still
use the value layer's evaluator. Compiling a closure per instance and attaching
a second memo does not by itself remove either shared cost.

The persistent design therefore separates immutable schema programs from
instance state and makes structure and reference edges explicit dependencies.
Its goal is to reuse both code and unchanged values while preserving Decl's
rounds, diagnostics, and ordering. The batch benchmark also covers tagged
unions, so improvements to literal records alone do not stand in for the cost
of binding many values through a shared schema.

## Target shape for persistent evaluation

Three layers, sharing the current value model (`semantics.ts`: Value, RecInst,
Ref, types, subsume) and parser/binder structure where possible.

### 1. Compiled expressions
Each declaration's expression is compiled once (AST -> a closure `Op`,
`(cx: Cx) => Value`, or a small bytecode) so a force is a call, not an AST walk.
Variable references resolve to slot/const/param reads at compile time. The
compile is pure and cached per module.

### 2. Query graph + memo (the core)
A `Db` holds memo entries keyed by a `Query`:
- `slot(instId, member)`   a record slot's value
- `root(name)`, `const(name)`
- `edge(Type, member)`     the `$referrers` inverse index for a type/member
- structural: `nodes(id)`, `children(id)` as needed
Instances carry a stable **id** and are indexed by absolute path (O(1) lookup),
replacing per-force path string building.

`memo[query] = { value, deps: Query[], changedRev, verifiedRev }`.

```
query(q):
  m = memo[q]
  if m && m.verifiedRev == rev: return m.value          # already current
  if m:                                                  # re-verify deps
     if every d in m.deps has query(d).changedRev <= m.verifiedRev:
        m.verifiedRev = rev; return m.value              # EARLY CUTOFF
  push q; deps=[]                                        # (re)compute
  v = run(compiled[q])                                   # records deps via cx.query
  pop
  if m && eq(v, m.value): m.changedRev unchanged         # EARLY CUTOFF
  else m.changedRev = rev
  memo[q] = { value:v, deps, changedRev, verifiedRev:rev }
  return v
```

`eq` is structural value equality (the same identity §4.5 uses); early cutoff at
both the result (a recompute that yields the same value does not bump changedRev)
and the verify (unchanged deps skip recompute).

### 3. Rounds as revisions (§7.6)
`$referrers` answers from the previous round's universe. Map "round" to a
revision: round N answers `edge(T,m)` at revision N-1's values. The `edge` query
depends on the `member` slot of every candidate; when those change, the edge's
changedRev bumps, and dependents recompute — but only them (early cutoff keeps
the pure majority). A universe still changing after ROUNDS is E5009, detected by
the edge queries' changedRev never settling. This is the incremental round done
right: the memo/revision is the mechanism, not a bolted-on reset+sweep.

Structure that depends on referrers (a node/link exists only for a certain
answer) is handled because the queries that *produce* structure (a container's
`children`) are themselves memoized and re-verified; a structural query whose
inputs changed recomputes and yields a different instance set, and the edge
queries over it see the new set. No registry sweep — structure is derived, not
accumulated.

## Live edits (not yet wired to the query database)

The engine is demand-driven and memoized precisely so it handles *live changes*
to the universe — not only a batch evaluate. This is a first-class goal: an
interactive edit / diff / repl model where each edit recomputes only what it
reaches.

The document and every supplied value are the **input layer**; the entire
evaluation (slots, structure, referrers edges, outputs) is *derived* from it. An
edit is a mutation of the input layer, and re-querying recomputes only the slice
the edit reaches — the same revision + early-cutoff machinery, no re-evaluation:

- **value edit** (`:update g.nodes["b"].weight = 5`): the supplied value at that
  path is an input; `setInput` bumps its revision, and only queries that
  transitively read it recompute (a query whose recomputed value is unchanged
  cuts its dependents off).
- **structural edit** (`:create g.nodes["c"] = …`, `:remove g.edges[2]`): the
  container's supplied member set is an input; changing it recomputes the
  container's *derived* structure query — a different instance set — and the
  referrers edges over it. Structure is derived, never accumulated, so create and
  remove propagate without a stale registry (the failure that sank the earlier,
  in-place attempt).
- **rebind / unbind** (`:bind g { … }`, `:unbind g`): replaces (or clears) a
  root's input; its subtree recomputes, the rest is reused.
- **undo/redo**: the session restores a prior input layer; revisions advance and
  the memo re-verifies. The memo persists across edits — that persistence is the
  whole point of the incremental engine.

The session/REPL (`session.ts`) sits on top: it owns the operation log and the
input layer, applies each edit as input mutations, and asks the engine for the
affected answers. `tests/repl/incremental/` (a scripted create/update/remove/
bind/unbind/undo/redo session that must match a full recomputation at every
step) is the edit-model's differential target, beside the batch corpora.

`Db` already provides the mechanism: inputs, revision, verify + value cutoff. The
decl layer adds the input grammar (a supplied value per path) and the derived
structure queries; nothing about edits is special-cased — an edit is just an
input at a new revision.

## Byte-identical strategy
- Value equality, diagnostics order (§6.7), path canonicalization, referrers
  ordering, E5007/E5009 — reproduce exactly. The differential harness diffs this
  engine against `engine.ts` on every fixture/golden/repl/benchmark; a diff is a
  bug in this engine.
- Build order: (a) core Db/query/compile skeleton; (b) scalars + records + refs;
  (c) comprehensions, maps, unions/patterns; (d) `$referrers` rounds; (e)
  dimensions/units, modules, asserts/validation, render/session. Each stage
  widens the fixture subset the harness runs green.

## Remaining structural work

Compile schema programs once independently of record identity, and pass the
instance and lexical frame at execution time. Then represent supplied values,
container membership, and frozen reference edges as explicit inputs before
retaining memo entries across revisions. A cached answer must account for
every input it reads, including validation and failed or absent reads; a stable
path alone does not prove that the value at that path is unchanged.

Before session adoption, compare each edit, structure change, rebind, undo, and
redo against a fresh evaluation with identical outputs and diagnostics. Batch
speed alone is not evidence of correct incremental invalidation.
