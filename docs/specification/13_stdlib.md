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
- A type variable may carry a **bound**: `T: int | float` means `T`
  instantiates to exactly one **arm** of the bound per call — `int` or
  `float`, never an arbitrary subtype — so results type at the base
  primitive (`sum` over a `(64 | 128)[]` argument instantiates
  `T = int` via array covariance and returns `int`). Bounds are what
  keep the standard library **overload-free**: one signature per name
  — §5.3's no-overloading rule holds for `std` too.
- Every function is pure, total on its stated domain, and
  deterministic (P2). A call outside the stated domain raises the
  evaluation error **E5008** (std domain error) with the function name
  and offending arguments as params; it never returns a sentinel.
- Arguments are evaluated per the ordinary rules — a maybe-absent or
  invalid argument follows §4.10 and §9.7 before the function is ever
  entered.
- Namespaces below are the complete v0.1 surface; `std.graph` is
  reserved (§13.11). Names not listed do not exist.

## 13.2 `std.array`

| Signature | Semantics |
|---|---|
| `count<T>(xs: T[]): int` | number of elements |
| `all<T>(xs: T[], p: (T) => bool): bool` | `true` iff `p` holds for every element; `true` on `[]` |
| `any<T>(xs: T[], p: (T) => bool): bool` | `true` iff `p` holds for some element; `false` on `[]` |
| `all_distinct<T>(xs: T[]): bool` | `true` iff no two elements are equal (§4.5 equality, including place equality for references) |
| `filter<T>(xs: T[], p: (T) => bool): T[]` | elements satisfying `p`, in order |
| `fold<T, A>(xs: T[], init: A, f: (A, T) => A): A` | left fold: `f(…f(f(init, xs[0]), xs[1])…)` — the language's general iteration (D17) |
| `sum<T: int \| float>(xs: T[]): T` | left-to-right sum; `0`/`0.0` on `[]`; float rounding per element in array order (§9.5) |

Predicates run under ordinary evaluation: an erroring predicate makes
the call error (root-cause at the element's path). `all`/`any` are
short-circuiting in cost only — by §9.4 the result is as if every
element were visited.

## 13.3 `std.math`

| Signature | Semantics |
|---|---|
| `abs<T: int \| float>(x: T): T` | absolute value |
| `min<T: int \| float>(a: T, b: T): T`, `max<T: int \| float>(a: T, b: T): T` | smaller / larger operand; mixing `int` and `float` fails to instantiate `T` — a type error |
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
| `starts_with(s: string, prefix: string): bool` | `true` iff `s` begins with `prefix` (code-point-wise) |

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
- **The defaults trap — read before layering configs**: `patch` is an
  ordinary `T` value, so by the time `merge` sees it, its omitted
  defaulted members are already **filled** (§5.7) — and those filled
  defaults then win over `base`'s explicit values like any other
  `patch` member. For environment layering over a schema with
  defaults, use `with` (nesting it for depth): `with` updates exactly
  the members written and nothing else. `merge` is for combining maps
  and open bags where default completion does not interfere.

## 13.8 `std.map`

| Signature | Semantics |
|---|---|
| `keys<V>(m: { [string]: V }): string[]` | keys in insertion order (D23) |
| `values<V>(m: { [string]: V }): V[]` | values in insertion order |
| `entries<V>(m: { [string]: V }): { key: string, value: V }[]` | key/value records in insertion order |

Maps are not directly iterable (§4.8); these three are the bridge to
array comprehensions. Counting is `std.array.count(std.map.keys(m))`.

## 13.9 `std.ref`

| Signature | Semantics |
|---|---|
| `path<T>(r: ref<T>): string` | the **absolute** canonical path of the target (§7.2) — always absolute, never the `$`-relative wire form |

`path` is what makes locality constraints expressible — "this edge's
endpoints lie inside me": `std.string.starts_with(std.ref.path(e.source),
$path)`. It is deterministic (canonical paths are unique) and it is the
only operation that turns a reference into data other than
serialization itself.

## 13.10 `std.units` — the SI catalog

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
- **Prefixed forms — the generation rule is the normative inventory**:
  the twenty SI prefixes (`y z a f p n u m c d da h k M G T P E Z Y`,
  1e-24 through 1e24, `u` standing for micro) apply to every base unit
  except `kg` and to every named derived unit (`ns = 1e-9 s`,
  `GHz = 1e9 Hz`). Mass prefixes attach to `g` (`unit g = 1e-3 kg`;
  `mg`, `ug`, …) — `kg` itself takes no prefix. `bit` and `B` take the
  binary prefixes `Ki Mi Gi Ti Pi Ei` and the decimal `k M G T P E`;
  the other SI prefixes do not apply to them.
- The catalog is **exactly** the base and named units listed above plus
  the set this rule generates — nothing more; the conformance suite
  enumerates it for testing, but the rule here is the definition.
- The catalog is **declarations, not mechanism** (D15): user code may
  declare further dimensions and units alongside it. The catalog
  occupies only the **unit and dimension name spaces** (§3.16) — a
  user module cannot redeclare the *unit* `ms`, but `const ms = 5` and
  `const s = 1` remain perfectly legal: ambient units pollute no value
  names, keeping D16's provenance story intact.

## 13.11 Reserved: `std.graph`

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
