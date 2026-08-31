# Evaluator Spike (ROADMAP §0.6)

A **time-boxed, throwaway** evaluator for a representative Decl subset —
the evidence gate before the v0.1 freeze. Nothing here is promised to
survive into the Phase 2 reference implementation.

Purpose (ROADMAP §0.6): errors caught by reading and errors caught by
execution are different sets. This spike executes the three benchmark
cases of `docs/examples/` end to end — parse → bind → evaluate →
validate → serialize → **round-trip** — and records what only execution
caught in [FINDINGS.md](FINDINGS.md), including the OQ3/OQ7 verdicts.

## Run

```bash
node spike/src/run.ts
```

(Node ≥ 23 — TypeScript runs natively via type stripping.)

## Scope

Implemented: the subset the benchmark corpus uses — record/union/range/
literal/pattern/map/intersection/extension types, type-level `else`,
the four member kinds, `assert`/`when`, `diagnostic` templates,
comprehensions, lambdas, `with`, `in`, `$this`/`$parent`/`$path`,
`$referrers`, place equality, `ref<T>` capture and transparent deref,
quantity literals and interchange form, input binding (closedness,
restatement, `$`-relative reference paths), taint/root-cause,
canonical serialization, byte-identity round-trip, and a working
newline-separator rule.

Not implemented (statically unexercised by the corpus, noted in
FINDINGS): `match`, `|>`, generics, predicate types `T(p)`, user
`func` declarations, `?.`, static type checking of expressions (the
spike checks dynamically), modules/imports, `decl.toml`.
