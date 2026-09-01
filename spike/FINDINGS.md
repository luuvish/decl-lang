# Evaluator Spike — Findings (ROADMAP §0.6)

Run: `node spike/src/run.ts` → **31 checks, 0 failures** across the
three benchmark cases: parse → bind → evaluate → validate → serialize →
round-trip. What follows is the evidence the spike exists for — what
only execution caught, and the OQ verdicts.

## Defects execution caught (spec amended for each)

1. **Nested literals could not reach their containers' names.** §7.3
   defined bare-name resolution as *sibling-only* (`$this.x` sugar) —
   under which the spec's own §7.4 example
   (`{ source: services[0] }` inside a links literal) does not
   resolve, and the interconnect case failed on every edge with
   "unknown name `ports`". The reading-level review never noticed;
   execution failed in the first minute. **Fix**: resolution walks the
   chain of enclosing instances, nearest first (§7.3, §4.2, D27).

2. **`$referrers` interacts with lazy evaluation.** A derived member
   (`width`) that reads a `$referrers`-derived sibling was forced
   during materialization, when the universe's edge instances did not
   all exist yet — and memoized an **empty referrer set**, silently
   breaking width propagation. Static "defers if it mentions
   `$referrers`" is insufficient: the dependency is transitive.
   **Fix**: normative rule in §7.6 — `$referrers` is answerable only
   after the universe is fully materialized; demand-driven
   implementations must defer the query and everything transitively
   reading it (the spike uses a defer-and-requeue signal).

3. **Integral floats broke the round trip.** Shortest-round-trip
   printing emits the float `30.0` as `30`, which is lexically an
   `int` and fails to re-bind where `float` is expected (the binding
   rule the spec itself fixes in §10.2). Every quantity magnitude hit
   this. **Fix**: §10.3/D29 — append `.0` when the shortest form is
   lexically an integer.

4. **`with` copying derived members trips the restatement rule.**
   `base with { tls: … }` carrying `base`'s computed `insecure` into
   the result made re-validation reject its own value the moment the
   update changed a dependency. Predicted during case-drafting,
   confirmed in execution. **Fix**: §4.12 — `with` does not copy
   derived members; they are recomputed downstream.

5. **The arbiter needed a width rule, not propagation.** The
   single-feeder propagation rule yields width 1 for any port fed by
   two wired edges — desk-checking missed it; tracing the 2x2 xbar
   found it before the first run. **Fix**: case 1 gives `Arbiter.mi`
   an explicit max-over-inputs derived override — and the fact that
   this is expressible as a one-line member override is itself
   evidence for the role-override design (§5.9 kind transitions).

## What the spike validated end to end

- **Width propagation as one derived member over `$referrers`** —
  replacing the legacy fixture's ~20 `@readonly` lambdas — flows
  correctly through master → decoder → arbiter(max) → slave →
  boundary, in both authored and re-bound documents.
- **Total round-trip** (D29): xbar serialized, re-bound under a
  *different* root name (document-relative `$.…` reference paths),
  re-validated clean, re-serialized **byte-identical**. Same for the
  32-case fixture sweep.
- **Root-cause discipline** (§6.6): a corrupted boundary width in a
  28-edge document produced **exactly one** `Edge.width_match` error;
  a config document with three defects produced exactly 2 errors + 1
  warning, with the warned value preserved in output.
- **Restatement** (D4): serialized derived members re-bind by
  equality; the engine had to learn not to *register* comparison-only
  instances (they would have doubled `$referrers` counts) — an
  implementation subtlety worth a note in Phase 2.
- Open-record opaque passthrough, `when`+`in` guards, nested `with`
  layering, comprehension grids, place equality, intersection layers
  (`EdgeHost &`), inline extension overrides, the newline-separator
  rule (including its parenthesis exemption), lexical int/float
  distinction in JSON binding.

## OQ verdicts

- **OQ3 (`std.graph.*`): not admitted in v0.1.** Every constraint in
  the ported corpus — the real interconnect fixture included — is
  one-hop. No reachability/acyclicity constraint appears; the legacy
  fixture never checked them either. Reserved namespace kept;
  re-entry requires new evidence (Phase 5's real-world corpus is the
  natural next test).
- **OQ7 (expression-level bindings): not re-admitted.** The corpus's
  most complex expression — arbiter width,
  `fold([p.width for p in values($parent.ins)], 1, (a, w) => max(a, w))`
  — remained a readable one-liner. No `func` body suffered.

## Spike limitations (untested surface — Phase 1/2 must cover)

Static type checking of expressions (the spike checks dynamically, so
the §4.10 static absence discipline and §3.17 subsumption algebra ran
only in their dynamic shadows); `match`; `|>`; generics; predicate
types `T(p)`; user `func` declarations; `?.`; modules/imports;
canonical-path bracketing of map keys (the spike prints identifier-
shaped map keys in dot form — consistent internally, not per §7.2);
`E2xxx` recovery behavior.
