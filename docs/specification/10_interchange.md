# 10. Data Interchange

This chapter defines the two crossings of the language boundary:
**input binding** — a JSON document becomes a validated value — and
**serialization** — an evaluated value becomes a JSON document. The
governing law is total round-trip idempotence (D29, §10.5): everything
the language can emit, it can re-bind.

## 10.1 The JSON relationship

- Every JSON document is a valid Decl value literal (P3, D1): objects,
  arrays, strings, numbers, `true`/`false`/`null` parse and mean the
  same. Two Decl liberalizations are irrelevant to JSON documents
  (unquoted identifier keys, newline separators), and one reading
  differs harmlessly: `-5` parses as unary minus applied to `5`
  (§2.6), which evaluates to the same value in every position a JSON
  number can occupy.
- The **data subset** is the value forms JSON itself can carry. Two
  Decl value kinds cross the boundary in dedicated encodings rather
  than as themselves: **quantities** as `{ "value": v, "unit": "u" }`
  objects (§3.16) and **references** as canonical path strings (§7.4).
  Both encodings are type-directed: the *expected type* at a position
  decides whether an object is a quantity and whether a string is a
  path. Nothing else needs encoding — every other Decl value is
  already a JSON value.

## 10.2 Input binding

A tool binds a JSON document to `input x: T` (§5.6). The document must
be well-formed JSON (RFC 8259, §11.7) — a malformed document fails the
binding as a whole, before any checking. Binding is then the
literal-construction check of §3.18 followed by the ordinary pipeline:

1. **Structure**: the document is checked member-wise against `T`.
   Required members must be present; optional members may be absent;
   a supplied defaulted member overrides its default; a supplied
   **derived** member is a restatement — accepted iff equal to the
   computed value (D4); a supplied **hidden** member is an error
   (E4006 — it is never part of a value, §5.7). Against a **closed** record, undeclared
   members are rejected; against an **open** one they pass through as
   opaque fields (§3.11). Entry order is preserved as document order
   (D23).
2. **Special forms, type-directed**: where `quantity<D>` is expected,
   exactly a `{ "value": number, "unit": string }` object binds, its
   unit symbol resolving to a unit of dimension `D` (§3.16) — any
   other shape there, or that shape where a plain record is expected,
   is an ordinary type mismatch. Where `ref<T>` is expected, the
   document must hold a **canonical path string** (§7.2): either
   **document-relative** — `"$.services[0]"`, resolved against the
   root this document is bound to — or absolute, resolved against the
   evaluation universe. A path that does not resolve — including a
   non-canonical spelling — is a dangling-reference error (§7.5). An
   absolute reference into another root requires that root in the
   universe (§8.8).
3. **Pipeline**: defaults fill, derived members compute, constraints
   validate — identically to an `output` (D22). The result is the
   ordinary pair *(value, diagnostics)*.

Binding never mutates the document notionally: absent stays absent
(no null-for-missing invention), `null` binds as the value `null`
(the target type must admit it), and numbers bind as `int` or `float`
by their lexical form (§2.6) — `1.0` does not bind where `int` is
expected. One asymmetric leniency exists at this boundary *(D29
amended, v0.1.7)*: where **`float`** is expected, an **integer
lexeme** binds iff it is exactly representable in binary64 — real
documents routinely serialize whole floats as `500`. The bound value
is the float; re-serialization emits the canonical `500.0`. The
reverse never holds: a float lexeme does not bind as `int`.

## 10.3 Serialization policy

Serializing an evaluated value (an exported `output`, or a bound
`input` re-emitted):

- **Absent** members are not emitted; **`null`** is emitted (D5).
- **Derived** members are included by default; a tool option may
  exclude them (D29). **Hidden** members (§5.7) are never emitted.
  Defaulted members are emitted with their
  effective value — filled or overridden — indistinguishably.
- **Member order**: the value's entry order — declaration order for
  values built in-language, document order for input-bound values —
  with members materialized by evaluation (filled defaults, derived
  members) appended in declaration order after the supplied ones.
- **Numbers**: `int` in plain decimal (no separators, no exponent),
  at full precision — JSON's grammar has no digit limit; consumers
  with 64-bit float parsers may lose precision on huge integers, and
  that is the consumer's documented concern, not a reason to truncate.
  `float` in the shortest round-trip form (the ECMAScript
  `Number::toString` algorithm, D29) — **with `.0` appended whenever
  that form is lexically an integer** (the float 10 emits as `10.0`):
  numbers bind by lexical form (§10.2), so a bare `10` would re-bind
  as an `int` and break the round trip at every integral-valued float
  (the §0.6 spike's quantity magnitudes hit this immediately). Parsing
  the emitted text yields the identical binary64.
- **Quantities**: `{ "value": v, "unit": "u" }`, normalized to the
  dimension's **base unit** (§3.16, §9.5) — one canonical encoding
  per value, whatever unit the source used.
- **References**: a canonical path string (§7.2) —
  **document-relative** (`"$.services[0]"`) when the target lies under
  the same evaluation root as the reference, absolute (rooted at the
  target root's name) when it lies under another. The rule is
  deterministic, and it is what keeps serialized documents
  self-contained: an emitted document never embeds its own root's
  name, so it can re-bind to an input slot of **any** name (§10.5).
  References are never inlined by the canonical serializer;
  denormalizing tools may exist, but their output is presentation,
  not the canonical form (a fixed answer to inlining's cycle problem:
  there is nothing to normatively inline).
- **Invalid values** (§9.7) are excluded: an invalid member is
  omitted from its parent's emission, and its diagnostics accompany
  the result; a wholly invalid root emits no value — diagnostics
  only. The emitted document plus the diagnostic list is the
  serialized form of the *(value, diagnostics)* pair — a document
  with omissions is only ever presented together with the diagnostics
  that explain them.

## 10.4 Canonical JSON text

Byte-identical conformance (P2, §9.5) requires one canonical text:

- UTF-8, no byte-order mark, LF where a tool chooses to break lines.
- The canonical form is **compact**: no insignificant whitespace.
  Pretty-printed output is a presentation option and is not the
  canonical byte sequence (hashes and byte comparisons use the
  compact form).
- Strings escape only what JSON requires: `"` `\` and control
  characters (`\b \f \n \r \t`, `\u00XX` for other controls);
  everything else is emitted as literal UTF-8 — no gratuitous
  `\uXXXX`.
- Object member order is §10.3's order; arrays keep element order.

## 10.5 Round-trip idempotence

Normative and total (D29, V1): for every evaluable value,

```
serialize(eval(v))  --bind-->  eval  --serialize-->  identical bytes
```

- Re-binding succeeds and validates: every emitted form has a defined
  input form — quantities (§10.2.2), references (§10.2.2), derived
  members (restatement, D4), `null`, big integers, shortest-float
  texts. Intra-root references make the trip because they serialize
  document-relative (§10.3): the emitted document never names its own
  root, so it binds to an input of any name — the slot name cannot
  collide with the output's (§5.1, §8.8), which is exactly why an
  absolute self-path would have broken this property. Cross-root
  references re-bind whenever the named roots are in the universe —
  which they are, when re-binding within the universe that produced
  them.
- Re-serialization is byte-identical: member order is preserved by
  the document-order rule, floats re-parse to the same binary64 and
  re-emit the same shortest form, quantities are already
  base-normalized, references are already canonical.
- The property is checked by the conformance suite over the
  validation corpus (ROADMAP §0.6); any value class that cannot make
  this trip is a specification defect by definition — the previous
  iteration's quantity hole is the precedent this rule exists to
  prevent (00_vision §3).

## 10.6 Scope: JSON only

v0.1 normatively defines interchange with **JSON alone**. YAML, TOML,
and other formats are tool-side conversions with no normative mapping
— deliberately: each carries its own type-coercion folklore (the
Norway problem is the article that started this project, 00_vision
§1), and a normative mapping would import it. This is a recorded
decision, not an omission.

## Open questions

None.

---

## Previous / Next

- Previous: [09. Evaluation Semantics](09_semantics.md)
- Next: [11. Grammar](11_grammar.md)
- Index: [Documentation home](../README.md)
