# 13. Standard Library

The normative definition of `std.*`: signatures, semantics, and error
conditions. `std` is ambient (§8.5) — no import, version fixed by this
specification. The v0.1 scope is deliberately small: the functions this
specification and the validation corpus actually use (ROADMAP §0.3);
additions come by revision, append-only like everything else.

## 13.1 Conventions

- Signatures are written in language syntax with **type variables**
  (`<T>`): standard-library functions may be polymorphic. This is a
  documented asymmetry — user `func`s are monomorphic in v0.1;
  builtins carry the polymorphism the collections need.
- Every function is pure, total on its stated domain, and
  deterministic (P2). A call outside the stated domain raises the
  evaluation error **E5008** (std domain error) with the function name
  and offending arguments as params; it never returns a sentinel.
- Arguments are evaluated per the ordinary rules — a maybe-absent or
  invalid argument follows §4.10 and §9.7 before the function is ever
  entered.
- Namespaces below are the complete v0.1 surface; `std.graph` is
  reserved (§13.10). Names not listed do not exist.

## 13.2 `std.array`

| Signature | Semantics |
|---|---|
| `count<T>(xs: T[]): int` | number of elements |
| `all<T>(xs: T[], p: (T) => bool): bool` | `true` iff `p` holds for every element; `true` on `[]` |
| `any<T>(xs: T[], p: (T) => bool): bool` | `true` iff `p` holds for some element; `false` on `[]` |
| `all_distinct<T>(xs: T[]): bool` | `true` iff no two elements are equal (§4.5 equality, including place equality for references) |
| `filter<T>(xs: T[], p: (T) => bool): T[]` | elements satisfying `p`, in order |
| `fold<T, A>(xs: T[], init: A, f: (A, T) => A): A` | left fold: `f(…f(f(init, xs[0]), xs[1])…)` — the language's general iteration (D17) |
| `sum(xs: int[]): int` / `sum(xs: float[]): float` | left-to-right sum; `0`/`0.0` on `[]`; float rounding per element in array order (§9.5) |

Predicates run under ordinary evaluation: an erroring predicate makes
the call error (root-cause at the element's path). `all`/`any` are
short-circuiting in cost only — by §9.4 the result is as if every
element were visited.

## 13.3 `std.math`

| Signature | Semantics |
|---|---|
| `abs(n: int): int` / `abs(x: float): float` | absolute value |
| `min` / `max` (two `int`s or two `float`s) | smaller / larger operand |
| `clog2(n: int): int` | ceiling of log₂ — smallest `k` with `2^k >= n`; **domain `n >= 1`** (E5008 otherwise) |
| `floor(x: float): int` | greatest integer `<= x` |
| `ceil(x: float): int` | least integer `>= x` |
| `round(x: float): int` | nearest integer, ties to even (banker's rounding — deterministic) |

`floor`/`ceil`/`round` are total: every finite binary64 has an exact
integer neighbor, and `int` is arbitrary-precision. There is no `sqrt`,
`sin`, `pow` in v0.1 — nothing in the spec or corpus needs them, and
each carries rounding-specification weight (P7) better paid when a use
case arrives.

## 13.4 `std.int`

| Signature | Semantics |
|---|---|
| `of(x: float): int` | exact conversion; **domain: `x` has no fractional part** (E5008 otherwise — use `floor`/`ceil`/`round` to choose a rounding) |
| `at_least(n: int): (int) => bool` | predicate `v >= n` — the one-sided-range predicate (§3.4) |
| `at_most(n: int): (int) => bool` | predicate `v <= n` |

## 13.5 `std.float`

| Signature | Semantics |
|---|---|
| `of(n: int): float` | nearest binary64, ties to even; **domain: magnitude within binary64's finite range** (E5008 otherwise — D24 forbids ±Infinity) |

## 13.6 `std.string`

| Signature | Semantics |
|---|---|
| `of(v: int \| float \| bool): string` | exactly the template-interpolation conversions (§4.11): decimal, shortest round-trip, `true`/`false` |
| `length(s: string): int` | number of Unicode code points (not bytes, not UTF-16 units — fixed for determinism) |
| `join(xs: string[], sep: string): string` | elements joined with `sep`; `""` on `[]` |

## 13.7 `std.object`

| Signature | Semantics |
|---|---|
| `merge<T>(base: T, patch: T): T` | deep merge of two values of the **same record type** |

The bias and conflict rules D16 promised, exhaustively:

- Per member: present in one side only → taken. Present in both:
  record-typed → **recurse**; everything else — scalars, arrays, maps,
  quantities, references — → **`patch` wins whole** (arrays are
  replaced, never spliced; element-wise array merging has no
  order-independent meaning).
- Absent stays absent unless the other side supplies; an explicit
  `null` in `patch` wins over a value in `base` (`null` is a value,
  D5).
- Open-record pass-through fields merge by the same per-key rule;
  opacity is preserved (§3.11).
- Derived members are not merged — they are recomputed on the result
  (they were never inputs, D4).
- `merge` is associative but **not commutative** — it is the ordered
  layering operation (`base` then `patch`), the deep counterpart of
  shallow `with` (§4.12); order-independent composition of
  *constraints* is `&` (§3.13), a different tool for a different job.

## 13.8 `std.map`

| Signature | Semantics |
|---|---|
| `keys<V>(m: { [string]: V }): string[]` | keys in insertion order (D23) |
| `values<V>(m: { [string]: V }): V[]` | values in insertion order |
| `entries<V>(m: { [string]: V }): { key: string, value: V }[]` | key/value records in insertion order |

Maps are not directly iterable (§4.8); these three are the bridge to
array comprehensions. Counting is `std.array.count(std.map.keys(m))`.

## 13.9 `std.units` — the SI catalog

The full SI catalog (D15), as ordinary `dimension`/`unit` declarations
made ambient with the rest of `std`:

- **Base dimensions and base units**: `Time`/`s`, `Length`/`m`,
  `Mass`/`kg`, `Current`/`A`, `Temperature`/`K`, `Amount`/`mol`,
  `LuminousIntensity`/`cd`.
- **Derived dimensions with named units**: `Frequency = Time ^ -1`
  (`Hz`), `Force` (`N`), `Pressure` (`Pa`), `Energy` (`J`), `Power`
  (`W`), `Charge` (`C`), `Voltage` (`V`), `Resistance` (`Ohm`),
  `Capacitance` (`F`), `DataSize` (`bit`, `B = 8 bit` — the one
  non-SI dimension the corpus domain needs).
- **Prefixed forms**: every SI prefix from `y` (1e-24) to `Y` (1e24)
  applied to every base and named derived unit symbol
  (`ns = 1e-9 s`, `GHz = 1e9 Hz`, `KiB`? — no: binary prefixes `Ki Mi
  Gi Ti` apply to `bit` and `B` only, decimal prefixes do not apply to
  `B`'s binary family).
- The catalog is **declarations, not mechanism** (D15): user code may
  declare further dimensions and units alongside it, and the unit
  symbols occupy the ordinary `std`-protected name space — a user
  module cannot redeclare `ms`.

The exact enumerated list ships as an appendix of the conformance
suite; a conforming implementation exposes precisely it.

## 13.10 Reserved: `std.graph`

The namespace `std.graph` is reserved for the finite-fixpoint
combinators of D18 (`closure`, `reachable`, `is_acyclic`, …). Whether
they enter v0.1 is decided by the evaluator spike (OQ3, ROADMAP §0.6);
until then the namespace is empty and unusable.

## Open questions

None here — the one pending question (`std.graph` admission) is OQ3 in
the design charter, gated on spike evidence by design.

---

## Previous / Next

- Previous: [12. Errors and Diagnostic Codes](12_errors.md)
- Index: [Documentation home](../README.md)
