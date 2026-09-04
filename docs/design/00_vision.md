# Decl: Vision and Background

Why this language exists, what it must achieve, and what its previous
iterations taught us. This is the founding document of the design series;
[requirements](01_requirements.md) and design decisions build on it.

- **Source**: distilled from a design-review conversation of 2026-08-14,
  shared at <https://claude.ai/share/b9bcc143-8007-48e7-8519-303d7af6e351>
- **Status**: informative background for the spec rewrite. Not normative.
- **Note**: all section numbers (§) and decision numbers (D-numbers, E-codes)
  below refer to the **previous Decl spec iteration**, which this
  repository deliberately did not migrate. They are cited as evidence of
  what the new spec must fix or decide differently.

The conversation spans four rounds: (1) analysis of an article on the future
of configuration languages, (2) a from-scratch language sketch ("Weft"),
(3) a detailed review of the previous Decl specification, and (4) a final
assessment with a recommendation. This document preserves the substance of
each round.

---

## 1. Context: "The Next Generation of Configuration Languages" (article)

An August 2026 article (6 sections, 13 references) whose argument frames the
whole conversation.

**Current landscape.** YAML's fundamental defects (no types, the Norway
problem, string-template assembly, no reuse mechanism) produced three
generations of responses: 1st — escape from templates (Jsonnet, Dhall, HCL);
2nd — types and validation (CUE's lattice-theoretic unification, Nickel's
gradual typing + contracts); 3rd — pragmatism (Pkl's familiar syntax and
polyglot code generation). The 2025–2026 releases share three trends:
performance bottleneck removal, LSP-first tooling, embeddable libraries.
Conclusion: no winner yet.

**The agent-era turn.** Leveraging Octoverse 2025 (TypeScript #1) and the
finding that ~94% of LLM compile errors are type-check failures, the article
recasts type systems as the safety net for agent output, across four layers:
output (constrained decoding), input (context engineering; serialization
format itself was not significant, p = 0.484), interface (MCP), platform
(semantic layer → queryable context store).

**Why formal verification never went mainstream — five bottlenecks:**

1. Mode-of-thought gap: procedural tracing vs constraint-based reasoning.
2. Non-local failure and blame assignment.
3. Configuration is read by the weakest reader at the worst moment
   (the 3 a.m. incident).
4. Cost/benefit asymmetry.
5. **The specification problem** — is the spec really what I meant?

Rust/Elm serve as counterexamples that a learning curve is offset by
feedback-loop quality. Agents largely mitigate ① and ②, but sharpen ⑤.
"Only the specification problem remains" is the article's axis.

**Configuration languages are state-management languages.** With *Out of the
Tar Pit* as lineage: Kubernetes/Terraform already manage state, but the
diff/reconcile layer hides in imperative code outside the language. The
article maps DBSP (incremental computation), event sourcing / bitemporality,
CRDTs, and lenses (Cambria) into a grid — "CUE in the static world and CRDT
in the dynamic world are siblings" — and notes that **no single language
spans the grid**. Agents demand exactly three properties — **diffable,
mergeable, verifiable** — corresponding to those theories. The next
generation is "a language that derives verified change from a declared
model", and competitiveness will be decided not by constraint expressiveness
but by the interface through which humans reflect their intent back.

---

## 2. The "Weft" sketch (drawn before reading Decl; later withdrawn)

A from-scratch design (7 principles → 12 features) targeting the article's
two gaps. Key ideas worth remembering even though the sketch was discarded:

- **User-defined lattices (F1)**: where CUE hard-codes one unification, users
  define lattice instances and the compiler imposes proof obligations
  (commutativity, associativity, idempotence). CRDT convergence (F6) then
  falls out as a corollary rather than a feature.
- **Correction from round 2**: definition composition `&` (intersection of
  allowed sets) and state merge (CRDT join) are *different operations that
  satisfy the same algebra*. Precisely stated: CUE's value lattice already
  *is* a state-based CRDT (unification is commutative, associative,
  idempotent, monotone in the information order — the CvRDT definition).
  Convergence is free (to ⊥); the real task is deciding *at compile time*
  which fields can converge to ⊥.
- **Residual constraints as constrained decoding (F12)**: on a lattice,
  the "remaining constraint" on a partial value is computable — i.e. a
  constrained decoder that pushes *semantic* constraints into decoding,
  where JSON-Schema-based constrained decoding stops at syntax. Named as the
  most concrete reason such a language should exist in the agent era.
- **diff-meaning (F11)**: computing subsumption (⊑) between two schema
  versions — "the allowed set narrowed" — impossible with text diff,
  derivable only from lattice structure. If bottleneck ⑤ is what remains,
  competition is decided by review cost, not expressiveness.
- **Honest difficulty**: constraints × CRDT is theoretically unresolved —
  two individually legal writes can violate an invariant after merge
  (I-confluence). The sketch surfaced this via a mandatory `@coordinated`
  marker instead of hiding it.
- **First killer domain (user decision): the data semantic layer.** Weft
  there is a *definition language* compiled to SQL, not a query engine. Five
  representative semantic-layer errors (fan-out double counting, summing
  non-additive metrics, mixing currencies, time-grain misuse, definition
  drift) are all syntactically valid, schema-passing, and silently produce
  wrong numbers — a ready-made answer to "why not just JSON Schema".
  User-defined lattices turned out to be load-bearing immediately: units
  (flat), grain (union), additivity (intersection).
- **Open kernel question**: whether `from` (the join graph) belongs in the
  kernel — grain derivation needs it, and it decides how much of the
  relational model enters the core.

**Final verdict on Weft** (round 4): overreach — deltas, bitemporality,
CRDTs, and lenses are four research problems in one language. Discarded.

---

## 3. Review of the previous Decl specification

### 3.1 Strengths (to carry forward)

- **One source, three outputs** — schema, resolved values, diagnostics; the
  design's center is that evaluation always yields *(value, diagnostics)*.
  Making partial validity a normal output instead of an error path is the
  spec's most undervalued idea — agents mass-produce "almost right".
- **Provenance traceability as a reinforcing rule set**: no wildcard
  imports, no shadowing anywhere, exact-pin versions only, content-hash
  fail-closed. "Where did this name come from" is always answerable.
- **Diagnostics as first-class**: stable id `<module>.<type>.<assert>`;
  id survives condition-text edits; code/id immutable while messages are
  mutable; append-only numbering; deterministic `(path, id)` ordering; and a
  taint rule that reports only root causes, so one error doesn't amplify
  into dozens of cascading diagnostics.
- **Rare determinism rigor**: FMA contraction and x87 extended precision
  explicitly banned; NaN/Infinity removed from the value domain in favor of
  evaluation-error diagnostics.
- **D15 type/value separation** (type after `:`, value after `=`): rejecting
  CUE's type=value fusion is what lets schema-layer errors be reported at
  the schema layer.
- **D17-style "syntax we do not have" table** and the requirements doc's
  falsifiable generality benchmark (§4: hardware interconnect without domain
  keywords) — both worth reproducing in the new spec.

### 3.2 Defects found (the new spec must address each)

**Round-trip / input defects (3):**

1. **Quantities break round-trip and input binding.** §10.6 serialized
   `quantity<D>` as a `{value, unit}` object, but the data-value grammar
   (§11.6) had no QuantityLiteral and no rule typed that object as
   `quantity<D>` (§3.13). Consequence: serialized output cannot re-bind
   (violating §10.8's normative idempotence), and *any schema using units
   cannot validate external JSON at all* — the unit system (D6) collides
   with the core `input` path (D12). The spec's own end-to-end example
   avoided quantities.
2. **References could target unvalidated values.** §7.3(a) allowed
   module-level `let` as a reference target while §5.2/D13 defined `let` as
   a pure constant that never runs the schema pipeline — so `&Port` could
   point at a value never validated as `Port`. The serialization path for
   such references was also undefined (§6.6 rooted paths at
   `instance`/`input` names only).
3. **Structural compatibility ignored two of the four member kinds.** The
   assignability rules (§3.9.3) covered required/optional/closedness but
   were silent on default members (`x: T = e`) and derived members
   (`let x = e`). The guide's own `let base: Service = {...}` stepped into
   the hole; the same judgment is needed for function parameter types and
   union discrimination.

**Undefined semantics (3):**

4. **`float<32>` had no semantics.** D6 said "allowed" in one line; §3.2.4
   deferred to ch. 9; §9.9 fixed binary64 only. Reading it like `int<N>`
   ("range refinement, no implicit truncation") would make
   `let ratio: float<32> = 0.1` an error, since 0.1 is not exactly
   representable in binary32 — presumably unintended, so a separate rule is
   required. Untracked in any open-questions list.
5. **`$path` inside refinement predicates** contradicted the stated
   rationale (§7.6) that refinements are position-independent and reusable.
6. **`$refs` ordering for input-bound values was undefined** (§7.7 fixed
   declaration order within modules only).

**Design reconsiderations (2):**

7. **Graphs can be built but not traversed.** `&T` forms cyclic graphs
   (`type Node = { next?: &Node }` was a canonical example), but recursion
   is banned (D3) and `$refs` is one hop — so reachability, acyclicity, and
   connectivity are inexpressible. The generality benchmark (hardware
   interconnect) is a graph domain whose representative constraints are
   exactly reachability and acyclicity: the benchmark may fail its own test.
   **Fix direction that keeps totality**: the evaluation world is finite, so
   least fixpoints of monotone operators terminate (the Datalog argument);
   a stdlib finite-fixpoint operator (e.g. `std.graph.closure(T, "prop")`)
   opens transitive queries with recursion confined inside the combinator
   and the user call graph still acyclic — same structure as `fold` giving
   iteration without loops. (Was drafted as candidate "D22".)
8. **No order-independent composition (conjunction).** Intersection `A & B`
   was consciously excluded (inheritance + open records as substitutes),
   but that substitute covers *extension*, not *conjunction* of independent
   constraint sets. Applying a security baseline and a region policy to the
   same type forces an artificial total order (`Base → Secured →
   SecuredEU`); `std.object.merge` is right-biased with wholesale array
   replacement — order-dependent, in a language otherwise obsessed with
   order independence (the article's dev/staging/prod problem, exactly).
   Half of the exclusion rationale was a token clash with reference type
   `&T` — a semantic decision justified by a syntax problem; if the real
   reason is complexity, the record should say so.

**Internal contradiction (1):**

9. **Closedness vs subtyping.** §3.9.3(3) forbade undeclared members on a
   closed record while §3.9.4 claimed `Child = Parent { label: string }` is
   a subtype of `Parent` — not simultaneously satisfiable. **Fix direction**
   (was drafted as candidate "D21"): redefine closedness as a
   construction/binding-time check rather than a subtyping property — keeps
   unknown-field detection on `input`, restores the subtype claim, and
   settles the record rules any future intersection type would need.

**Tooling-semantics gap:** partial evaluation surfaces no diagnostics for
unevaluated members, so using it as an agent's "is this valid?" channel
produces false passes.

---

## 4. "Proposal v2" — open subsumption instead of a new language

The key observation: the previous Decl *already computed subsumption* — to
detect widening in inheritance (its E3011), the checker must decide "is T′
narrower than T". It exposed the capability only as a yes/no compile check on
one inheritance edge. Opening ⊑ as a first-class, queryable operation makes
these one procedure with five exposures:

- intersection/conjunction types,
- residual constraints ("what values can still go here" — the constrained-
  decoding hook),
- semantic diff between schema versions (diff-meaning),
- boundary example generation,
- the existing inheritance-narrowing check.

Together with the two candidate decisions above (closedness redefinition;
finite-fixpoint operators), this was the whole of the proposal: use a
capability the language already has once, five times.

---

## 5. Final assessment and recommendation

**"Three specs, zero implementations."** UDCL → Decl → this repository is
the third spec-stage iteration; no evaluator has ever run. The defects found
(§3.9.3/§3.9.4 contradiction, quantity input, `float<32>`) are precisely the
kind an implementation surfaces within days — implementing assignability
immediately exposes the two unspecified member kinds; implementing `input`
binding hits the quantity wall at once. The spec was *more* precise than many
shipped 1.0 language docs; but errors caught by reading and errors caught by
execution are different sets, and only the first set had ever been swept.
Bottleneck ⑤ applies to the project itself: no spec has met a real workload,
so "is the spec what we meant" has no evidence behind it.

**When a new language is right**: only when the target domain changes.
Tree+reference vs relational is a kernel decision, not an extension — a
semantic layer needs grain derived from a join graph, and no revision of a
tree-kernel language arrives there. But that is a *domain* decision, not a
language decision. (The UDCL → Decl transition had evidence: real fixtures
and a Pkl/Nickel comparison. A third language needs comparable evidence.)

**If a new language is built anyway**, the one genuinely novel combination:
**relational kernel + finite fixpoint + (value, diagnostics) + first-class
subsumption** — the four interlock (finite world ⇒ monotone fixpoints
terminate ⇒ relations without giving up purity/determinism/termination), and
none of CUE/Pkl/Decl occupies that spot. Must-keeps from Decl regardless:
the *(value, diagnostics)* universal result type and the type/value surface
separation.

**Recommended next step — build evidence before choosing**: write a
time-boxed minimal evaluator for a Decl subset and run it against a real
interconnect fixture. Three purposes: sweep the defects reading cannot catch; empirically judge the
generality benchmark; own a real corpus for whatever is built next. The
stated prediction: the interconnect case is unusable without transitive closure — if
true, evidence for kernel-level relations; if false, evidence the fixpoint
extension is unnecessary. Either way, far cheaper than a fourth spec.

---

## 6. Implications for this rewrite (summary checklist)

Decisions and defects the new spec should explicitly resolve, with the old
spec's failure mode as the test:

1. Quantity serialization **and** input binding — round-trip idempotence
   must hold with units in play.
2. Reference targets — everything a `&T` can point at must have passed the
   `T` pipeline; serialization paths defined for all legal targets.
3. Assignability rules must cover **all** member kinds (required, optional,
   default, derived) — same judgment reused by function params and unions.
4. `float<32>`: define semantics or drop it.
5. Context variables allowed in refinement predicates: decide and justify
   (position-independence of refinements).
6. Ordering guarantees for reverse-reference queries over input-bound data.
7. Transitive/fixpoint queries over reference graphs: in or out, decided
   with evidence (the interconnect fixture), not by default.
8. Order-independent conjunction of constraint sets: adopt, or record the
   real (semantic, not syntactic) reason for exclusion.
9. Closedness defined so it cannot contradict subtyping.
10. Partial evaluation semantics must state what diagnostics it does *not*
    produce.
11. Consider exposing subsumption (⊑) as a first-class query — it underlies
    narrowing checks, semantic diff, and residual constraints.
12. Preserve: *(value, diagnostics)* result type, type/value surface
    separation, provenance traceability rules, first-class diagnostics with
    stable ids and root-cause-only taint reporting, determinism rigor.
