# Decl: Design Requirements

What the language must achieve, what it deliberately will not do, and the
quality bar the specification has to clear. This document builds on
[00. Vision and Background](00_vision.md) — in particular, the defect
checklist in its §6 is treated here as requirements, not as suggestions.
Numbered design decisions (02, to be written) must satisfy this document;
where they cannot, this document is revised first.

References of the form *(V n)* point to checklist item *n* in
[00. Vision §6](00_vision.md).

## 1. Goals

Decl is a **general-purpose declarative language** providing three
capabilities as one language:

1. **Describe** — declare the specification of structured data: types,
   schemas, constraints, and diagnostics, in a form people can read and
   review.
2. **Generate** — deterministically produce values from the specification:
   evaluate defaults, derived properties, and comprehensions into a fully
   resolved value tree, exportable to standard formats such as JSON.
3. **Validate** — judge whether values satisfy the specification: both
   values defined in the language and externally injected data (JSON),
   yielding a list of diagnostics with stable ids.

The three are not separate modes but three views of a **single evaluation
semantics**. Evaluation always produces the pair *(resolved values,
diagnostics)* — partial validity is a normal output, not an error path.
This is deliberate: the language's consumers include agents and tools that
mass-produce and iterate on "almost right" data, and they need the values
*and* the judgment together (00_vision §3.1, §5).

The language serves three consumer classes, in this priority: **human
reviewers** (the weakest reader at the worst moment), **tools** (formatter,
LSP, diff), and **agents** (generation and repair loops). A design that
helps the third at the cost of the first is rejected.

## 2. Non-goals

- **No domain-specific features in the language.** Concepts belonging to a
  particular domain (hardware, networking, configuration management, …) —
  e.g. connection-based value propagation, port/wire element kinds, RTL
  template binding — are never keywords or built-in semantics. Users express
  them at the library level with types, derived properties, functions, and
  constraints.
- **Not a general-purpose programming language.** No I/O, no mutation, no
  unbounded loops, no exception control flow. Every evaluation is pure and
  terminates.
- **Not a relational/query kernel.** The v0.1 data model is trees plus
  references, not relations; there is no join graph in the kernel and the
  language is not a query engine. This is a conscious domain decision, not
  an oversight: a relational kernel (as a data semantic layer would need) is
  a different language, and switching is justified only by a change of
  target domain with evidence behind it (00_vision §5).
- **Does not replace natural-language specification documents.** A Decl file
  can be the executable expression of an agreed spec, but which artifact is
  the final authority on meaning is the adopting project's decision.
- **Template/rendering engines are not language core.** Rendering evaluated
  results into arbitrary text is tooling-layer work. The language is
  responsible up to the resolved value tree and diagnostics.

## 3. Requirements

### 3.1 Describe (schema definition)

- Primitive types: `null`, `bool`, `string`, arbitrary-precision `int`,
  `float`
- Bit-width-restricted numeric types (e.g. `int<N>`, `uint<N>`); if a
  reduced-width float type is offered, its assignment and rounding semantics
  must be fully specified — a type form with "allowed" as its only rule is
  inadmissible *(V4)*
- Literal types, and enumeration via unions of literals
- Pattern (regular-expression) string types, with pattern interpolation
- Refinement types: a value predicate participating in the type
- Composite types: closed and open records, arrays (with size ranges), maps
  (typed keys and pattern keys)
- Tagged unions with structural discrimination and exhaustiveness checking
- Generics: type parameters and value parameters
- A unit/dimension (quantity) system for type-safe arithmetic on physical
  quantities — admitted only together with its serialization and input
  story (see 3.2, 3.3) *(V1)*
- Member kinds: required / optional (absent-able) / defaulted / derived
- Schema inheritance with member refinement (narrowing only)
- **Assignability is total over the surface**: the structural compatibility
  judgment must be defined for *every* member kind — required, optional,
  defaulted, derived — and the same judgment must serve object-literal
  assignment, function parameters, and union discrimination *(V3)*
- **Closedness must not contradict subtyping**: the definition of a closed
  record and the claims of the inheritance/subtype rules must be
  simultaneously satisfiable; if closedness is a construction/binding-time
  check rather than a subtype property, the spec must say so explicitly *(V9)*
- **Conjunction of independent constraint sets**: it must be possible to
  apply two independently authored constraint layers (e.g. a security
  baseline and a region policy) to the same type without forcing an
  artificial linear inheritance order — or the design decisions must record
  the *semantic* reason for excluding this, not a syntax clash *(V8)*

### 3.2 Generate (value evaluation)

- Derived properties: computed from other properties, evaluated in
  dependency topological order; reference cycles are errors
- Defaults: omitted properties filled by expression evaluation, with access
  to other properties
- Deterministic construction of substructure via comprehensions
- Record update and merge with explicit, specified semantics (bias and
  conflict rules)
- Canonical serialization (JSON mapping): absent properties are not
  emitted; `null` is emitted
- **Round-trip idempotence is normative and total**: serializing an
  evaluated result and re-binding it as input must succeed and validate for
  *every* value the language can produce — including quantities and
  references. Any type that can appear in output must have a defined input
  form *(V1)*
- Determinism: the same input yields the same values and the same
  diagnostics, bit-identical across conforming implementations

### 3.3 Validate (constraints and diagnostics)

- Value constraints (range, pattern, enumeration) expressed as refinement
  types
- Structural constraints: cross-field invariants as assert members
- Relationship constraints: reference integrity, cross-entity checks on
  referenced values, uniqueness
- **References only to validated values**: everything a reference type can
  legally point at must have passed that type's full pipeline, and every
  legal reference target must have a defined serialization path *(V2)*
- Conditional constraint groups
- Diagnostics as first-class declarations: stable id, severity
  (error/warning/info), parameters, message template
- Errors invalidate the affected value (and its dependents) with
  **root-cause-only reporting** — one defect must not amplify into cascades;
  warnings preserve values
- Every diagnostic carries its occurrence path automatically
- External data validation: bind a JSON document to an input declaration and
  validate under exactly the same rules as language-defined values
- **Deterministic ordering everywhere**: diagnostic order, member order, and
  the order of reverse-reference query results are specified — including
  over input-bound data, not just declared modules *(V6)*
- **Graph-shaped constraints**: the hardware-interconnect benchmark (§4)
  requires reachability- and acyclicity-class constraints over reference
  graphs. The spec must either provide a totality-preserving mechanism
  (e.g. finite-fixpoint combinators in the standard library) or demonstrate
  via the evaluator spike that the benchmark passes without one; silence is
  not an option *(V7)*

### 3.4 Modules and reproducibility

- File = module; explicit export/import; no wildcard namespace injection
- Name provenance is always answerable: no shadowing, named re-export only
- Package manifest with **exact version pinning + lock file** (content
  hashes, fail-closed)
- Deterministic module resolution: same lock state, same resolution
- Tooling premise: evaluation results can be stamped with the version/hash
  of the specification used

### 3.5 Tooling and agent consumers

- A surface fit for human review: one concept, one syntax; the grammar
  leaves no stylistic choices that a formatter cannot canonicalize
- Parser resilience: a partially broken file still yields diagnostics for
  the rest
- Lazy evaluation and dependency tracking: incremental re-validation
- **Partial evaluation with an honest contract**: evaluating a chosen path
  yields that path's value and diagnostics, and the spec states explicitly
  which diagnostics partial evaluation does *not* produce — so a tool or
  agent cannot mistake a partial pass for whole-document validity *(V10)*
- Diagnostics are machine-readable: stable ids and paths support
  cataloguing, localization, and automated repair loops
- The type checker's subsumption judgment ("is T′ narrower than T") SHOULD
  be exposed as a queryable operation rather than only an internal yes/no
  check — it is the single procedure underlying narrowing checks, semantic
  diff between schema versions, and residual-constraint queries; whether
  and how far v0.1 exposes it is a numbered design decision *(V11)*

## 4. Validation cases (the generality benchmark)

Generality is verified by one falsifiable criterion: **can a domain spec be
written with no domain keywords?** Representative cases:

- **Hardware interconnect**: node/port/edge graph, bit-width parameters,
  owner-dependent constraints, value agreement across connection endpoints —
  expressed with records, references, derived properties, and asserts. The
  real fixture `../decl-lang/tests/validation/customs/oic.decl` is the
  concrete target.
- **API/config schema**: open records, default filling, per-environment
  override.
- **Test fixture generation**: comprehension-parameterized instance
  generation.

Two levels of evidence are required (ROADMAP §0.5–0.6): a desk check of all
three cases against the spec alone, and an **executable pass** — a
time-boxed throwaway evaluator running the cases and binding the real OIC
fixture as input. Reading-level review alone does not discharge this
section; the previous iteration froze three specs on reading alone
(00_vision §5). If a case needs a language extension, the extension must
first pass the generality test — otherwise it goes to the standard library
or user land.

## 5. Quality bar

- Every normative rule is accompanied by an example and a counterexample.
- Every error condition has an assigned error code.
- If the formal grammar and chapter prose diverge, the grammar chapter wins
  — and the divergence is fixed immediately upon discovery.
- **No partially specified features**: a construct is admitted into the
  spec only together with its complete assignability, evaluation, and
  serialization rules. "Allowed, semantics later" is how the previous
  iteration accumulated holes *(V4)*.
- **The vision checklist is closed out**: before v0.1 freeze, every item in
  [00. Vision §6](00_vision.md) is resolved by a named chapter section or a
  numbered design decision — including the items whose resolution is a
  documented rejection.
- **Freeze requires execution**: v0.1 is not declared frozen until the
  evaluator spike (ROADMAP §0.6) has met the spec with every finding
  resolved.

---

## Previous / Next

- Previous: [00. Vision and Background](00_vision.md)
- Next: [02. Design Decisions](02_design_decisions.md)
- Index: [Documentation home](../README.md)
