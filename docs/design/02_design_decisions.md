# Decl: Design Decisions

This document fixes every design decision of Decl under a stable number.
Specification chapters cannot be written in conflict with it; on discovering
a divergence, either the chapter is corrected to match this document or this
document is revised first and the chapter follows.

**Method.** These decisions are derived from
[00. Vision and Background](00_vision.md) and
[01. Design Requirements](01_requirements.md) — not carried over from the
previous iteration's decision list. Where a decision resolves a defect of
the previous iteration, the citation is to the vision checklist
([00 §6](00_vision.md), written *(V n)*), and the resolution is argued on
its own merits. Outcomes that coincide with the previous iteration do so
because the same requirements produce them, not because they were inherited.

---

## Design principles

- **P1. Generality** — no domain keywords. Domain semantics are expressed as
  libraries: types, functions, constraints, derived properties. Every
  feature proposal must first pass "does this concept mean something outside
  one domain?"
- **P2. Pure, deterministic, terminating** — expressions have no side
  effects; recursion and unbounded iteration are impossible, so every
  evaluation terminates. The same input yields the same values and the same
  diagnostics in every conforming implementation.
- **P3. JSON superset** — every JSON document is a valid Decl value literal.
  External data becomes a language value without conversion.
- **P4. One semantics, one result shape** — describe, generate, and validate
  are three views of a single evaluation semantics, and evaluation always
  yields *(resolved values, diagnostics)*. Partial validity is a normal
  output, not an error path.
- **P5. One concept, one syntax** — no two notations for the same thing; the
  grammar leaves no stylistic choice a formatter cannot canonicalize.
- **P6. Provenance and reviewability first** — the origin of every name is
  always answerable, and the surface is optimized for the human reviewer
  before the tool and the tool before the agent.
- **P7. No partially specified features** — a construct enters the language
  only together with complete assignability, evaluation, and serialization
  rules. "Allowed, semantics later" is inadmissible.

---

## A. Surface

### D1. Value literals are a JSON superset

- Objects `{ "key": value }`, arrays, strings, numbers, `true`/`false`/
  `null` are valid exactly as in JSON. Object keys satisfying the
  identifier rule may drop their quotes.
- Element separators are comma **and** newline (comma accepted for JSON
  compatibility, newline as Decl style). Trailing commas are allowed. The
  formatter's canonical form: newline-separated when multi-line, comma-
  separated when single-line (P5 — the grammar admits both only because P3
  demands it; the canonical form removes the choice).

### D2. The type surface and the value surface are separate

- Types appear after `:`, values after `=`: `const x = 1`,
  `type T = ...`, `output o: T = ...`; object literal interiors use
  `key: value` for JSON compatibility.
- Decl deliberately rejects type=value fusion (the CUE model). Keeping the
  surfaces apart is what lets schema-layer errors be reported at the schema
  layer, and lets each surface use notation natural to it without collision
  (see D11/D12 vs D16).

### D3. `const` is the single binding form for computed values

- Module level: `const max_width = 256` — named constant.
- Schema member level: `const x = expr` — derived member (D4).
- Both are the same concept — "name = computed value" — differing only in
  scope; each scope is delimited by its enclosing construct. `const` binds
  values only; types use `type`, functions use `func`.
- The keyword is `const`, not `let`: in the mainstream (TypeScript/
  JavaScript) reading, `let` announces a *mutable* binding — the opposite
  connotation of a language where every binding is immutable. `const` says
  exactly what holds.
- **There is no expression-level binding form in v0.1.** Named
  intermediates come from decomposition: another `const` member in a
  schema, a module `const`, or a helper `func`; comprehensions already
  bind their own iteration names. Every candidate syntax carried a real
  cost — `let x = e in body` collides with the membership operator `in`
  (D16) and reads as a foreign idiom; a braced body would give `{` a
  second meaning on the value surface (P3/P6); arrow terminators collide
  with `=>` (D16). If the evaluator spike shows single-expression `func`
  bodies genuinely suffering, a parenthesized-head form
  (`const (x = e) body`) is the designated re-entry candidate, admitted
  with evidence (OQ7).

### D4. Member kinds are distinguished by syntax, not modifiers

| Form | Meaning |
|---|---|
| `x: T` | Required — must be present in input |
| `x?: T` | Optional — may be absent |
| `x: T = expr` | Defaulted — filled by evaluating `expr` when omitted |
| `const x = expr` | Derived — always computed; input cannot set it |

- No `@required`/`@optional`-style modifiers. The derived kind is not a
  dedicated keyword but `const` (D3): a binding is not an input slot, so
  "input cannot set it" follows from what `const` already means.
- `const` members may carry a type (`const x: T = expr`) or infer it.
- Default and derived expressions may reference sibling properties (the
  dependency graph, D23). Input data cannot **set** a derived member — but
  it may **restate** one: a bound document supplying a derived member is
  accepted iff the supplied value equals the computed one, and is an error
  otherwise. Without the restatement rule, D29's round-trip (derived
  members included in output by default) would reject its own output on
  re-binding.
- `@` annotations (`@deprecated`, `@doc("...")`) are metadata only; no
  annotation affects semantics (P5 — semantics live in one place).

### D5. Absence and `null` are distinct

- An omitted `x?: T` is **absent**; absence is not a value. `null` is an
  explicit value of type `null`; `T?` abbreviates `T | null`. (The `?` in
  `x?: T` marks the declaration; the `?` in `T?` marks the type — different
  concepts, deliberately different positions.)
- Absence is handled with `?.` (safe navigation), `??` (fallback — for
  absent *or* `null`, the nullish reading), and the membership operator
  `in` as the presence test: `"x" in a` for an optional record member,
  `k in m` for a map key (`null` is present). Presence is a question
  about the **container**, never about an absent "value" — there is no
  `exists()` special form, so nothing in the language consumes absence
  except `?.` and `??`. The three forms answer different questions:
  presence, safe navigation, usable-value fallback. Absence is tracked
  statically (a maybe-absent flag on expressions), so consuming an
  absent value is a compile error, never a runtime one.
- Serialization: absent properties are not emitted; `null` is emitted (D29).

---

## B. Types

### D6. `int` is arbitrary precision; width types are refinements

- `int` has no overflow; derived computations are safe at any magnitude.
- `int<N>`/`uint<N>` restrict the representable range. Assigning an
  out-of-range value is an error — there is no implicit truncation. They
  denote the same value sets as the corresponding range types (D8); the
  types chapter fixes both as one refinement mechanism, so the checker
  treats `uint<8>` and `0..255` identically.

### D7. `float` is IEEE 754 binary64 — and the only float

- No `float<32>` or other reduced-width float in v0.1. Admitting one would
  require complete assignment and rounding semantics (P7): under the
  int-style "range refinement, no implicit conversion" reading, ordinary
  literals like `0.1` would be assignment errors (not exactly representable
  in binary32), and under a rounding reading determinism rules must specify
  every rounding site. No current use case pays for that specification
  cost; the previous iteration left exactly this hole *(V4)*. Reduced-width
  floats may return via a revision that carries full semantics.

### D8. Literal, pattern, range, and predicate types

- Literal types `"idle"`, `1`, `true` combine with unions for enumeration.
- Pattern types `/[a-z][a-z0-9_]*/` are whole-match string types;
  `${Type}` interpolation inside patterns is allowed.
- **Range types**: `1..65535`, `0.0..100.0`, `0..<256` are types
  (Pascal/Ada subrange heritage — no keyword, no predicate machinery for
  the common case). The base type is read off the endpoints: int literals
  make an int range, float literals a float range; mixed endpoints are an
  error (no implicit conversion, D6/D7). Endpoints are compile-time
  constant expressions — the same machinery as array sizes (D9).
  Membership requires the base type (`3.0` does not satisfy `1..10`).
  A range denotes a subtype of its base type, so subsumption (D13)
  handles `1..10 ⊑ 0..20 ⊑ int` directly. One-sided ranges (`2..`) are
  not in v0.1 — parameterized predicates cover them.
- **Predicate types**: `T(p)` refines `T` by a predicate, where `p` is an
  expression of type `(T) => bool` — canonically a named function,
  possibly parameterized; a comma list is order-independent conjunction:

  ```decl
  func is_aligned(n: int): bool = (n & 7) == 0
  func divisible_by(d: int): (int) => bool = (n) => n % d == 0

  type Aligned = int(is_aligned)
  type Stride  = int(divisible_by(8))
  type Strict  = int(is_aligned, divisible_by(4))
  ```

  There is no candidate sigil (`$value`) and no clause keyword (`where`):
  the predicate is an ordinary function of the value and nothing else,
  which is exactly what makes a refinement mean the same thing wherever
  it is used *(V5)* — and function identity gives subsumption clean base
  cases (`T(f) ⊑ T(f)`, `T(f, g) ⊑ T(f)`, `T(f) ⊑ T`). Constraints that
  need position belong in `assert` members (D20).
- **Canonical forms** (P5): contiguous ranges are written as range types,
  enumerations as literal unions, string shapes as patterns; `T(p)` is
  for what those cannot say. A hand-written predicate that duplicates a
  range cannot be forbidden by grammar (predicates are code); the
  formatter/linter steers to the canonical form.

### D9. Canonical composite forms: arrays and maps

- Arrays: `T[]`, `T[n]` (fixed), `T[min..max]`, `T[min..<max]`.
- Maps: `{ [string]: V }`, pattern keys `{ [/si\d+/]: V }`, type keys
  `{ [KeyType]: V }`.
- No alias forms (`Array<T, N>`, `Map<K, V>`) — P5.

### D10. Records are closed by default; closedness is a construction-time check

- A record type's members are exhaustive unless the member list ends with
  `...` (open record), which preserves and passes through undeclared
  fields.
- **Closedness is a check applied when a value is constructed against or
  bound to a type** (object literals, `input` binding): undeclared members
  are rejected there. It is **not** a clause of the subtype relation.
  Consequence: a type extending a closed record (D21) is still a subtype
  of it — subsumption compares declared members only — while unknown-field
  detection on input remains fully effective. Defining closedness inside
  subtyping makes extension and closedness contradict each other, which is
  precisely the trap the previous iteration fell into *(V9)*.
- Unknown fields passing through `...` are **opaque**: preserved and
  re-serialized faithfully, but inaccessible to expressions — to compute
  on a field, declare it. A first-class `unknown` type (TS-style,
  access-after-narrowing) is rejected for v0.1: it would require complete
  narrowing semantics, a cost P7 does not allow to defer.
  *(resolves former OQ2)*

### D11. Unions with structural discrimination

- `A | B` is a union type. Tagged unions are discriminated **structurally**
  by literal-typed fields — there is no reserved tag-field convention.
- Discriminability of record arms is **required, not optional**: each
  record arm carries its own defaults, derived members, and constraints,
  so the arm that runs must be uniquely determined by the value — a
  union with two non-discriminable record arms is an error at its
  declaration (P2). Arms without member semantics (primitives, literals,
  ranges, patterns) may overlap freely; the types chapter fixes the
  layered determination procedure.
- `match` performs exhaustiveness checking over the discriminating field.

### D12. Intersection `A & B` is the conjunction of constraint layers

- A value satisfies `A & B` iff it satisfies both `A` and `B`. `&` is
  commutative, associative, and idempotent by construction — composition
  of independently authored constraint layers is **order-independent**,
  which single inheritance cannot provide *(V8)*: a security baseline and
  a region policy apply to the same type as `Service & Secured & Regional`
  with no artificial linear order and each layer independently reusable.
- Member rules (detailed in the types chapter, all derived from
  "satisfies both"):
  - The member set is the union of both sides' members; a member present
    in both is constrained by both types (its effective type must be
    non-empty and comparable under D13 — otherwise a compile diagnostic).
  - A member is required in `A & B` if required in either side.
  - Two derived (`let`) members with the same name are an error.
  - Constraints (`assert`, `when`) are the union of both sides, ids
    qualified by their origin type (D20).
  - The result is closed iff **either** side is closed (intersection of
    allowed member sets).
- **Emptiness detection is structural**: an intersection that is
  uninhabited for structural reasons — primitive mismatch
  (`int & string`), disjoint ranges or literals (`1..10 & 20..30`),
  conflicting member kinds — is a compile diagnostic. Emptiness that
  hinges on predicates (`int(f) & int(g)` with no common satisfier) is
  undecidable and is **not** detected statically; it surfaces when a
  value is constructed or bound. *(resolves former OQ5)*
- The token `&` is available because references are spelled `ref<T>`
  (D26); the previous iteration excluded intersection half on a token
  clash — a semantic decision must not be justified by a syntax problem.

### D13. Subsumption is one normative, total judgment

- The judgment `T′ ⊑ T` ("every value satisfying T′ satisfies T") is
  specified normatively and is **total over the type surface, including
  all four member kinds** — required, optional, defaulted, derived — and
  over unions, intersections, ranges, predicate refinements, patterns,
  quantities, and generics *(V3)*.
- One judgment serves every consumer: object-literal assignability,
  function-argument compatibility, union-variant discrimination,
  narrowing checks in inheritance (D21), member compatibility in `&`
  (D12).
- The judgment is designated for exposure as a queryable operation —
  semantic diff between schema versions and residual-constraint queries
  are the same procedure asked differently *(V11)*. v0.1 specifies the
  judgment normatively and uses it internally (narrowing, `&`
  compatibility, discrimination); the tool-facing query surface —
  semantic diff, residual constraints — lands with the CLI/LSP phase
  (Phase 4). *(resolves former OQ4)*

### D14. Generics with type and value parameters

- `type Pair<T> = { first: T, second: T }`; value parameters
  `type Vec<T, N: int> = T[N]`.
- Parameter constraints are expressed by the parameter's own type — range,
  union, or predicate types (D8): `type Vec<T, N: 1..1024> = T[N]`. There
  is no separate constraint clause; `where` is not a keyword of the
  language.

### D15. Quantities are typed by dimension and have a defined interchange form

- `dimension Time`, `unit s: Time` (base unit), `unit ms = 1e-3 s`
  (derived), `quantity<Time>` as the value type, literals `10ms` (number
  and unit identifier, no space).
- Addition across different dimensions is a type error; multiplication and
  division compose dimensions.
- **Interchange form** *(V1)*: a quantity serializes as the object
  `{ "value": <number>, "unit": "<base-unit symbol>" }` (converted to the
  dimension's base unit for determinism), and exactly that object shape,
  appearing where `quantity<D>` is expected with a unit symbol belonging
  to `D`, binds back as the quantity. Quantities are therefore fully
  round-trippable and usable in `input`-bound JSON — a unit system whose
  values cannot cross the input boundary would collide with the
  language's core purpose.
- The stdlib ships the **full SI catalog**: the seven base dimensions,
  their base units, the standard derived units, and the SI-prefixed
  forms — all as ordinary `dimension`/`unit` declarations, no special
  mechanism (the stdlib chapter fixes the exact inventory). Domain
  units are user-declared on top. *(resolves former OQ1)*

---

## C. Expressions and functions

### D16. The expression vocabulary

- Logical operators: `!`, `&&`, `||`. Bitwise operators: `&`, `|`, `^`,
  `~`, `<<`, `>>`. These are the TypeScript/JavaScript forms — the
  largest developer population reads them natively; the type surface's
  `|` and `&` (D11, D12) live on the other side of D2's separation, so
  there is no collision. Mixing `??` with `&&`/`||` without parentheses
  is a compile error (the JS rule — removes the classic precedence
  footgun).
- Conditionals are `if c then a else b` — kept over the ternary
  `c ? a : b` deliberately: it reads as English for the weakest reader
  (P6), and `?` stays reserved for the optionality family
  (`x?: T`, `T?`, `?.`, `??`).
- Lambdas: `(x) => e` — the TypeScript arrow. `->` does not exist in the
  language; function types are written `(int) => bool` and function
  return types with a colon (D17).
- Kept: `match`, pipeline `|>`, string interpolation
  `` `...${e}...` ``, `??`, `?.`, `in`, ranges `..` (inclusive) / `..<`
  (exclusive), spread `...e`, `s matches /pattern/`.
- Comprehensions: `[f(x) for x in xs if p(x)]`; map comprehension
  `{ k(x): v(x) for x in xs }`.
- **No method-style calls, no trailing blocks** — function application and
  pipelines only: `std.array.count(xs)` or `xs |> std.array.count`;
  collection predicates take lambdas:
  `std.array.all(ports, (p) => p.mode == "input")`.
- The standard library lives under the `std.` namespace — a plain
  namespace, exactly like a user library's (`mylib.foo(x)` and
  `std.array.count(xs)` are the same syntax; the stdlib gets no
  privileged marker, and the no-shadowing rule (D27) already protects the
  name). `std` is **ambient**: available in every module with no import,
  like `Math`/`JSON` in JavaScript — every use site is fully qualified,
  so provenance needs no import statement. It is not a package: its
  semantics and version are fixed by the spec's stdlib chapter, not by
  dependency resolution (D28); importing it (`from "std"`) is an error,
  `std` is reserved as a package name, and dotted access
  (`std.array.count`) is namespace member access, never module
  resolution. The `$` sigil is reserved for **context variables** — values
  that depend on the evaluation position: `$this $parent $root $key
  $path $referrers`. A sigil on `std` (`$std.*`) would dilute that signal:
  `$` answers "does this depend on where I am?" at a glance.
- Record update: `base with { width: 128 }` — shallow, produces a new
  value. Deep merge is a stdlib function with specified bias and conflict
  rules.

### D17. Functions are total: single expression, no recursion

- `func clog2(n: int): int = std.math.clog2(n)` — the body is one
  expression; there are no statements. The return type follows a colon,
  like every other type position (D2) — there is no `->` in signatures.
- Direct and indirect recursion are compile errors: the call graph must be
  acyclic. This is the basis of P2's termination guarantee. Iteration is
  expressed with comprehensions and `std.array.fold`.
- Lambdas are values and may be passed as arguments (higher-order
  functions are allowed).

### D18. Transitive queries via finite-fixpoint combinators — admission gated on evidence

- Reference graphs (D26) raise reachability/acyclicity/connectivity
  constraints, which one-hop queries and fold cannot express *(V7)*.
- The designated mechanism is a set of standard-library combinators
  (reserved namespace `std.graph.*`, e.g. `closure`, `reachable`,
  `is_acyclic`) whose semantics are **least fixpoints of monotone
  operators over the finite evaluation world** — finiteness plus
  monotonicity guarantees termination (the Datalog argument), so P2 is
  fully preserved: recursion lives inside the combinator, and the user
  call graph stays acyclic, exactly as `fold` provides iteration without
  loops.
- **Admission into v0.1 is decided by the evaluator spike**
  (ROADMAP §0.6): if the hardware-interconnect benchmark needs transitive
  constraints, the combinators enter the stdlib spec; if it demonstrably
  does not, they stay out and this decision records that evidence (OQ3).
  Either way the grammar is untouched — they are ordinary functions.

---

## D. Schemas and constraints

### D19. Schema members are flat, in two natures

- Properties (D4), derived `const` members, `assert` members (D20), and
  `when` groups appear side by side — no `properties {}` /
  `constraints {}` / `diagnostics {}` blocks.
- Members divide by **which component of P4's result they feed**:
  - **Value members** — `x: T`, `x?: T`, `x: T = e`, `const x = e` —
    become key/value pairs of the evaluated value; they serialize (D29)
    and (except derived) may be set by input.
  - **Constraint members** — `assert` and `when` groups — feed the
    diagnostics list only; they are not data, never appear in output,
    and cannot be set by input. An `assert` is not a property: it is a
    named cross-member predicate the type imposes on its values — the
    cross-field sibling of the single-value predicate types `T(p)` (D8),
    carrying a stable diagnostic id (D20).
  - The two natures also merge differently: value members compose by
    narrowing, constraint members by union (D12, D21).
- **One name space**: within a schema, value-member and constraint-member
  names live in a single name space and cannot collide — a property
  `symmetric` and an `assert symmetric` in the same type is an error
  (paths and diagnostic ids would blur otherwise).
- **Division of labor between type-attached constraints and asserts**:
  1. Hard admissibility of a single value → the member's *type* (range,
     union, pattern, predicate — D8): reusable, checkable in positions
     that have no assert host (array elements, map values, function
     parameters), visible to subsumption (D13), rejected at type-check
     time with a path — and with a custom diagnostic when the named type
     declaration carries an `else` clause (D20).
  2. Relationships between members or entities → `assert` (D20) — a
     member of the type that owns those properties, not an external
     device: named stable id, custom message and parameters, `when`
     grouping.
  3. Soft guidance (warn/info) → always `assert`: a type is hard by
     nature — a value either has it or not — so "valid but discouraged"
     can only be said by a value-preserving diagnostic.
  A single-member hard constraint written as an assert is legal but
  non-canonical (P5); the formatter steers it into the member's type.
- `when <condition> { ... }` groups contain **constraints only**.
  Conditionally different *shape* is expressed with tagged unions (P1 —
  no special conditional-existence feature).

### D20. Constraints are `assert`; diagnostics are first-class declarations

```decl
assert width_match: source.width == target.width
    else width_mismatch(source.width, target.width)

diagnostic width_mismatch(src: int, dst: int) {
    severity = error
    message = `source width ${src} != target width ${dst}`
}
```

- `assert <name>: <bool-expr> [else <diagnostic-ref | inline severity
  message>]`. The name is unique within its schema and forms the stable
  diagnostic id `<module-path>.<type>.<assert-name>`; the id survives
  edits to the condition text.
- Omitting `else` produces a default error diagnostic. Inline forms:
  `else error \`...\``, `else warn \`...\``, `else info \`...\``.
- `diagnostic` declarations are module-level: id, severity, parameters,
  message template — the unit of cataloguing, localization, and
  documentation. Ids and codes are immutable and append-only; messages
  are mutable.
- **Type-level custom diagnostics**: a *named* type declaration may carry
  the same `else` clause as an assert:

  ```decl
  type Port = 1..65535
      else error `port must be between 1 and 65535`

  diagnostic bad_name(v: string) {
      severity = error
      message = `service name ${v} must be lowercase kebab-case`
  }
  type ServiceName = /[a-z][a-z0-9-]*/ else bad_name
  ```

  When a value fails the type, this diagnostic **replaces** the generic
  type-mismatch one. A referenced diagnostic receives the offending value
  bound to its first parameter — no sigil needed; an inline message is
  static text (path and actual value accompany every diagnostic
  automatically, D9-style). The severity must be `error`: a type is hard
  admissibility (D19) — softening belongs to asserts. Anonymous inline
  types take no `else`; name the type, which also gives the diagnostic
  its stable id (`<module>.<TypeName>`).
- **Error severity invalidates** the value and everything derived from
  it; **reporting is root-cause-only** — invalidation propagates, but
  dependent members produce no cascading diagnostics, so one defect is
  one report. Warnings and infos preserve values.
- Every diagnostic automatically carries its occurrence path; the
  diagnostic list is sorted by `(path, id)` for byte-stable output.

### D21. Inheritance is extension plus narrowing

- `type Child = Parent { ... }` adds members and may narrow inherited
  member types (checked by subsumption, D13); any widening is an error.
  Single inheritance only — combining independent constraint layers is
  `&`'s job (D12), not inheritance's.

---

## E. Evaluation

### D22. Evaluation roots are `output` and `input`; module `const` is pure

- `output x: T = { ... }` — a named value this module produces: the type
  is mandatory and the value runs the full pipeline (type check →
  defaults → derived → constraints). Tools treat exported outputs as the
  unit of evaluation and serialization; a non-exported output is a
  module-internal validated value (e.g. a reference target). The keyword
  pairs with `input` — a module's I/O contract is its inputs and outputs
  (the previous iteration's "instance" said less: input/output symmetry
  explains itself, "instance of a type" does not).
- `input x: T [= default]` — a value injected by the tool at evaluation
  time (a JSON document is already a Decl value by P3). Binding runs the
  identical pipeline; validation of external data and generation from
  external input are this one path. An unbound `input` whose value is
  needed is an error. There is no other I/O in the language (P2).
- A module-level `const` is a pure constant: it is **not** an evaluation
  root, is not schema-validated, and is **not a legal reference target**
  (D26) — a reference type must never point at a value that has not
  passed its type's pipeline *(V2)*.

### D23. The evaluation pipeline and its determinism

- Stages: parse → import resolution → name resolution → type check →
  dependency analysis → evaluation (lazy) → constraint validation →
  emission. Diagnostics from every stage persist to the end (P4).
- The reference graph formed by default, derived, and constraint
  expressions must be **acyclic**; every member involved in a cycle is an
  error.
- Evaluation is lazy with results observationally identical to eager
  evaluation.
- **Ordering is specified everywhere** *(V6)*: module members in
  declaration order; input-bound data in document order; map/object
  members preserve insertion order; duplicate keys are errors; query
  results (e.g. `$referrers`) in canonical path order (D26).
- Numeric semantics (arbitrary-precision `int`, binary64 rounding rules,
  D24 safety) are fixed so implementations agree bit-for-bit. FMA
  contraction and extended-precision intermediates are forbidden.

### D24. No NaN, no Infinity, no silent division

- A float operation that would produce NaN or ±Infinity produces an
  **evaluation-error diagnostic** instead; the value domain contains
  neither. Division by zero (integer and float) is likewise a diagnostic,
  not a value.
- Grounds: determinism (P2) and serialization round-trip (D29) — JSON has
  no NaN/Infinity.

### D25. Partial evaluation has an explicit, honest contract

- A tool may request any path; the language evaluates the minimal
  dependency set for that path and returns its value plus the
  diagnostics **arising from that set only**. The result is explicitly
  marked partial.
- A whole-document validity verdict is obtainable **only** from full
  validation; the semantics chapter enumerates the diagnostic classes
  partial evaluation cannot produce (unevaluated members' assertion
  failures, unreached input mismatches). An agent or tool asking "is this
  valid?" through partial evaluation is a specified misuse *(V10)*.

---

## F. Relationships

### D26. Composition by default; references are `ref<T>`

- Property values are **composition** (ownership): a value tree, single
  parent, no value cycles.
- `ref<T>` is a **non-owning reference**: graphs and reference cycles are
  allowed; a dangling reference is a validation error.
- The form is a built-in parameterized type, not a keyword or symbol — the
  fourth member of the lowercase built-in family `int<N>`, `uint<N>`,
  `quantity<D>`, using the ordinary generic machinery (D14). It reads
  natively for TypeScript users (`Partial<T>`, Vue's `Ref<T>`), frees `&`
  for intersection (D12), and makes composition explicit where a prefix
  marker is ambiguous: `ref<Service>[]` is an array of references,
  `ref<Service[]>` a reference to an array.
- Reference reading is type-directed in both directions: a navigation
  expression in a `ref<T>` position denotes the reference (the place,
  not a copy), and a reference in a plain `T` position denotes the
  target's **value** (dereference where a declaration asks for the
  value — never silently). Member access and spread navigate through
  references transparently.
- **Legal targets** are values reachable under evaluation roots
  (`output`/`input` values and their sub-values) — therefore every
  referenced value has passed its type's pipeline (D22, *(V2)*), and
  every legal target has a canonical serialization path rooted at its
  evaluation root's name.
- Navigation context: `$this $parent $root $key $path`. Reverse query:
  the one universe query is the reverse query `$referrers(T, "m")` —
  read as a relationship edge: `T` names **who** refers (and types the
  result), `"m"` names **through what** (the member carrying the
  reference, where "carrying" traverses arrays/maps under `m` but not
  nested records). Premise: a reference is always owned by a record
  instance, so the two arguments fully determine the edge. The result
  is distinct `ref<T>[]` in **canonical path order**, defined
  identically for declared and input-bound data *(V6)*. Place equality
  supports the surrounding idioms: `==`/`in` with reference operands
  compare canonical paths. There is no ambient "all values of T"
  enumeration; other relationship constraints belong to the container
  type that owns the collections, as comprehensions over named
  collections.

---

## G. Modules and packages

### D27. Imports and exports are named; provenance is absolute

- File = module. Only `export`-marked declarations are visible outside.
- `import { A, B } from "./x.decl"` and `import * as ns from "./x.decl"`
  (namespace imports keep origins answerable). **No bare wildcard
  injection** (`import * from`) — P6.
- Re-export is named-only: `export { A as B } from "./x.decl"`; no
  `export * from`.
- No shadowing anywhere: a name bound in scope cannot be rebound by an
  inner scope.

### D28. Packages: exact pin and lock

- Manifest `decl.toml`: three semantic fields — name, version,
  dependencies. Dependency versions are **exact pins only** — no ranges,
  no carets.
- Descriptive metadata fields (description, license, authors, …; the
  modules chapter fixes the list) are permitted and **never affect
  resolution or evaluation**. Fields outside the semantic and metadata
  sets are errors (fail-closed). *(resolves former OQ6)*
- The lock file records content hashes and is **fail-closed**: a hash
  mismatch stops resolution. Under the same lock state, module resolution
  is deterministic.

---

## H. Interchange

### D29. Serialization policy and total round-trip

- Absent properties are not emitted; `null` is emitted (D5).
- Derived (`const`) members are **included** by default (tool option to
  exclude).
- Floats print in shortest round-trip form (the ECMAScript
  `Number::toString` algorithm).
- Quantities serialize per D15; references serialize as canonical path
  strings (D26) — **document-relative** (`"$.a[0]"`) for targets under
  the same evaluation root, absolute for cross-root targets, so an
  emitted document never embeds its own root name and can re-bind to a
  slot of any name (without this, intra-root references would break
  the round-trip below: an input can never share its output's name).
  Member order follows D23.
- **Round-trip idempotence is normative and total** *(V1)*: for every
  value the language can produce, serializing and re-binding the output
  as `input` succeeds, validates, and re-serializes byte-identically.
  Every serializable form has a defined input form — no exceptions, no
  value classes left out.

---

## Syntax deliberately absent

One concept, one syntax (P5). The left column does not exist in Decl; use
the right column.

| Absent | Use instead |
|---|---|
| `properties {}` / `constraints {}` / `diagnostics {}` blocks | flat members + `assert` + `diagnostic` (D19, D20) |
| `let` keyword | `const` (D3) |
| expression-level bindings (`let … in`, parenthesized heads, binding blocks) | decompose into `const` members, module `const`s, helper `func`s (D3) |
| `->` (lambda arrow, return-type arrow) | `(x) => e` lambdas, `func f(...): T` returns (D16, D17) |
| ternary `c ? a : b` | `if c then a else b` (D16) |
| `@required` / `@optional` / `@readonly` modifiers | `x: T` / `x?: T` / `const x = e` (D4) |
| `int[8..256]` bracket range refinement | range type `8..256` (D8) |
| `T where <expr>` refinement clause, `$value` sigil | predicate type `T(p)` with a named predicate (D8) |
| `int(0..255)` range-in-parens | range type `0..255` (D8) |
| `Array<T, N>`, `Map<K, V>` | `T[N]`, `{ [K]: V }` (D9) |
| `&T`, `*T`, prefix-keyword `ref T` reference syntax | `ref<T>` (D26) |
| `float<32>` | `float` (D7) |
| reserved tag fields (`__tag`) | literal-field structural discrimination (D11) |
| word operators `and` `or` `not`, `band` `bor` `bxor` `bnot` `shl` `shr` | `&&` `\|\|` `!`, `&` `\|` `^` `~` `<<` `>>` (D16) |
| method calls (`xs.count()`), trailing blocks | `std.*` application + `\|>` + lambdas (D16) |
| `$std.*` sigil namespace | plain `std.*` — `$` is context-variables-only (D16) |
| `import * from` (bare injection) | named imports, `* as ns` (D27) |
| `export * from` | named re-export (D27) |
| `boolean`, `integer` | `bool`, `int` |
| `NaN`, `Infinity` literals | none — diagnostics instead (D24) |

---

## Comprehensive example

A service-topology domain — no domain keywords — exercising quantities,
intersection layers, references, and both evaluation roots. Every spec
chapter must agree with this example's syntax.

```decl
dimension Time
unit s: Time
unit ms = 1e-3 s

type Protocol = "http" | "grpc" | "tcp"
type ServiceName = /[a-z][a-z0-9-]*/
type Port = 1..65535
    else error `port must be between 1 and 65535`

type Service = {
    name: ServiceName
    protocol: Protocol
    port: Port = 8080
    replicas: int = 1
    timeout: quantity<Time> = 500ms
    description?: string

    const endpoint = `${name}:${port}`

    assert scaled: replicas in 1..16
        else warn `replicas ${replicas} is outside the recommended range`
}

// Two independent constraint layers — order-independent conjunction (D12)
type Secured = {
    protocol: "grpc"
    ...
}
type Regional = {
    replicas: 2..16
    ...
}
type ProdService = Service & Secured & Regional

diagnostic protocol_mismatch(src: string, dst: string) {
    severity = error
    message = `link endpoints use different protocols: ${src} vs ${dst}`
}

type Link = {
    source: ref<Service>
    target: ref<Service>
    weight: int = 1

    assert no_self_link: source.name != target.name
    assert protocols: source.protocol == target.protocol
        else protocol_mismatch(source.protocol, target.protocol)
}

type Topology = {
    services: Service[1..64]
    links: Link[]

    const service_count = std.array.count(services)

    assert unique_names:
        std.array.all_distinct([s.name for s in services])

    when service_count > 32 {
        assert dense_topology:
            std.array.count(links) >= service_count
    }
}

// Generation: a comprehension-built value this module produces (full pipeline)
output demo: Topology = {
    services: [
        { name: `svc-${i}`, protocol: "grpc", port: 9000 + i }
        for i in 0..<3
    ]
    links: []
}

// Validation: an external JSON document bound to the same rules
input external_topology: Topology
```

---

## Vision checklist traceability

Every item of [00. Vision §6](00_vision.md) resolves to a decision here
(quality bar, [01 §5](01_requirements.md)):

| Item | Resolution |
|---|---|
| V1 quantity round-trip and input | D15 (interchange form), D29 (total round-trip) |
| V2 references only to validated values | D22 (module `let` excluded), D26 (targets under evaluation roots) |
| V3 assignability total over member kinds | D13 (one total subsumption judgment) |
| V4 `float<32>` semantics | D7 (removed; P7 bars re-entry without full semantics) |
| V5 context variables in refinements | D8 (predicates are plain functions of the value — no context variables, no sigil at all) |
| V6 ordering guarantees | D23 (ordering everywhere), D26 (`$referrers` canonical path order) |
| V7 transitive/fixpoint queries | D18 (reserved combinators, spike-gated — OQ3) |
| V8 order-independent conjunction | D12 (intersection `&`) |
| V9 closedness vs subtyping | D10 (construction-time check, not a subtype clause) |
| V10 partial evaluation contract | D25 |
| V11 subsumption as first-class query | D13 (normative judgment; exposure scope OQ4) |
| V12 preserve list | P4/P6 (result shape, provenance), D2, D20, D23–D24, D27–D28 |

---

## Open questions

Tracked here until resolved; resolutions are promoted into the numbered
decisions. OQ1 (SI catalog → D15), OQ2 (opaque unknown fields → D10),
OQ4 (subsumption exposure scope → D13), OQ5 (structural emptiness →
D12), and OQ6 (manifest fields → D28) were resolved on 2026-08-30 and
folded into their host decisions. The two remaining questions are
**deliberately open** — their resolution criterion is the evaluator
spike's evidence (ROADMAP §0.6), and deciding them earlier would defeat
the evidence gate:

- **OQ3.** Admission of `std.graph.*` fixpoint combinators into v0.1 —
  decided by the evaluator spike (D18).
- **OQ7.** Re-admission of an expression-level binding form (designated
  candidate: `const (x = e) body`) — decided by the evaluator spike's
  evidence on single-expression `func` bodies (D3).

---

## Previous / Next

- Previous: [01. Design Requirements](01_requirements.md)
- Index: [Documentation home](../README.md)
