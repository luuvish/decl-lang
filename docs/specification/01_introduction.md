# 01. Introduction

This chapter states what Decl is, the concepts every later chapter builds
on, and the conventions the specification itself follows.

## 1.1 What Decl is

Decl is a general-purpose declarative language for structured data. One
source provides three capabilities:

- **Describe** — declare types, schemas, constraints, and diagnostics.
- **Generate** — evaluate defaults, derived members, and comprehensions
  into a fully resolved value tree, exportable to JSON.
- **Validate** — judge language-defined and externally bound values against
  the same rules, producing diagnostics with stable ids.

These are not modes. They are three views of a single evaluation semantics
(P4): evaluating anything always yields the pair

> **(resolved values, diagnostics)**

Partial validity is a normal outcome, not an error path: a value can be
returned together with the diagnostics that describe exactly how it falls
short.

Two properties frame everything else:

- **JSON superset** (P3): every JSON document is a valid Decl value
  literal. External data becomes a language value without conversion.
- **Pure, deterministic, terminating** (P2): expressions have no side
  effects, recursion is impossible, every evaluation terminates, and the
  same input yields bit-identical values and diagnostics in every
  conforming implementation.

## 1.2 Core concepts

**Types and schemas.** A `type` declaration names a set of admissible
values: primitives, literals, ranges (`1..65535`), patterns
(`/[a-z]+/`), predicate refinements (`int(is_aligned)`), records, arrays,
maps, unions (`A | B`), intersections (`A & B`), generics, and quantities
(`quantity<Time>`). Record types are schemas: their members divide into
**value members** (`x: T`, `x?: T`, `x: T = e`, `const x = e`), which
become the evaluated value, and **constraint members** (`assert`, `when`),
which produce diagnostics only.

**Evaluation roots.** Nothing evaluates until a root asks for it:

- `output x: T = e` — a value this module produces. The type is mandatory
  and the value runs the full pipeline: type check → defaults → derived
  members → constraint validation.
- `input x: T` — a declared slot for external data. A tool binds a JSON
  document to it; the bound value runs the *identical* pipeline.

`const x = e` at module level is a pure constant — not a root, not
schema-validated, and not a legal reference target.

**References.** Property values are owned compositions forming a tree.
`ref<T>` declares a non-owning reference into the trees under evaluation
roots; references may form graphs and cycles, and a dangling reference is
a validation error.

**Diagnostics.** Constraints (`assert`) and `diagnostic` declarations are
language constructs with stable ids (`<module>.<type>.<assert>`),
severities (`error` / `warn` / `info`), parameters, and message
templates. An `error` invalidates the affected value and its dependents
but is reported once, at the root cause. Named type declarations may
attach their own error diagnostic with an `else` clause.

**Modules.** A file is a module. Visibility is explicit (`export`,
named `import`), shadowing is banned everywhere, and dependency versions
are exact-pinned under a content-hashed lock. The standard library `std`
is ambient — available without import in every module.

## 1.3 A first example

```decl
type Port = 1..65535
    else error `port must be between 1 and 65535`

type Service = {
    name: /[a-z][a-z0-9-]*/
    port: Port = 8080
    replicas: int = 1

    const endpoint = `${name}:${port}`

    assert scaled: replicas in 1..16
        else warn `replicas ${replicas} is outside the recommended range`
}

output demo: Service = { name: "gateway" }

input external: Service
```

Evaluating `demo` yields
`{ "name": "gateway", "port": 8080, "replicas": 1, "endpoint": "gateway:8080" }`
and an empty diagnostic list. Binding an external JSON document to
`external` validates it under exactly the same rules — defaults filled,
`endpoint` derived, `scaled` checked.

## 1.4 Authority and precedence

- The design charter
  ([02. Design Decisions](../design/02_design_decisions.md), P1–P7 and
  D1–D30) binds this specification: no chapter may contradict it. On
  discovering a conflict, the chapter is corrected, or the charter is
  revised first.
- Within the specification, if a chapter's prose and the formal grammar
  ([11. Grammar](11_grammar.md)) diverge, **the grammar wins**, and the
  divergence is corrected immediately upon discovery.
- Code blocks marked `decl` are examples. Examples are informative;
  the prose around them is normative. Counterexamples are marked as such.

## 1.5 Conformance

- The normative verbs are **must** (a requirement), **must not** (a
  prohibition), **may** (an allowance), and **should** (a recommendation a
  conforming implementation can deviate from only with documented
  reason).
- A conforming implementation, given the same module set, lock state, and
  input bindings, must produce byte-identical serialized values and an
  identical, identically ordered diagnostic list — across platforms and
  implementations (P2; [09. Evaluation Semantics](09_semantics.md) fixes
  the numeric and ordering rules that make this possible, and
  [12. Errors](12_errors.md) §12.3 states the one scoped relaxation for
  syntax-recovery diagnostics).
- Every error condition named by this specification receives a code in
  [12. Errors and Diagnostic Codes](12_errors.md).

## 1.6 Chapter map

| Chapter | Contents |
|---|---|
| 02. Lexical Structure | source text, tokens, keywords, literals, separators |
| 03. Type System | all type forms, subsumption, assignability |
| 04. Expressions | operators, conditionals, lambdas, comprehensions |
| 05. Declarations and Schemas | type/const/func/output/input, members |
| 06. Constraints and Diagnostics | assert, when, diagnostic, severities, ids |
| 07. Relationships | composition, ref<T>, navigation, reverse queries |
| 08. Modules and Packages | import/export, manifest, lock |
| 09. Evaluation Semantics | pipeline, laziness, determinism, numerics |
| 10. Data Interchange | JSON mapping, input binding, serialization |
| 11. Grammar | the formal grammar (EBNF) |
| 12. Errors and Diagnostic Codes | code registry and report format |
| 13. Standard Library | `std.*` signatures, semantics, error conditions |

## Open questions

None.

---

## Previous / Next

- Next: [02. Lexical Structure](02_lexical.md)
- Index: [Documentation home](../README.md)
