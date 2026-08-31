# 03. Type System

A type denotes a set of values. This chapter defines every type form, the
subsumption judgment `⊑` that relates them, and the assignability rules
built on it. Types appear after `:` and are a separate surface from
values (D2); nothing in this chapter is an expression.

## 3.1 Type declarations and structural typing

```decl
type Port = 1..65535
type Service = { name: string, port: Port }
```

- `type Name = TypeExpr` names a type. Names are **transparent aliases**:
  Decl typing is structural — two types with the same structure denote
  the same value set regardless of their names. A name adds only
  documentation, a stable home for a type-level `else` diagnostic
  ([06. Constraints](06_constraints.md)), and a readable identity in
  messages.
- Type declarations may be **mutually recursive**:

  ```decl
  type Menu = { title: string, items: MenuItem[] }
  type MenuItem = { label: string, submenu?: Menu }
  ```

  Every cycle in the *composition* graph of a set of types must pass
  through a member that can terminate it — an optional member, an array
  or map (which can be empty), or a union arm that ends the recursion.
  A cycle with no such member denotes no finite value; it is an
  **uninhabited-type error** (§3.19).

  *Counterexample:* `type T = { child: T }` — the only member is
  required, so no finite value exists; error.
- Type parameters make a declaration generic (§3.15).

## 3.2 Primitive types

| Type | Value set |
|---|---|
| `null` | the single value `null` |
| `bool` | `true`, `false` |
| `int` | all integers, **arbitrary precision** (D6) |
| `float` | IEEE 754 binary64, excluding NaN and ±Infinity (D24) |
| `string` | Unicode strings |

- `int` and `float` are disjoint: no value is both, and there is no
  implicit conversion in either direction (D6, D7). `1` is an `int`;
  `1.0` is a `float`; `1 == 1.0` is a type error
  ([04. Expressions](04_expressions.md)).
- There is no other float width (D7) and no `char` type — one-character
  strings serve.

## 3.3 Literal types

A literal is a type whose value set is that one value: `"idle"`, `42`,
`-1`, `2.5`, `true`, `null`. Enumerations are unions of literals:

```decl
type Protocol = "http" | "grpc" | "tcp"
type PowerOfTwoWidth = 8 | 16 | 32 | 64
```

Negative number literals are literal types (`-1 | 0 | 1`). A literal
type's base primitive is the type of its value.

## 3.4 Range types

```decl
type Port = 1..65535        // int range, inclusive
type Ratio = 0.0..1.0       // float range, inclusive
type Index = 0..<256        // upper bound exclusive
```

- `lo..hi` contains every value of the base type with `lo ≤ v ≤ hi`;
  `lo..<hi` excludes the upper endpoint.
- The **base type is read off the endpoints**: two int-typed endpoints
  make an int range; two float-typed endpoints a float range. Mixed
  endpoints are an error. *Counterexample:* `0..100.0` — error.
- Endpoints are compile-time constant expressions of the base type —
  the class of §4.13: literals, `const` references, value parameters,
  arithmetic and `func` calls over those; the same class as array
  sizes (§3.9). `lo > hi` (or
  `lo ≥ hi` for `..<`) is an **empty-range error** at the declaration.
- Membership requires the base type: `3.0` does not satisfy `1..10`.
- One-sided ranges (`2..`) do not exist in v0.1; use a predicate
  (§3.7) such as `int(std.int.at_least(2))`.

## 3.5 Width-restricted integers

`int<N>` and `uint<N>` (N a constant `int` expression, `N ≥ 1`) are
**notation for ranges** (D6):

- `int<N>` ≡ `-(2^(N-1)) .. 2^(N-1) - 1`
- `uint<N>` ≡ `0 .. 2^N - 1`

The checker treats `uint<8>` and `0..255` identically; the width form
exists to state representability intent (hardware interop). Assigning an
out-of-range value is an error — never a truncation.

## 3.6 Pattern types

A pattern literal is a whole-match string type:

```decl
type ServiceName = /[a-z][a-z0-9-]*/
```

- A string satisfies the pattern iff the **entire** string matches.
  Matching is case-sensitive; there are no flags (lexical §2.8).
- The pattern grammar is the portable regular-expression core:
  character literals and escapes, classes `[…]`/`[^…]`, `.`,
  alternation `|`, grouping `(…)`, repetition `* + ? {m} {m,} {m,n}`,
  and the class escapes `\d \w \s` (with uppercase negations). There is
  no backreference, lookaround, or capture semantics — patterns denote
  regular languages, keeping membership decidable and cheap.
- `${T}` **interpolation** splices another type into a pattern. `T` must
  be string-shaped (a pattern, string literal, or union of those) or
  integer-shaped (an int literal, int range, or union of those); an
  integer-shaped `T` denotes the decimal representations of its members.

  ```decl
  type Lane = /si${0..7}/        // si0 … si7
  type Wide = /m${"in" | "out"}/ // min, mout
  ```

## 3.7 Predicate types

`T(p)` refines `T` by a predicate (D8). `p` is a type-surface expression
denoting a function of type `(T) => bool` — canonically a named `func`,
possibly parameterized; a comma list conjoins:

```decl
func is_aligned(n: int): bool = (n & 7) == 0
func divisible_by(d: int): (int) => bool = (n) => n % d == 0

type Aligned = int(is_aligned)
type Stride  = int(divisible_by(8))
type Strict  = int(is_aligned, divisible_by(4))
```

- A value satisfies `T(p₁, …, pₙ)` iff it satisfies `T` and every
  `pᵢ(v)` evaluates to `true`. Predicate evaluation follows the purity
  and totality rules of [09. Semantics](09_semantics.md); a predicate
  whose evaluation errors (e.g. division by zero) makes the value fail
  the type with that evaluation diagnostic.
- Predicates receive the value and nothing else — no context variables
  (D8): a predicate type means the same thing in every position.
- **Predicate identity** (used by `⊑`, §3.17): two predicate references
  are identical iff they resolve to the same `func` declaration and
  their arguments are equal constants. `divisible_by(8)` equals
  `divisible_by(8)`, not `divisible_by(4)`.
- The parenthesized position takes predicates only. A range in parens is
  the rejected form `int(0..255)` — write `0..255`.

## 3.8 Optionality and `null`

- `T?` abbreviates `T | null` — nothing more. It is a type.
- **Absence is not a value and not a type.** `x?: T` marks the
  *declaration* optional (D4, D5); there is no type whose set contains
  "absent". Consequently `T?` in a required member still requires a
  value (possibly `null`), and `x?: T` without `?` on the type does not
  admit `null`.

```decl
type A = { x?: int }        // x may be absent; if present, an int (never null)
type B = { x: int? }        // x must be present; may be null
type C = { x?: int? }       // may be absent; if present, int or null
```

## 3.9 Array types

```decl
T[]          // any length
T[4]         // exactly 4
T[1..8]      // 1 to 8 elements, inclusive
T[0..<16]    // 0 to 15 elements
```

- An array value satisfies `T[σ]` iff every element satisfies `T` and
  its length is in the size set `σ`. Sizes are constant `int`
  expressions ≥ 0; an empty size set (`T[3..1]`) is an error.
- There is no `Array<T, N>` form (D9). Suffixes compose left-to-right:
  `ref<Service>[]` is an array of references; `int[4][2]` is two arrays
  of four ints.

## 3.10 Map types

```decl
{ [string]: Port }          // any string key
{ [/si\d+/]: Port }         // keys matching a pattern
{ [ServiceName]: Service }  // keys satisfying a named type
```

- One form: `{ [K]: V }` where `K` is a **string-shaped** type (string,
  pattern, string literal, or unions/refinements of those). A map value
  satisfies it iff every entry's key satisfies `K` and value satisfies
  `V`. There is no `Map<K, V>` form (D9).
- A map with a non-string-shaped `K` is an error (JSON object keys are
  strings — P3).
- Maps and records are different types with the same value syntax:
  records declare fixed member names; maps constrain a key *domain*.
  A type expression is one or the other, never both.

## 3.11 Record types (schemas)

```decl
type Router = {
    name: string
    buffer_size: int = 128
    description?: string
    const label = `router:${name}`

    assert named: name != ""
    ...
}
```

- Member names are strings: written bare when identifier-shaped,
  quoted otherwise (`"my-key": int` — closed records must be able to
  declare any JSON key, P3). A reserved keyword as a name is also
  quoted (`"type": string` — the bare form cannot lex). Quoting a name
  that could be written bare is an error (one form per name); access
  forms are §4.3.
- Members divide into **value members** — required `x: T`, optional
  `x?: T`, defaulted `x: T = e`, derived `const x = e` — and
  **constraint members** — `assert`, `when` (D19). Constraint members
  contribute diagnostics only and are covered in
  [06. Constraints](06_constraints.md); their names share the record's
  single name space (a duplicate name across any two members is an
  error).
- A record **value** satisfies a record type iff every required member
  is present and satisfies its type, every present optional member
  satisfies its type, and — for evaluated values — defaulted and
  derived members are present with their computed values
  ([05. Declarations](05_declarations.md) defines the evaluation;
  §3.18 defines checking of unevaluated literals).
- **Closedness** (D10): a record type is closed unless its member list
  ends with `...`. Closedness is a **construction- and binding-time
  check**: when a value is constructed against, or bound to, a closed
  record type, undeclared members are rejected. Closedness is *not* a
  clause of `⊑` (§3.17) — extension subtypes (§3.14) remain subtypes of
  closed parents.
- **Open records and unknown fields**: `...` passes undeclared fields
  through. Passed-through fields are **opaque** (D10): they are
  preserved, compared for equality, and re-serialized faithfully, but
  no expression can read them. To compute on a field, declare it.

  *Counterexample:* with `type P = { debug?: bool, ... }` and
  `input p: P`, the expression `p.verbose` is a name error even if the
  bound document contains `"verbose"`.

## 3.12 Union types and discrimination

`A | B` contains every value of `A` and every value of `B`. Unions are
associative, commutative, idempotent; `|` binds looser than `&`.

**Structural discrimination** (D11): when a union of record types is
inspected (by `match`, or when checking a value against the union), the
variant is identified by **literal-typed fields** that distinguish the
arms — no reserved tag field exists.

```decl
type Circle = { kind: "circle", radius: float }
type Rect   = { kind: "rect", w: float, h: float }
type Shape  = Circle | Rect
```

- **Arm determination** is layered and must be unique wherever arms
  carry semantics:
  1. Arms of different **value kinds** (null, bool, number, string,
     array, object) are discriminated by the value itself; `int` and
     `float` arms are discriminated by the value's numeric kind (in
     data, by lexical form — §2.6, §10.2).
  2. Arms with no member semantics — primitives, literals, ranges,
     patterns, arrays — may overlap freely: a value satisfies the
     union iff it satisfies some arm, and which one is unobservable
     (nothing runs per-arm).
  3. **Record arms must be pairwise discriminable**: some set of
     member names carries, in each record arm, literal types whose
     value combinations are pairwise disjoint (`kind: "circle"` vs
     `kind: "rect"`). This is not only `match`'s requirement but
     validation's: each record arm has its own defaults, derived
     members, constraints, and closedness, so the arm that runs
     **must be uniquely determined** or the same input could evaluate
     two ways (P2). A union type with two non-discriminable record
     arms is an error at its declaration.
  4. Among object-kind arms, at most **one** may be a non-record form
     (a map, a `quantity<D>`, or an open catch-all): it matches
     exactly when no record arm's discriminant does. Two non-record
     object arms are not discriminable — an error.
- An object whose discriminant members match a record arm is checked
  wholly against that arm, and its diagnostics name that arm; an
  object matching no discriminant (and no fallback arm) fails at the
  union with the discriminant members and expected values named
  ([06. Constraints](06_constraints.md)).
- `match` requires the inspected union's arms to be discriminable
  under the same rules and checks exhaustiveness over them
  ([04. Expressions](04_expressions.md)).

## 3.13 Intersection types

`A & B` contains the values satisfying **both** (D12). `&` is
associative, commutative, idempotent — conjunction of independently
authored constraint layers is order-independent.

```decl
type Secured  = { protocol: "grpc", ... }
type Regional = { replicas: 2..16, ... }
type ProdService = Service & Secured & Regional
```

Derived member rules ("satisfies both" spelled out for records):

- The member set is the union of both sides'. A value member present in
  both sides is constrained by both types; its effective type is the
  conjunction (checked for structural emptiness, §3.19). It is required
  in `A & B` if required in either side; optional only if optional in
  both; a defaulted member meeting a required one is required with both
  constraints; two defaulted members with different default expressions
  are an error (which default would apply is ambiguous); two derived
  members with the same name are an error.
- Constraint members are unioned; their ids keep their origin type
  ([06. Constraints](06_constraints.md)).
- The result is **closed iff either side is closed** — the intersection
  of the allowed member sets.
- For non-record operands, `&` is still value-set intersection:
  `1..20 & 16..32` ≡ `16..20`; `int & string` is empty (§3.19).

## 3.14 Inheritance

```decl
type Child = Parent { label: string, port: 1..1024 }
```

- `Parent { … }` **extends** a record type — extending any non-record
  type is an error. It may add members and may
  **narrow** an inherited member — replace its type `T` with `T′ ⊑ T`,
  or strengthen optional to required. Any widening — loosening a type,
  making required optional, changing a member kind incompatibly — is an
  error (D21).
- `Child ⊑ Parent` holds by construction (§3.17; closedness does not
  interfere — D10).
- Single inheritance only. Combining independent layers is `&`'s job
  (§3.13). Inheritance declares an is-a intent and a narrowing
  relationship; intersection conjoins peers.

## 3.15 Generics

```decl
type Pair<T> = { first: T, second: T }
type Vec<T, N: int> = T[N]
type Bounded<T, N: 1..1024> = { items: T[0..N] }
```

- Type parameters (`T`) range over types; **value parameters** (`N`)
  over constant values of their declared type. A value parameter's type
  may be any type usable for constants — ranges, unions, predicates —
  and *is* the parameter's constraint (D14); there is no separate
  constraint clause.
- Instantiation (`Pair<Port>`, `Vec<int, 4>`) substitutes arguments and
  checks value arguments against their parameter types at compile time.
  After substitution, typing is structural: `Pair<Port>` is exactly
  `{ first: Port, second: Port }`.
- Generic declarations are checked at instantiation (v0.1 does not
  require checking a generic body once-for-all-instantiations); every
  instantiation in a program is fully checked.

## 3.16 Dimensions, units, and quantities

```decl
dimension Time
dimension Length
dimension Speed = Length / Time

unit s: Time                 // base unit of Time
unit ms = 1e-3 s             // derived unit
unit m: Length
unit mps: Speed

type Delay = quantity<Time>
const t: Delay = 10ms
```

- **Dimensions** form an abelian group: a dimension expression is a
  base dimension name, a product `D1 * D2`, a quotient `D1 / D2`, or an
  integer power `D ^ n`. Two dimension expressions are equal iff their
  base-dimension exponent vectors are equal (`Length / Time` =
  `Length * Time ^ -1`). `dimension Name` declares a base dimension;
  `dimension Name = expr` names a derived one.
- **Units**: `unit u: D` declares the base unit of `D` (one per
  dimension per scope — a second base unit for the same dimension is
  an error); `unit u = factor u0` declares a unit as a constant
  multiple of another; its dimension is `u0`'s. Conversion factors are
  constant expressions.
- **Units and dimensions have their own name spaces**, separate from
  the value/type name space of §5.1. A unit symbol is meaningful only
  in unit positions — after a number (`10ms`), as the trailing unit of
  a `unit` declaration, in the `"unit"` string of the interchange form
  — and a dimension name only in dimension expressions and
  `quantity<D>` arguments; every such position is syntactically
  unambiguous, so `const ms = 5` and `unit ms` coexist without
  conflict. The no-shadowing rule applies *within* each space
  (redeclaring the unit `ms` is an error; declaring a value `ms` is
  not), and `export`/`import` carry units and dimensions into the
  importer's corresponding spaces (§8.2).
- `quantity<D>` is the type of quantities of dimension `D`. A unit
  literal `10ms` has type `quantity<Time>` — the dimension of its unit.
  The stdlib ships the full SI catalog as ordinary declarations (D15).
- A quantity's **magnitude is IEEE 754 binary64** (unit conversions
  force fractions, so an integer magnitude kind would not survive
  arithmetic). Literal magnitudes convert exactly when representable
  (`10ms` is exactly 10.0); conversion to the base unit is an exact
  rational scaling with one final rounding (§9.5).
- Arithmetic ([04. Expressions](04_expressions.md)): `+`/`-` and
  comparison require **equal dimensions** (error otherwise — never a
  conversion failure, since equal dimensions always convert); `*`/`/`
  compose dimensions (`quantity<Length> / quantity<Time>` :
  `quantity<Length / Time>`); a bare `int`/`float` scales a quantity.
  A quantity whose dimension vector cancels to zero is a plain number.
- **Interchange form** (D15): where `quantity<D>` is expected, the
  object `{ "value": v, "unit": "u" }` — `v` a number, `u` the symbol of
  a unit whose dimension equals `D` — satisfies the type and denotes
  `v` in unit `u`. This is both the serialization output
  ([10. Interchange](10_interchange.md), base-unit normalized) and the
  input form, closing the round-trip.

  *Counterexample:* `{ "value": 10, "unit": "m" }` against
  `quantity<Time>` — dimension mismatch, error.

## 3.17 Subsumption (`⊑`)

`T′ ⊑ T` — "every value satisfying `T′` satisfies `T`" — is the one
normative judgment behind assignability, narrowing, discrimination, and
`&`-compatibility (D13). It must be total: defined for every pair of
type forms. It is reflexive and transitive. The defining clauses:

**Primitives and literals.** A primitive ⊑ itself only. A literal `ℓ` ⊑
`T` iff `ℓ`'s value satisfies `T` (decided by evaluation of the
membership conditions — always decidable since `ℓ` is a constant).

**Ranges.** `r₁ ⊑ r₂` iff same base type and `set(r₁) ⊆ set(r₂)`
(endpoint arithmetic). A range ⊑ its base primitive. `int<N>`/`uint<N>`
participate as their ranges (§3.5).

**Patterns.** A pattern ⊑ `string`. A string literal ⊑ a pattern iff it
matches. Between two patterns, `p₁ ⊑ p₂` holds iff their **normalized
literal text is identical**. Semantic regular-language containment,
though decidable, is *not* part of the judgment — implementations would
have to agree on an expensive algorithm to stay deterministic, and the
formatter-normalized text comparison is stable.
*Counterexample:* `/ab*/ ⊑ /ab*|a/` does **not** hold, though the
languages are contained.

**Predicates.** `T(F′) ⊑ T(F)` iff `F′ ⊇ F` under predicate identity
(§3.7) — dropping predicates widens, adding narrows. `T(F) ⊑ T`.
Semantic implication between different predicates is never inferred.

**Unions.** `T ⊑ A | B` if `T ⊑ A` or `T ⊑ B`, or — when `T` is itself
a union — each arm of `T` subsumes into some arm. `A | B ⊑ T` iff
`A ⊑ T` and `B ⊑ T`.

**Intersections.** `A & B ⊑ A` and `A & B ⊑ B`; `T ⊑ A & B` iff
`T ⊑ A` and `T ⊑ B`.

**Arrays.** `T₁[σ₁] ⊑ T₂[σ₂]` iff `T₁ ⊑ T₂` and `σ₁ ⊆ σ₂`.

**Maps.** `{ [K₁]: V₁ } ⊑ { [K₂]: V₂ }` iff `K₁ ⊑ K₂` and `V₁ ⊑ V₂`.

**Records** — over the four member kinds (V3), comparing declared
members only (closedness excluded — D10). `R′ ⊑ R` iff for every member
`m` of `R`:

| `m` in `R` | requirement on `R′` |
|---|---|
| required `m: T` | `R′` declares `m` as required, defaulted, or derived, with type `⊑ T` |
| optional `m?: T` | `R′` omits `m`, or declares it (any kind) with type `⊑ T` |
| defaulted `m: T = e` | as for optional — plus, evaluated values of `R′` need not reproduce `R`'s default (the default is `R`'s completion rule, not a value constraint) |
| derived `const m …` | `R′` declares `m` (any kind) with type `⊑` the declared/inferred type of `R`'s `m` |

and additionally `R′` has no member that `R` declares with an
incompatible kind (a derived member of `R` met by a derived member of
`R′` with a different defining expression is still `⊑`-compatible if
the types agree — expressions are not compared). The judgment on
mutually recursive records is defined **coinductively**: a pair
`(R′, R)` under test is assumed to hold while its members are checked
(implementations memoize visited pairs; the greatest fixed point is the
defined relation).

**Quantities.** `quantity<D₁> ⊑ quantity<D₂>` iff the dimension vectors
are equal.

**References.** `ref<T₁> ⊑ ref<T₂>` iff `T₁ ⊑ T₂`.

**Function types.** `(A₁) => R₁ ⊑ (A₂) => R₂` iff `A₂ ⊑ A₁`
(contravariant) and `R₁ ⊑ R₂` (covariant). Used for lambda arguments
and predicate payloads.

`⊑` is exposed to tools as a queryable operation in the CLI/LSP phase
(D13); this chapter's definition is what that query answers.

## 3.18 Assignability and checking

One judgment serves every checking site (D13):

- **Static assignability** — an expression of inferred type `S` is
  assignable where `T` is expected iff `S ⊑ T` — with the two
  type-directed readings of references
  ([07. Relationships](07_relationships.md) §7.4): a navigation
  expression in a `ref<T>` position denotes the reference (assignable
  iff the location's type `⊑ T`), and a `ref<S>`-typed expression in a
  non-reference `T` position denotes the target's value (assignable
  iff `S ⊑ T`).
- **Literal construction** — an object/array literal checked against `T`
  is checked member-wise (each provided member against its declared
  type; required members present; defaulted/derived members *omitted*
  — they are completed by evaluation, [05. Declarations](05_declarations.md));
  plus the closedness check (§3.11) against undeclared members. A
  provided derived member follows the restatement rule below.
- **Input binding** — a bound document is checked as a literal
  construction of the target type, with one addition: a supplied
  **derived** member is accepted iff its value equals the computed one
  (restatement, D4) — this is what lets serialized output, which
  includes derived members, re-bind (D29). A supplied **defaulted**
  member simply overrides the default.
- **Union discrimination** and **function arguments** reduce to `⊑` on
  the discriminated arm and the parameter types respectively.

*Example / counterexample:*

```decl
type Service = { name: string, port: 1..65535 = 8080, const tag = `s:${name}` }

output ok: Service  = { name: "a" }                    // port completed, tag derived
output bad: Service = { name: "a", tag: "s:b" }        // error: derived restated unequal
```

## 3.19 Uninhabited types

A type whose value set is empty is an error at the point that creates
it. **Structural emptiness must be detected at compile time** (D12):

- an empty range (`1..0`) or empty array-size set;
- an intersection with clashing primitives (`int & string`), disjoint
  ranges or literals (`1..10 & 20..30`, `"a" & "b"`), or member-kind
  conflicts (§3.13);
- a record whose required member has an uninhabited type;
- a recursive composition cycle with no absent-capable member (§3.1).

Emptiness that hinges on predicate semantics
(`int(is_even, is_odd)`-style) is **not** detected statically —
predicates are opaque functions — and surfaces when a value is
constructed or bound against the type.

## Open questions

None.

---

## Previous / Next

- Previous: [02. Lexical Structure](02_lexical.md)
- Next: [04. Expressions](04_expressions.md)
- Index: [Documentation home](../README.md)
