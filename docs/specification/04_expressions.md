# 04. Expressions

Expressions compute values. Every expression is pure (no side effects),
total (evaluation terminates), and deterministic (P2); how and when
expressions are evaluated is fixed in [09. Semantics](09_semantics.md).
This chapter defines the forms, the static typing rules, and the
evaluation-error conditions of each.

Expressions appear in: default and derived members, `assert` conditions,
`when` conditions, predicate arguments, `output` values, `func` bodies,
diagnostic message templates, and constant positions (§4.13).

## 4.1 Primary expressions

- Literals: numbers, strings, `true`/`false`/`null`, unit literals
  (`10ms`), templates (§4.11), patterns only in `matches` (§4.7).
- Name references: constants, function names, parameters, comprehension
  and lambda bindings, `match`-arm bindings. Resolution and the
  no-shadowing rule are in [08. Modules](08_modules.md).
- Context variables (`$this`, `$parent`, …): valid only inside schema
  member expressions; semantics in
  [07. Relationships](07_relationships.md).
- Parenthesized expressions `(e)`.

## 4.2 Object and array construction

```decl
{ name: "gateway", port: 8080 }
[1, 2, 3]
{ ...defaults, region: "eu" }      // error if `defaults` also has `region`
[...head, x, ...tail]
```

- Object members are `key: expr` with keys per §2.3; entry order is
  preserved (D23). A duplicate key — written twice, or written and also
  produced by a spread — is an error: **overriding is not construction's
  job**; use `with` (§4.12).
- Array spread `...e` splices an array-valued expression in place.
  Object spread `...e` copies the entries of an object-valued
  expression; collisions with other entries or spreads are errors.
- A construction member whose expression is maybe-absent is a compile
  error (§4.10) — there is no implicit member dropping. Discharge the
  absence (`??`, an `in` guard) or build conditionally.
- **Sibling references in typed construction**: within an object
  literal checked against a *record type* (§3.18), entry expressions
  are member expressions of the instance under construction — bare
  names resolve **up the chain of enclosing instances under
  construction** (nearest first, then module scope; §7.3), and the
  references join the member dependency graph (§9.3, acyclic as
  ever). The chain is essential: an edge literal nested inside a
  node's literal reaches the node's `ports` by name (§7.4). In a
  literal not checked against a record type (a map, a bare object),
  names resolve in the ordinary lexical scope only.

## 4.3 Operators and precedence

From tightest to loosest; binary operators are left-associative except
`..`/`..<`, equality, and the comparisons (all non-associative — no
chaining) and `=>` (right). `with` sits between levels 1 and 2:
`-a with { … }` is `-(a with { … })`, and `a * b with { … }` is
`a * (b with { … })`:

| Level | Operators | Notes |
|---|---|---|
| 1 | `e.m` `e?.m` `e[i]` `f(args)` | access, index, call |
| 2 | unary `!` `-` `~` | |
| 3 | `*` `/` `%` | |
| 4 | `+` `-` | |
| 5 | `<<` `>>` | shifts |
| 6 | `..` `..<` | ranges (§4.6) |
| 7 | `<` `<=` `>` `>=` `in` `matches` | comparison, membership, match |
| 8 | `==` `!=` | equality |
| 9 | `&` | bitwise and |
| 10 | `^` | bitwise xor |
| 11 | `\|` | bitwise or |
| 12 | `&&` | logical and |
| 13 | `\|\|` | logical or |
| 14 | `??` | fallback (§4.10) |
| 15 | `\|>` | pipeline (§4.9) |
| 16 | `if…then…else`, `match` | whole-expression forms |
| 17 | `(params) => body` | lambda, body extends right |

- Mixing `??` with `&&` or `||` without parentheses is a **compile
  error** (D16) — the classic precedence footgun is removed rather than
  resolved. *Counterexample:* `a && b ?? c` — error; write
  `(a && b) ?? c` or `a && (b ?? c)`.
- Comparison and equality do not chain: `a < b < c` is an error (the
  first comparison yields `bool`, which `<` rejects).

**Access forms.** `e.m` reads a declared record member with an
identifier-shaped name — resolved statically against `e`'s type
(§3.11). `e[k]` reads: an array element (`k: int`; out of bounds is an
evaluation error, §4.14), a map entry (`k: string`; maybe-absent,
§4.10), or a record member whose name the dot cannot spell — not
identifier-shaped, or one of the literal keywords `true`/`false`/`null`
(`r["my-key"]`, `r["null"]`; reserved keywords such as `type` are
ordinary dotted names, D33) — then `k` must be a string literal, and
the access is as static as `.`. One name, one form: bracket access to a dot-spellable record
member (`r["port"]`) is a compile error. Records read with `.`, maps
always with `[…]`; the quoted form exists only for names the dot
cannot write. Access on a **union-typed** operand is legal when every
arm declares the member; the result types as the union of the arms'
member types — reading a common member never requires discrimination
(§3.12).

## 4.4 Arithmetic, bitwise, and shifts

- Arithmetic requires both operands of the **same** numeric kind —
  `int`×`int`, `float`×`float`, or quantity rules below. `int`/`float`
  mixing is a type error (D6, D7); convert explicitly with
  `std.float.of` / `std.int.of` ([13. Stdlib](13_stdlib.md)).
- `int` arithmetic is arbitrary precision — no overflow exists.
  `/` on ints is **truncating division** (toward zero); `%` is the
  remainder with the sign of the dividend (`-7 / 2 == -3`,
  `-7 % 2 == -1`). Division or remainder by zero is an evaluation
  error (D24).
- `float` arithmetic follows IEEE 754 binary64 round-to-nearest-even,
  with the D24 guard: any operation whose IEEE result would be NaN or
  ±Infinity is an evaluation error instead (`1.0 / 0.0`, `0.0 / 0.0`,
  `1e308 * 10.0`). No FMA contraction, no extended intermediates (D23).
- Bitwise `& | ^ ~` and shifts `<< >>` operate on `int` only (type
  error otherwise). Values are conceptually infinite two's-complement:
  `~n == -n - 1`; `n << k` is `n · 2^k`; `n >> k` is floor division by
  `2^k` (sign-preserving). A negative shift count is an evaluation
  error.
- **Quantities** (§3.16): `+` `-` require equal dimensions; `*` `/`
  compose dimensions; a plain number scales a quantity. Results carry
  the composed dimension; a fully cancelled dimension yields the plain
  numeric value.
- Unary `-` negates `int`, `float`, or a quantity.
- **Static result types (D31)**: `int` `+`/`-`/`*` over operands whose
  static types are ranges or literals type as the interval-arithmetic
  range of their endpoints; `/` and `%` type as `int`; `float`
  arithmetic types as `float`.

## 4.5 Comparison, equality, membership

- Ordering `< <= > >=`: `int`×`int`, `float`×`float`, `string`×`string`
  (lexicographic by Unicode code point), quantity×quantity of equal
  dimension. Anything else is a type error.
- Equality `== !=` is **structural**: primitives by value, quantities by
  dimension-checked magnitude (converted to the base unit), arrays
  element-wise, objects entry-wise (order-insensitive for equality),
  references by target path — and when either operand is
  reference-typed, both operands are read as **places** (`$this` and
  navigation chains denote their locations) and compared by canonical
  path ([07. Relationships](07_relationships.md) §7.4). Otherwise
  operands must have overlapping types
  (`S₁ & S₂` inhabited) — `1 == "1"` and `1 == 1.0` are type errors,
  not `false`. Since NaN does not exist, `==` is reflexive.
- `x in e`: membership. `e` may be an array (element equality —
  including place equality when elements are references, §7.4), a range
  (bounds check, same base type), a map (**key** membership,
  `x: string`), or a record — then `x` must be a string **literal**
  naming an *optional* member of `e`'s type, and the result is that
  member's presence (§4.10; any other record key is a static error —
  the answer would be a constant). Result `bool`.
- `s matches /re/`: whole-string match of `s` against the pattern;
  `bool`. The right operand is a pattern literal, not a string.

## 4.6 Range expressions

`lo..hi` and `lo..<hi` are first-class range values over `int` or
`float` endpoints of the same kind (mixed kinds: type error). Ranges are
used by `in` (§4.5) and iterated by comprehensions (§4.8; iteration
requires `int` endpoints — iterating a float range is a type error).
Range values do not serialize ([10. Interchange](10_interchange.md)) and
cannot be value-member types' values — they live inside expressions.

## 4.7 Conditionals and `match`

```decl
if replicas > 8 then "large" else "small"
```

- `if c then a else b`: `c` must be `bool`; exactly the taken branch is
  evaluated (so the untaken branch cannot raise evaluation errors). The
  result type is the union of the branch types. There is no ternary
  operator (D16).

```decl
const area = match shape {
    (c: Circle) => 3.141592653589793 * c.radius * c.radius
    (r: Rect)   => r.w * r.h
}
```

- `match e { arms }` inspects a value of a **discriminable union**
  (§3.12). Each arm is `(name: Type) => expr` — the lambda shape, read
  as "when the value is a `Type`, call it `name`". The arm applies when
  the value's discriminant selects a variant with `variant ⊑ Type`;
  `name` is bound with that arm's type inside `expr`.
- Arms must be **pairwise disjoint** (no variant selected by two typed
  arms) and **exhaustive** (every variant selected by some arm) — both
  checked statically; violating either is a compile error. Order of
  arms therefore never matters.
- At most one **catch-all** arm `(name) => expr` (no type) may appear;
  it covers exactly the variants no typed arm covers, and `name` is
  bound at the union of those variants. A catch-all when typed arms are
  already exhaustive is an error (dead arm).
- For a union of literals, arm types are the literals:

```decl
const port = match protocol {
    (p: "http") => 80
    (p: "grpc") => 50051
    (p: "tcp")  => 4000
}
```

- Arms are separated by newline or comma (§2.9). The result type is the
  union of all arm result types. Only the selected arm is evaluated.

## 4.8 Comprehensions

```decl
[f(x) for x in xs if p(x)]
[x + y for x in xs for y in ys]
{ s.name: s for s in services if s.public }
```

- Array form: one or more `for` clauses, each optionally followed by
  `if` filters; clauses nest left-to-right, elements are produced in
  source iteration order (D23).
- Iterables are **arrays** and **int ranges** (`for i in 0..<8`). Maps
  are iterated through `std.map.keys` / `values` / `entries` — a map is
  not directly iterable (one concept, one syntax: iteration is over
  arrays).
- **The iteration variable's type**: over an array, the element type;
  over a range, **the range itself as a type** (§3.4) — in
  `for p in 0..<8`, `p: 0..<8`, so `p` is directly assignable where
  `0..7` is expected. Without this rule every range-driven generation
  would fail its own bounds statically.
- Map form `{ k(x): v(x) for … }`: key expressions must be `string`; a
  duplicate produced key is an evaluation error (D23).
- Comprehension variables are bindings, not assignments; the
  no-shadowing rule applies (a comprehension variable cannot reuse an
  enclosing name).

## 4.9 Calls, lambdas, and the pipeline

- Calls are `f(a, b)` — positional arguments only, arity checked
  statically, each argument assignable to its parameter (§3.18). There
  are **no method calls and no trailing blocks** (D16):
  `xs.count()` does not exist; write `std.array.count(xs)`.
- Lambdas `(x) => e`, `(x: T, y: U) => e`: parameter types may be
  omitted where the context determines them (argument positions,
  predicate positions); a lambda in a context that fixes no parameter
  types must annotate. Lambdas capture enclosing bindings by value.
- **Function values are not data**: a function-typed value cannot be a
  value member's type, an element of an array/map, or the type of an
  `output`/`input`. Functions flow only through parameters, returns,
  and predicate positions (§3.7).
- Pipeline `x |> f` is call notation, not a new operation:
  **first-argument insertion** — `x |> f` ≡ `f(x)`, and
  `x |> f(a, b)` ≡ `f(x, a, b)`. Chains associate left:

```decl
ports |> std.array.filter((p) => p.mode == "input")
      |> std.array.count
```

## 4.10 Absence, `null`, and their operators

Absence and `null` are distinct (D5): absence is a declared-optional
member not being there — never a value — while `null` is an ordinary
value. Both are tracked **statically**: every expression has a type
(which may include `null`) and a **maybe-absent** flag, which arises
only from optional-member and map access. The three operators answer
three different questions:

| Question | Form | when absent | when `null` |
|---|---|---|---|
| is it present? | `"m" in e` | `false` | `true` — null is a present value |
| navigate past a gap | `e?.m` | absent | absent |
| usable value, or fallback | `e ?? d` | `d` | `d` |

Presence is a question about the **container**, so it is asked with the
membership operator (§4.5) — `"m" in e` for a record's optional member,
`k in m` for a map key. No expression form ever turns an absent
"value" into a question: there is no `exists()`; the only consumers of
a maybe-absent expression are `?.` and `??`.

**Static discipline** — the rules that make "absent used" impossible at
runtime:

- `e.m` requires `e` to be definitely present and non-null; otherwise
  it is a compile error and `?.` must be used. Its result is
  maybe-absent iff `m` is optional.
- A maybe-absent expression may be consumed only by `?.` and `??`,
  avoided under a narrowing guard (below), or written in a **`ref<T>`
  position** — there it denotes a place (§7.4), and whether the place
  holds a value is *reference integrity* (a dangling-reference check,
  §7.5), not absence discipline. Any other consumption — arithmetic,
  call argument, construction member (§4.2), comparison — is a
  **compile error**.
- `e?.m` — when `e` is `null` or absent, the result is absent; else
  `e.m`. This deliberately collapses "no value on the way" and "no
  value here" into one state: `?.` exists to feed `??`, which treats
  them alike. When the distinction matters, test **before** collapsing
  (`in`, `== null`).
- **`?.` always guards its left operand, never the member being
  accessed.** `a?.x` asks nothing about `x`: if `x` is optional, plain
  `a.x` is already maybe-absent with no operator involved — `x`'s
  absence is *tested* by `"x" in a` and *defaulted* by `a.x ?? d`.
  In a chain `a?.b?.c`, each `?` looks left: the first guards `a`, the
  second guards the result of `a?.b`. The three `?` positions each
  live on a different axis (D5):

  | Position | Form | Meaning |
  |---|---|---|
  | declaration | `x?: T` | this member may be **absent** |
  | type | `T?` | this value may be **null** (`T \| null`) |
  | expression | `a?.x` | pass safely when the **left side** is null/absent |
- `a ?? b` — `b` when `a` is absent **or** `null` (the nullish
  reading); only the needed operand is evaluated. The result is
  definitely present, typed `nonnull(A) | B`.
- **Map access** `m[k]` is maybe-absent (the key may be missing); the
  presence test is `k in m` (§4.5).

**Narrowing** — the only two flow-narrowing rules in the language, both
over navigation paths `P` (`a.b.c`) in `if`/`when` conditions:

- `"m" in P` narrows `P.m` to definitely-present in the true
  branch/group (likewise `k in P` for a map access `P[k]`, including a
  lambda- or comprehension-bound `k`);
- narrowing flows through the logical operators: in `A && B`, `A`'s
  narrowing holds within `B` (and in any true-branch guarded by the
  whole expression); in `A || B`, `A`'s **negation** narrows `B` —
  so `k in m && m[k] == x` and `!("x" in a) || a.x > 0` are both
  well-typed;
- `P != null` narrows `P`'s type to exclude `null` in the true branch
  (`P == null` narrows it in the false branch).

```decl
type Cfg = { name: string, buffer?: int? }   // absent, null, or int

const b1 = cfg.buffer ?? 128
// int: 128 when buffer is absent OR null

const b2 = if "buffer" in cfg then cfg.buffer else 128
// int?: absent → 128, but an explicit null is preserved —
// "not configured" defaults, "explicitly disabled" stays null

when "buffer" in cfg {
    assert sized: (cfg.buffer ?? 0) >= 16   // buffer: int? — null still possible
}
```

*Counterexample:* `cfg.buffer + 1` — compile error twice over: the
operand is maybe-absent and its type admits `null`.

## 4.11 String templates

`` `text ${expr} text` `` concatenates literal parts with converted
interpolations. An interpolated value must be `string`, `int`, `float`,
or `bool`; the conversions are fixed for determinism: strings verbatim,
ints in decimal, floats in shortest round-trip form (D29), bools as
`true`/`false`. Interpolating `null`, absent, quantities, objects,
arrays, or references is a type/evaluation error — convert explicitly
(`std.string.of`, [13. Stdlib](13_stdlib.md)).

## 4.12 `with` — record update

```decl
const eu_service = base with { region: "eu", replicas: 3 }
```

- `base with { … }` produces a **new** object: `base`'s value-member
  entries, with the listed members replaced or (for optional members)
  supplied. **Derived members are not copied** — they are recomputed
  by whatever pipeline consumes the result; copying them would trip
  D4's restatement rule the moment an update changed one of their
  dependencies (the §0.6 spike hit exactly this with a copied
  `insecure` after enabling TLS). Shallow — nested objects are
  replaced whole; deep merging is `std.object.merge` with its
  specified bias rules.
- Statically: `base`'s type must be a record type; every updated key
  must be a declared member (against a closed type, unknown keys are
  errors) with an assignable value; a **derived** member cannot be
  updated (it is recomputed — updating it is the restatement trap of
  D4). The result type is `base`'s type.
- `with` cannot remove a member and cannot make a present optional
  member absent.
- On an **unbound literal** (e.g. a constructor func's result before
  it reaches a typed position), `with` merges entries and the result
  stays unbound (D32); unbound literals support only member/index
  access, `with`, and embedding — deeper reshaping chains are errors.

## 4.13 Constant expressions

Positions that the type surface evaluates at elaboration time — range
endpoints (§3.4), array sizes (§3.9), value arguments (§3.15), unit
factors (§3.16), predicate arguments (§3.7) — take **constant
expressions**: expressions built from literals, operators, module
`const` references, generic **value parameters** in scope (§3.15), and
`func` calls, with no `input`, no `output` references, and no context
variables. Constant evaluation follows the
same semantics and error rules as runtime evaluation (an erroring
constant is a compile-time diagnostic).

## 4.14 Evaluation errors in expressions

The expression-level **evaluation**-error conditions (codes in
[12. Errors](12_errors.md)): division/remainder by zero; float
operation producing NaN or ±Infinity; negative shift count; duplicate
key from construction, spread, or a map comprehension; array index out
of bounds. (Null member access and absent-value consumption are
compile errors under §4.10's static discipline, not evaluation
errors; template interpolation of a non-convertible value, `in` on
incompatible operands, and quantity arithmetic on unequal dimensions
are type errors, §4.4–4.5, §4.11.) Where and when these surface — lazily, and attributed to
root causes — is defined in [09. Semantics](09_semantics.md).

## Open questions

None.

---

## Previous / Next

- Previous: [03. Type System](03_types.md)
- Next: [05. Declarations and Schemas](05_declarations.md)
- Index: [Documentation home](../README.md)
