# 09. Evaluation Semantics

This chapter fixes how evaluation happens: the pipeline, the dependency
graph, laziness, determinism (including the numeric rules that make
bit-identical results possible), invalidation, partial evaluation, and
the termination argument. Everything here serves P2 and P4: evaluation
is pure, deterministic, terminating, and always yields
*(resolved values, diagnostics)*.

## 9.1 The pipeline

Evaluation proceeds in stages (D23):

```
parse → import resolution → name resolution → type check
      → dependency analysis → evaluation (lazy) → constraint validation
      → emission
```

- **Diagnostics persist**: a diagnostic raised at any stage remains in
  the final list; later stages never erase earlier findings.
- **Stages are resilient, not gated** (P6): a failure affects what
  depends on it, nothing more. A parse error in one declaration leaves
  the rest of the module parsed and analyzed; an unresolved import
  invalidates the names it would have bound; a type error taints its
  value exactly as a constraint error does (§9.7). "One defect, one
  report, minimal blast radius" applies pipeline-wide.
- The boundary between *compile-time* diagnostics (parse, names,
  types — reportable without any input bound) and *evaluation-time*
  diagnostics (evaluation errors, constraint failures) is observable
  to tools; the report format is shared
  ([12. Errors](12_errors.md)).

## 9.2 The evaluation universe

The **universe** of an evaluation is the set of evaluation roots — all
`output`s and all bound `input`s — of the module set: the entry modules
the tool was invoked on plus their transitive imports (§8.8). The
universe is what `$referrers` ranges over (§7.6) and what constraint
validation covers (§9.4); it is fixed before evaluation begins and does
not depend on which results the tool ultimately reads. Root names are
unique across the universe (§8.8).

## 9.3 The member dependency graph

- For each record instance, its members' default, derived, and
  constraint expressions reference siblings, context values, and other
  roots' values. These references, instantiated **per instance**, form
  the dependency graph.
- The graph must be **acyclic**. A cycle is a compile-time error when
  it is visible in the type structure (a member's expression reaching
  itself through the same type's members) and an evaluation-time error
  otherwise (e.g. through references bound from input); every member
  on the cycle is reported once, with the cycle spelled out.
- **A cycle never aborts evaluation.** Detection is local: lazy
  evaluation marks a member *in evaluation*, and a demand re-entering
  an in-evaluation member is the detection point — the members on the
  cycle become invalid with one diagnostic naming the cycle, the
  demand unwinds, and everything not on or downstream of the cycle
  evaluates and validates normally (§9.7). There is no "run failed"
  state: the result is the ordinary pair — all valid values plus the
  cycle diagnostic. A partial-evaluation request whose minimal set
  contains the cycle returns invalidity with that diagnostic under
  the ordinary §9.8 contract.
- Reference *values* may form cycles freely (§7.5) — the constraint is
  on **evaluation dependencies**, not on the shape of the data:
  `a.next` pointing at `b` while `b.next` points at `a` is a graph;
  `const x = y` with `const y = x` is a dependency cycle.

## 9.4 Lazy evaluation, observational equivalence

- Evaluation is **demand-driven with memoization**: a member is
  computed at most once per instance, when first needed; results are
  shared thereafter.
- What creates demand: emission of an exported output; validation
  (every constraint member of every instance in the universe is
  checked, and checking demands the values its condition reads);
  serialization (demands every value member, including derived ones);
  a partial-evaluation request (§9.8); the fallback of an `input`
  demanded while unbound (§5.6).
- An unbound `input` that nothing demands raises no diagnostic; the
  moment something demands it, the missing binding is an error at the
  demanding path.
- **Observational equivalence**: the *(values, diagnostics)* result is
  identical to that of a hypothetical eager evaluation in declaration
  order — laziness affects cost, never outcome. In particular, an
  expression that would error is an error whenever anything demands
  it, and no error is observed from members nothing demands (there are
  none under full validation except unbound-undemanded inputs and
  overridden defaults, §5.7).

## 9.5 Determinism

Given the same module set, lock state, and input bindings, a conforming
implementation must produce **byte-identical serialized values and an
identical, identically ordered diagnostic list** (P2). The rules that
make this achievable:

- **Integers** are arbitrary-precision and exact; `/` truncates toward
  zero, `%` takes the dividend's sign, shifts are defined on infinite
  two's-complement (§4.4). No numeric environment exists to vary.
- **Floats** are IEEE 754 binary64, round-to-nearest-even, one rounding
  per operation as written: **no FMA contraction, no
  extended-precision intermediates, no re-association or other
  value-changing rewrites** — `a + b + c` is `(a + b) + c` and nothing
  else. Constant folding must produce the same bits as runtime
  evaluation.
- **Quantities** normalize to their dimension's base unit for
  comparison, equality, and serialization; conversion factors are
  exact rational scalings evaluated in arbitrary precision before the
  final rounding to binary64.
- **Order is specified everywhere** (D23): module members in
  declaration order; input-bound entries in document order;
  object/map entries preserve insertion order and duplicate keys are
  errors; comprehensions iterate in source order; `$referrers` results
  in canonical path order; diagnostics sort by `(path, id)` (§6.7).
- Nothing else is implementation-observable: memoization, evaluation
  order, and parallelism are invisible by §9.4.

## 9.6 Numeric safety

Recapping D24 with this chapter as the normative home: no value is ever
NaN or ±Infinity. A float operation whose IEEE result would be either,
and any division or remainder by zero (int or float), produces an
**evaluation-error diagnostic** at the demanding member's path instead
of a value; the member is then invalid (§9.7). Grounds: determinism
(P2) and the JSON round-trip (D29) — the value domain contains nothing
JSON cannot carry.

## 9.7 Invalidation, operationally

- A member becomes **invalid** when its own evaluation raises an
  error-severity diagnostic (type failure, evaluation error, failing
  `assert` with error severity at its instance).
- Demanding an invalid value makes the demander invalid **silently** —
  taint propagates along the dependency graph with no further
  diagnostics; constraint conditions touching invalid values are
  skipped silently (§6.6). Invalid values are excluded from emission
  ([10. Interchange](10_interchange.md)) and from `$referrers`
  candidacy (§7.6).
- Warnings and infos invalidate nothing.
- The final result is always the pair: every valid value, plus the
  root-cause diagnostics — partial validity is the normal shape of a
  failed run, not an exception.

## 9.8 Partial evaluation

A tool may request any canonical path (D25):

- The implementation evaluates the **minimal dependency set** for that
  path and returns its value (or its invalidity) together with the
  diagnostics arising from that set only. The result is explicitly
  marked **partial**.
- A partial result never speaks for the whole document. The classes of
  diagnostics partial evaluation cannot produce, enumerated: assertion
  failures of unevaluated instances; type and evaluation errors of
  undemanded members; unbound-input errors for inputs the set does not
  demand; dangling-reference errors of references the set never
  resolves. A whole-document verdict is obtainable **only** from full
  validation — using a partial pass as "is this valid?" is a specified
  misuse.
- Incremental tooling premise (P6): partial results are memoizable and
  re-usable across edits by dependency tracking; the observational
  rules of §9.4 make any such reuse invisible.

## 9.9 Termination

Every evaluation terminates, by construction, with no appeal to fuel or
limits:

1. Function reference graphs are acyclic (§5.3) — no recursion.
2. Comprehensions iterate finite arrays and bounded int ranges (§4.8).
3. The member dependency graph is acyclic (§9.3), and memoization
   evaluates each member at most once per instance.
4. Reference navigation is demand-driven through already-evaluated or
   in-evaluation members; reference cycles never induce evaluation
   cycles because navigation demands *values*, which sit on the
   acyclic dependency graph.
5. Constraint validation visits each (rule, instance) pair exactly
   once over the finite universe.

Should the fixpoint combinators of D18 be admitted (OQ3), they preserve
this argument: least fixpoints of monotone operators over the finite
universe reach their fixed point in finitely many steps, and the
recursion stays inside the combinator.

## Open questions

None.

---

## Previous / Next

- Previous: [08. Modules and Packages](08_modules.md)
- Next: [10. Data Interchange](10_interchange.md)
- Index: [Documentation home](../README.md)
