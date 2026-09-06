# 12. Errors and Diagnostic Codes

Every error condition this specification names has a code here (§1.5).
This chapter defines the code scheme, the machine-readable report
format, the ordering and conformance rules, and the registry itself.

## 12.1 The code scheme

- Codes are `E####` (error conditions), `W####` (language-defined
  warnings), `I####` (informational). Bands follow the pipeline
  (§9.1):

  | Band | Stage |
  |---|---|
  | E1000–E1999 | lexical |
  | E2000–E2999 | syntax |
  | E3000–E3999 | names, modules, packages |
  | E4000–E4999 | types |
  | E5000–E5999 | evaluation |
  | E6000–E6999 | validation and binding |
  | E7000–E7999 | rendering (tooling: [05. Renderer](../tooling/05_render.md)) |

- **Codes are immutable and append-only** (D20): a code's meaning never
  changes; retired conditions keep their number reserved; new
  conditions take new numbers. The registry below freezes with v0.1.
- A **code classifies the condition; an id identifies the occurrence**
  (§6.1). User-fired diagnostics keep class codes stable: a failing
  `assert` reports E6001/W6001/I6001 by its severity, a type-level
  `else` keeps the type-mismatch class E4001 — custom messages and
  templates never move a diagnostic out of its class, so tooling that
  filters by code is immune to message churn.

## 12.2 The report format

A diagnostic serializes as:

```json
{
  "code": "E6001",
  "id": "netlib/schemas.decl:Service.scaled",
  "severity": "warn",
  "message": "replicas 20 is outside the recommended range",
  "path": "net.services[2]",
  "location": { "file": "schemas.decl", "line": 14, "col": 5 },
  "template": "netlib/diag.decl:width_mismatch",
  "params": { "src": 64, "dst": 32 }
}
```

- `code`, `severity`, `message` are always present.
- `id` — the stable occurrence id, rendered
  `<package>/<module-path>:<TypeName>.<assert>` for asserts,
  `<package>/<module-path>:<TypeName>` for type-level `else`
  (§6.2, §6.5); language-detected conditions (no declared rule) omit
  it.
- `path` — the canonical value path (§7.2); present for
  evaluation-time and validation-time diagnostics.
- `location` — source file, 1-based line and column; present for
  compile-time diagnostics, and for evaluation-time ones when the
  erring expression has a source span.
- `template`, `params` — present when a `diagnostic` declaration was
  referenced (§6.4); `params` values serialize per
  [10. Interchange](10_interchange.md).

## 12.3 Ordering and conformance

- **Compile-time diagnostics precede evaluation- and validation-time
  diagnostics.** Among compile-time ones, order is
  `(file, line, col, code)`; among the rest, `(path, id)` per §6.7.
- Conformance (§1.5) requires identical lists field-for-field, with
  one scoped relaxation: for **syntax-band codes (E2xxx)** the required
  agreement is `(code, location)` — the message text and the number of
  cascading syntax diagnostics after a recovery point are
  implementation-quality concerns, since independent parsers recover
  differently. Every other band requires the full field set to match
  byte-for-byte. (Toolchains built on the canonical tree-sitter
  grammar will in practice agree on E2xxx too.)

## 12.4 Registry

### E1xxx — lexical (§2.11)

| Code | Condition |
|---|---|
| E1001 | malformed or unknown token (§2.11: invalid UTF-8 or BOM, unterminated literal/template/pattern/comment, unknown escape, malformed number, keyword as identifier, unknown `$`-token or character) |
| E1002 | unit literal on a non-decimal base (§2.7) |

### E2xxx — syntax (ch. 11)

| Code | Condition |
|---|---|
| E2001 | input does not match the grammar |
| E2002 | rejected-syntax form (`;`, `let`, ternary `?:`, `where`, wildcard import/export — §2, D16) |

### E3xxx — names, modules, packages (§5.1, ch. 8)

| Code | Condition |
|---|---|
| E3001 | duplicate name in module |
| E3002 | shadowing a predeclared name |
| E3003 | unknown name |
| E3004 | module not found |
| E3005 | importing a name the module does not export |
| E3006 | import collides with an existing binding |
| E3007 | module import cycle |
| E3008 | namespace name used as a value |
| E3009 | import from `"std"` |
| E3010 | package not declared in `[dependencies]` |
| E3011 | manifest: unknown field (fail-closed) |
| E3012 | manifest: version is not an exact pin |
| E3013 | manifest: invalid package name |
| E3014 | conflicting versions required for one package |
| E3015 | lock: missing entry |
| E3016 | lock: version differs from manifest |
| E3017 | lock: content-hash mismatch |
| E3018 | root-name collision in the evaluation universe |
| E3019 | a local binding (comprehension, lambda, match arm) shadows an enclosing name |

### E4xxx — types (ch. 3, §4.3–4.13, §5.7–5.9)

| Code | Condition |
|---|---|
| E4001 | value does not satisfy the expected type (assignability, `⊑`) |
| E4002 | required member missing |
| E4003 | undeclared member on a closed record |
| E4004 | duplicate member in one construction |
| E4005 | derived member restated with a differing value (D4) |
| E4006 | hidden member supplied by a document or a literal (§5.7, D34) |
| E4010 | mixed-kind range endpoints |
| E4011 | empty range (`lo > hi`) or empty array-size set |
| E4012 | structurally uninhabited type (intersection clash, hopeless recursion, empty required member) |
| E4013 | record union arms not discriminable |
| E4014 | more than one non-record object arm in a union |
| E4015 | map key type not string-shaped |
| E4020 | predicate is not a `(T) => bool` expression |
| E4021 | non-constant expression in a constant position (§4.13) |
| E4022 | generic arity or argument mismatch |
| E4023 | value argument outside its parameter's type |
| E4030 | inheritance widens an inherited member (D21) |
| E4031 | extending a non-record type |
| E4032 | illegal member-kind transition in an override (§5.9) |
| E4040 | quoting a member name that must be bare, or vice versa (§3.11) |
| E4041 | bracket access to a dot-spellable member, or `.` on one that is not (§4.3) |
| E4050 | maybe-absent expression consumed outside `?.`/`??`/guards (§4.10) |
| E4051 | member access on a possibly-null expression without `?.` |
| E4052 | `??` mixed with `&&`/`\|\|` without parentheses |
| E4053 | chained comparison |
| E4054 | `in` on a record key that is not an optional member (§4.5) |
| E4060 | function-typed value in a data position (§4.9) |
| E4061 | lambda parameter types not inferable from context |
| E4062 | call arity mismatch |
| E4070 | template interpolation of a non-convertible value (§4.11) |
| E4071 | operand kinds invalid for the operator (incl. `int`/`float` mixing) |
| E4072 | quantity arithmetic or comparison across dimensions |
| E4073 | unknown unit, unit of the wrong dimension, or a second base unit for a dimension |
| E4080 | `with` on a non-record base, updating a derived member, an unknown member, or removing one |
| E4090 | embedding site fails a declared context bound, or gives a context variable no meaning (§7.3) |
| E4094 | context variable used without a context declaration, or a duplicate/invalid context declaration (§7.3) |
| E4091 | `$referrers`: first argument not a record type, or no compatible `ref` position |
| E4092 | `$referrers`: second argument not a string literal naming such a member |
| E4093 | `ref` position navigating a module `const`, a hidden member, or other non-root value (§7.5) |
| E4100 | `match` arms overlap |
| E4101 | `match` not exhaustive |
| E4102 | `match` catch-all is dead |
| E4103 | `match` on a non-discriminable subject |
| E4110 | type-level `else` on an anonymous type, or with non-error severity (§6.5) |
| E4111 | `when` group containing a value member |
| E4112 | recursion in the `func` reference graph (§5.3) |
| E4113 | dependency cycle visible in the type structure (§9.3) |
| E4114 | duplicate member name within one record declaration (value or constraint member — §3.11) |
| E4115 | comprehension over a non-iterable value or a float range (§4.8) |
| E4116 | range value in a data position (§4.6) |
| E4117 | pattern interpolation `${T}` of a type neither string- nor integer-shaped (§3.6) |
| E4118 | ambiguous member combination in an intersection: conflicting defaults, or duplicate derived members (§3.13) |
| E4119 | malformed pattern body: outside the portable regular-expression core (§3.6) — the reason is one of a fixed set (`unterminated character class`, `nothing to repeat`, `unbalanced parenthesis`, `unsupported construct (?`, `unsupported escape \x`, …), identical across implementations |

### E5xxx — evaluation (§4.14, ch. 9)

| Code | Condition |
|---|---|
| E5001 | division or remainder by zero |
| E5002 | float operation producing NaN or ±Infinity (D24) |
| E5003 | negative shift count |
| E5004 | duplicate key produced at evaluation time (spread, map comprehension) |
| E5005 | array index out of bounds |
| E5006 | unbound `input` demanded (§5.6) |
| E5007 | dependency cycle detected at evaluation time (§9.3) |
| E5008 | standard-library function called outside its domain ([13. Stdlib](13_stdlib.md)) |

### E6xxx — validation and binding (ch. 6, 7, 10)

| Code | Condition |
|---|---|
| E6001 | `assert` failed (error severity — user or default diagnostic) |
| E6002 | dangling reference: path does not resolve, or is non-canonical (§7.5) |
| E6003 | reference target root not in the evaluation universe |
| E6004 | bound document cannot be read, or is not well-formed JSON or YAML, or uses a YAML construct outside the core schema (§10.2; the YAML reader is tooling, [05. Renderer](../tooling/05_render.md) §2) |

### E7xxx — rendering (tooling: [05. Renderer](../tooling/05_render.md))

The renderer is tooling, not language (§10.6, D35): these codes are
registered here so that the band is reserved and the three
implementations report them alike; their conditions are fixed by the
renderer's own document.

| Code | Condition |
|---|---|
| E7001 | a template does not parse: an unclosed or unknown tag, a tag out of place, an expression that is not a Decl expression, an include cycle (05_render §5.8) |
| E7002 | a value with no text form in a value tag: absent, or a function (05_render §5.5) |
| E7003 | a template file cannot be read (05_render §3.3) |
| E7004 | an invalid `@render` annotation (05_render §3) |
| E7005 | a fan-out path that is not a string, is empty, is absolute, leaves the destination directory, or repeats (05_render §6) |

### Warnings and information

| Code | Condition |
|---|---|
| W0001 | unknown annotation (§5.10) |
| W0002 | use of a declaration annotated `@deprecated` |
| W6001 | `assert` failed with `warn` severity |
| I6001 | `assert` failed with `info` severity |

## 12.5 Coverage rule

Every error condition named by chapters 02–11 and 13 must map to
exactly one code above; a condition without a code, or a code without a
naming chapter, is a specification defect found by the §0.5
cross-consistency pass. New conditions introduced by revisions append
new codes; they never reuse or renumber.

## Open questions

None.

---

## Previous / Next

- Previous: [11. Grammar](11_grammar.md)
- Next: [13. Standard Library](13_stdlib.md)
- Index: [Documentation home](../README.md)
