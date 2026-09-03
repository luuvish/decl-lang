# 06. Constraints and Diagnostics

Evaluation always yields *(resolved values, diagnostics)* (P4). This
chapter defines where diagnostics come from, what they carry, and how
severities behave: `assert` and `when` constraint members, `diagnostic`
declarations, type-level `else` clauses, invalidation, and root-cause
reporting. The machine-readable report format and the code registry are
[12. Errors](12_errors.md).

## 6.1 What a diagnostic is

Every diagnostic carries:

| Field | Content |
|---|---|
| **id** | the stable occurrence identity (§6.2, §6.4, §6.5) |
| **severity** | `error`, `warn`, or `info` |
| **message** | rendered text from a template |
| **path** | where it occurred (§6.7) |
| **template** | the referenced `diagnostic` declaration's id, when one was used |
| **params** | the argument values passed to that template |

Diagnostics arise from three sources: **type checks** — a value failing
its type (generic report, or a type-level `else`, §6.5); **constraint
members** — `assert` conditions (§6.2); **evaluation errors** — the
conditions of §4.14 and [09. Semantics](09_semantics.md). All three
share the fields above and the ordering rule of §6.7.

## 6.2 `assert` members

```decl
assert symmetric: num_inputs == num_outputs

assert width_match: source.width == target.width
    else width_mismatch(source.width, target.width)

assert scaled: replicas in 1..16
    else warn `replicas ${replicas} is outside the recommended range`
```

- Form: `assert <name>: <bool-expr> [else <tail>]`. The condition is
  checked for **every value** of the enclosing type — each element of
  `services: Service[]` runs `Service`'s asserts.
- **Stable id**: `<module-path>.<TypeName>.<assert-name>`. The name
  lives in the schema's single name space (D19). The id is the
  contract: editing the condition text, the message, or moving the
  assert into a `when` group does not change it; renaming does, and is
  a breaking change to consumers that track diagnostics by id.
- The `else` tail is one of:
  - **absent** — a failure produces an `error` diagnostic with the
    fixed message `assert <name> failed` (deterministic; the path and
    id carry the specifics);
  - **inline** — `else error|warn|info` followed by a template string;
    the template may interpolate sibling members and locals under the
    rules of §4.11;
  - **reference** — `else <diagnostic-name>(args)`; the arguments are
    expressions evaluated only when the assert fails, checked against
    the declaration's parameter types (§6.4). The report's severity and
    message come from the referenced declaration; its id remains the
    assert's, with the template id recorded alongside.
- **Condition outcomes**: `true` — no diagnostic. `false` — the `else`
  diagnostic. **Evaluation error** in the condition (division by zero,
  …) — the evaluation-error diagnostic is reported at the assert's
  path *instead of* the else diagnostic; the assert neither passes nor
  fails. A condition that touches an **invalidated** value is skipped
  entirely (§6.6). An error evaluating a reference tail's arguments
  likewise replaces the intended diagnostic.

## 6.3 `when` groups

```decl
when data_width > 64 {
    assert wide_buffer: buffer_size >= 256
}

when "buffer" in cfg {
    assert sized: (cfg.buffer ?? 0) >= 16
}
```

- `when <cond> { … }` guards the constraints it contains: they are
  checked when the condition is `true` and skipped — passing silently —
  when it is `false`. Nested `when`s conjoin. The guarded asserts keep
  their ordinary ids (nesting does not rename).
- The condition is a `bool` expression over siblings and context; an
  `in` condition narrows inside the group (§4.10).
- A condition that errors, or that touches an invalidated value, skips
  the group under root-cause rules (§6.6): the underlying defect is
  already reported once.

## 6.4 `diagnostic` declarations

```decl
diagnostic width_mismatch(src: int, dst: int) {
    severity = error
    message = `source width ${src} != target width ${dst}`
}
```

- A module-level, exportable declaration of a reusable diagnostic
  **template**: typed parameters, a severity, and a message template.
  Its **catalog id** is `<module-path>.<name>`.
- The message template interpolates **its parameters only** — not
  arbitrary scope. This is what makes the declaration a catalog unit:
  it can be listed, translated, and documented in isolation (D20).
- Many asserts (and type declarations, §6.5) may reference one
  template; each occurrence keeps its own id and passes its own
  arguments.
- Stability policy (D20): the catalog id is immutable; the message
  text is freely editable; a severity change alters invalidation
  semantics (§6.6) and is a breaking revision, to be recorded as such.

## 6.5 Type-level `else`

```decl
type Port = 1..65535
    else error `port must be between 1 and 65535`

diagnostic bad_name(v: string) {
    severity = error
    message = `service name ${v} must be lowercase kebab-case`
}
type ServiceName = /[a-z][a-z0-9-]*/ else bad_name
```

- A **named** type declaration may carry the same `else` tail as an
  assert (D20). When a value fails the type — at construction, input
  binding, or assignment — the attached diagnostic **replaces** the
  generic type-mismatch report at the same path.
- The occurrence id is `<module-path>.<TypeName>`. In the reference
  form, the offending value is bound to the template's **first
  parameter** — no sigil is needed. In the inline form the template is
  static text; the path and the reported actual value carry the
  specifics.
- The severity **must** be `error`: a type is hard admissibility
  (D19's division of labor) — a referenced template with `warn`/`info`
  severity is a compile error at the type declaration. Soft guidance
  belongs to asserts.

## 6.6 Severities, invalidation, and root-cause reporting

- **`error` invalidates.** The value at the diagnostic's path becomes
  *invalid*: it does not appear in serialized output
  ([10. Interchange](10_interchange.md)) and taints its dependents.
- **Taint propagates, reporting does not** (D20). Every member whose
  evaluation references an invalid value becomes invalid **silently**;
  every assert or `when` condition that touches an invalid value is
  skipped **silently**. One defect produces exactly one report — the
  root cause — no matter how wide its cone of dependents.
- Independent defects are all reported: two unrelated failing members
  yield two diagnostics.
- **`warn` and `info` preserve.** The value stands, nothing is
  tainted, evaluation and serialization proceed.
- Severity comes from the diagnostic that fires, not from the
  construct: an assert whose `else` is `warn` never invalidates; a
  failing type check always does.

```decl
type Router = {
    ports: int
    half = ports / 2          // ports invalid → half invalid, silently
    assert even: ports % 2 == 0     // skipped when ports is invalid
}
```

Binding `{ "ports": "eight" }` yields **one** diagnostic — the type
mismatch at `.ports` — not three.

## 6.7 Paths and ordering

- Every diagnostic carries the **path** of its occurrence, in the
  canonical path syntax of [07. Relationships](07_relationships.md):
  the evaluation root's name followed by `.member`, `[index]`, and
  `["key"]` segments.
  - A type-check or evaluation-error diagnostic points at the value it
    judged (`demo.services[2].port`).
  - An assert's diagnostic points at the **record instance** whose
    assert fired (`demo.services[2]`) — the assert's id names the rule;
    the path names the culprit instance.
- Evaluation- and validation-time diagnostics sort by **(path, id)** —
  path in canonical path order
  ([07. Relationships](07_relationships.md)), then id lexicographically.
  Compile-time diagnostics precede them, ordered by source location;
  [12. Errors](12_errors.md) §12.3 fixes the full order. Together the
  list is byte-stable across implementations (D20, P2).

## 6.8 Where each pipeline stage reports

Lexical, parse, name-resolution, and type-check diagnostics arise in
their stages ([09. Semantics](09_semantics.md)) and persist to the end
alongside constraint-stage diagnostics; all share the format of §6.1
and the registry of [12. Errors](12_errors.md). This chapter's
machinery — templates, invalidation, root-cause, ordering — applies
uniformly: a parse error in one declaration does not suppress
diagnostics elsewhere (parser resilience, P6), and a type error taints
exactly like an assert error.

## Open questions

None.

---

## Previous / Next

- Previous: [05. Declarations and Schemas](05_declarations.md)
- Next: [07. Relationships](07_relationships.md)
- Index: [Documentation home](../README.md)
