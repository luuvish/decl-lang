# Query engine (a from-scratch incremental, memoized evaluator)

Status: **WIP, parallel to the current engine.** Built and validated
differentially against `engine.ts` on every corpus; it replaces the current
evaluator only once byte-identical everywhere, and then is ported to Rust and
Python. Until then it ships nothing and changes no observable behavior.

## Why

The current evaluator is a fresh-engine-per-round tree walker. Two costs it
cannot shed (measured, F21):
1. It re-walks the expression AST on every force (dispatch per node).
2. `$referrers` runs the whole universe in rounds; a round redoes ~85% pure work
   (proven), yet bolting incremental reuse onto the tree walker did not pay off
   — its per-round cost is the tree WALK + edge computation + per-read taint
   bookkeeping, not the pure-slot compute we could skip.

A demand-driven, compiled, memoized **query** engine is a different *kind* of
evaluator and sheds both costs: expressions compiled once to bound rules, a
memoized query graph with revision + early cutoff, an absolute-path index, and a
compact typed runtime. This engine adopts that architecture for the full decl
language, staying byte-identical to the spec.

## Shape

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

## Live edits (the incremental core)

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

## Non-goals for stage 1
No Rust/Python port until the TS reference is byte-identical on the full corpus.
No swap-in until then. This document is the blueprint; it will move to
`docs/design/` when the approach is proven.
