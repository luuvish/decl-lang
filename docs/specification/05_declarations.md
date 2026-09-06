# 05. Declarations and Schemas

This chapter defines the module-level declaration forms — `const`,
`func`, `type`, `output`, `input`, `diagnostic`, `dimension`/`unit` —
and the members of a schema (record type). Modules and visibility are
[08. Modules](08_modules.md); evaluation order is
[09. Semantics](09_semantics.md).

## 5.1 Modules and the single name space

A module is a sequence of declarations separated by newlines (§2.9).
All declaration kinds share **one name space per module**: a module
cannot declare a `type Port` and a `const Port` — the second is a
duplicate-name error. Together with the no-shadowing rule (D27), every
name in a module resolves to exactly one declaration, visible at a
glance.

Declaration forms at module level:

```decl
const max_width = 256                    // §5.2
func clog2(n: int): int = …              // §5.3
type Service = { … }                     // §5.4
output demo: Service = { … }             // §5.5
input external: Service                  // §5.6
diagnostic bad_port(v: int) { … }        // 06. Constraints
dimension Time                           // 03. Types §3.16
unit s: Time
import / export …                        // 08. Modules
```

## 5.2 `const` — module constants

```decl
const max_services = 64
const window: int = max_services * 4
```

- `const name [: T] = expr` binds a name to a value. The expression is
  a **constant expression** (§4.13): literals, operators, other
  constants, `func` calls — no `input`/`output` references, no context
  variables. With an annotation, `expr`'s type must be `⊑ T`; without,
  the type is inferred.
- A module `const` is a **pure constant** (D22): it is not an
  evaluation root, does not run any schema pipeline, and is **not a
  legal reference target** (`ref<T>` can never point at it). It exists
  to name values, including for use in type positions (range
  endpoints, array sizes, value arguments).

## 5.3 `func` — functions

```decl
func clog2(n: int): int = std.math.clog2(n)
func is_pow2(n: int): bool = n > 0 && (n & (n - 1)) == 0
func divisible_by(d: int): (int) => bool = (n) => n % d == 0
```

- `func name(params): R = expr` — the body is a **single expression**;
  there are no statements (D17). Parameter types are mandatory; the
  return type may be omitted and inferred from the body. When
  annotated, the body's type must be `⊑ R`.
- **No recursion**: the name-reference graph among `func` declarations
  — an edge from `g` to `f` whenever `g`'s body mentions `f`, whether
  as a call or as a value — must be acyclic. This is deliberately
  conservative (mentioning is enough; no call-path analysis), which
  keeps the check trivial and the termination guarantee (P2) airtight.
  Iteration is expressed with comprehensions and `std.array.fold`.
- **No overloading**: one `func` per name (§5.1). No default parameter
  values; every call supplies every argument (§4.9).
- Functions are values only in the restricted sense of §4.9: they pass
  through parameters, returns, and predicate positions, and never enter
  data.

## 5.4 `type` declarations

```decl
type Port = 1..65535
    else error `port must be between 1 and 65535`

type Pair<T> = { first: T, second: T }
type Vec<T, N: 1..1024> = T[N]
```

- `type Name [<params>] = TypeExpr [else …]` — the type forms are
  [03. Types](03_types.md); generic parameters §3.15.
- Type declarations exist at **module level only** — there are no
  nested type declarations inside record bodies. Typing is structural
  (§3.1), so an inner type would buy only a name prefix; a helper type
  private to one schema is a non-exported module type (§8.2), and
  one-off member shapes are anonymous inline types.
- The optional **`else` clause** attaches a custom error diagnostic to
  the *declaration*: when a value fails this type, the attached
  diagnostic replaces the generic type-mismatch report. Forms and
  semantics — including the error-only severity rule — are in
  [06. Constraints](06_constraints.md) §6.5. Anonymous inline
  types cannot carry `else`; name the type.
- Names are transparent aliases (§3.1): the `else` clause and the name
  itself never change the value set.

## 5.5 `output` — values this module produces

```decl
output demo: Service = {
    name: "gateway"
    replicas: 3
}
```

- `output name: T = expr` (D22). The annotation is **mandatory** — an
  output is a claim that a value conforms to a type, and the claim must
  be visible. `expr` may be any expression of the module (not just a
  literal); it may reference constants, functions, and other outputs'
  members by navigation — but not `input`s that are unbound.
- Evaluating an output runs the **full pipeline** on its value: check
  against `T` (§3.18), fill defaults, compute derived members, validate
  constraints — producing *(value, diagnostics)*. Tools treat exported
  outputs as the units of evaluation and serialization
  ([10. Interchange](10_interchange.md)); a non-exported output is a
  module-internal validated value, usable as a reference target
  ([07. Relationships](07_relationships.md)).

## 5.6 `input` — values bound from outside

```decl
input topology: Topology
input env: EnvProfile = { profile: "dev" }
```

- `input name: T [= fallback]` declares a slot the evaluating tool
  binds a document to (D22); binding semantics are
  [10. Interchange](10_interchange.md). The bound value runs the
  pipeline **identical** to an output's.
- The fallback is a constant expression used when the tool binds
  nothing. An unbound, fallback-less input whose value is demanded by
  evaluation is an error; an unbound input that nothing demands is not
  ([09. Semantics](09_semantics.md), laziness).
- An `input` cannot appear in constant positions (§4.13) — types and
  sizes never depend on external data.

## 5.7 Schema members: the four value kinds, and hidden members

Inside a record type, a value member's kind is read off two marks (D4) —
no modifier keywords: `?` says input may supply the member, `= e` says
the schema computes it.

| Form | Meaning | Set by input? | In evaluated value? |
|---|---|---|---|
| `x: T` | required | must | always |
| `x?: T` | optional | may | only if set |
| `x?: T = e` | defaulted | may (overrides `e`) | always |
| `x: T = e`, `x = e` | derived | restate-only (D4) | always |
| `x$: T = e`, `x$ = e` | **hidden** (D34) | must not (E4006) | never |

```decl
type Service = {
    name: ServiceName                  // required
    description?: string               // optional
    port?: Port = 8080                  // defaulted
    replicas?: int = 1

    endpoint = `${name}:${port}` // derived

    assert scaled: replicas in 1..16   // constraint member — 06
        else warn `replicas ${replicas} outside recommended range`
}
```

- **Required** — construction and binding must supply it (§3.18).
- **Optional** — may be absent; absence is not `null` (§3.8, §4.10).
- **Defaulted** — when unsupplied, `e` is evaluated to fill it. A
  supplied value simply overrides; the default expression is then never
  evaluated (so an erroring default cannot hurt a document that
  overrides it). A settable member (`?`) always declares its type — the
  type is the input's contract.
- **Derived** — always computed from `e`; input may only *restate* the
  computed value (equal value accepted, differing value is an error —
  D4). With an annotation, `e` must be `⊑ T`; otherwise the member's
  type is inferred. Derived members are included in serialized output
  by default (D29). A value written in the schema *is* the value: to let
  input change it, write `?`.
- **Hidden** — a derived member whose name ends in `$` (`feeders$`). It
  is computed and read like any derived member — by sibling expressions,
  by other instances' navigations (`edge.source$.width`), by
  `$referrers(T, "m$")` — but it is **not part of the value**: never
  emitted (§10.3), never compared (§4.5, §3.17), never copied by `with`,
  spread, or `std.object.merge`, never a reference target (§7.5). A
  document or literal that supplies it is in error (E4006, §10.2) — a
  hidden name cannot even be written bare in a literal. Only derived
  members can be hidden (D34).
- Default and derived expressions may reference **sibling members** by
  name, and the enclosing context through `$this`/`$parent`/`$root`
  ([07. Relationships](07_relationships.md)). The references form the
  member dependency graph, which must be acyclic; evaluation follows
  it lazily ([09. Semantics](09_semantics.md)). Referencing an
  optional sibling yields a maybe-absent expression with the §4.10
  discipline.
- All members — value and constraint alike — share one name space
  (D19); any duplicate is an error. The canonical ordering (required →
  optional → defaulted → derived → constraints) is formatter style,
  not a language rule: any order parses and means the same.

## 5.8 Constraint members

```decl
assert symmetric: num_inputs == num_outputs
assert width_match: source.width == target.width
    else width_mismatch(source.width, target.width)

when data_width > 64 {
    assert wide_buffer: buffer_size >= 256
}
```

- `assert name: bool-expr [else …]` and `when cond { … }` are the
  constraint members (D19, D20). Their full semantics — severities,
  diagnostic ids, invalidation, reporting — are
  [06. Constraints](06_constraints.md); this chapter fixes their
  placement rules:
  - Constraint members appear only inside record types.
  - A `when` group contains only `assert`s and nested `when`s — never
    value members (conditional *shape* is a tagged union, D19). Nested
    `when`s conjoin their conditions.
  - `when` conditions are `bool` expressions over sibling members; a
    condition referencing an optional member follows §4.10 (use an
    `in` guard or `??`).

## 5.9 Inheritance declarations

```decl
type Child = Parent { label: string, port: 1..1024 }
```

`Parent { … }` extends a record type (D21, §3.14). The body's members
either **add** (a name Parent lacks) or **override** (a name Parent
has). Overriding is narrowing-only; the kind-transition matrix — which
member kinds may replace which — follows directly from the subsumption
table (§3.17):

| Parent member | Child may redeclare as |
|---|---|
| `x: T` (required) | required, defaulted, or derived, with type `⊑ T` |
| `x?: T` (optional) | any kind with type `⊑ T` (required strengthens; defaulted supplies) |
| `x?: T = e` (defaulted) | required (drops the default — stricter), defaulted (may change the default expression), or derived, with type `⊑ T` |
| `x = e` (derived) | derived only, with type `⊑`; the expression may differ (types are compared, expressions are not — §3.17) |
| `x$ = e` (hidden) | hidden only — a hidden member stays hidden (its name is its own; `x` and `x$` are different members) |

Everything else — widening a type, required → optional, derived →
non-derived — is a compile error. Constraint members are inherited
as-is and may not be removed; the child may add more.

*Counterexample:* `type Loose = Service { port: int }` — widening
`Port` to `int`; error.

## 5.10 Annotations and documentation

```decl
/// The public service surface.
@deprecated
type LegacyService = { … }

type Service = {
    @doc("stable dns name")
    name: ServiceName
}
```

- `@name` and `@name(args)` annotations attach to declarations and to
  individual members. Annotations are **metadata only** (D4): no
  annotation affects typing, evaluation, or serialization. The known
  set is `@deprecated` and `@doc("…")`, which tools surface
  (deprecation warnings on use, hover documentation), and — on an
  `output` — `@render({ … })`, which declares the form the tools emit
  the root in (a format and layout, a template, a destination, a
  fan-out); its keys are fixed by the renderer's document
  ([05. Renderer](../tooling/05_render.md) §3, D35), and the annotation
  changes nothing about the root's value.
- An **unknown** annotation is a *warning*, not an error — annotations
  are semantics-free, so unknown ones are safe to carry
  (forward-compatibility); the warning keeps typos visible.
- `///` doc comments (§2.2) attach to the following declaration or
  member and serve the same documentation channel as `@doc`; the
  formatter's canonical form is `///` for prose, `@doc` for short
  inline notes.

## Open questions

None.

---

## Previous / Next

- Previous: [04. Expressions](04_expressions.md)
- Next: [06. Constraints and Diagnostics](06_constraints.md)
- Index: [Documentation home](../README.md)
