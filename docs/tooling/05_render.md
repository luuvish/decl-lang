# The renderer — `--format` and `decl render`

Evaluation ends at a resolved value tree and its diagnostics; the
specification defines interchange with JSON alone (§10.6) and the
requirements keep rendering out of the language core (01_requirements
§2). Rendering is therefore **tooling**: the `decl` command line emits
an evaluated document in another structured format, or through a
template into text, and the three implementations do so identically.
This document is informative; it fixes the formats and the template
dialect, and the corpus that holds the three implementations to them.

```
decl evaluate <file> [--input name=doc.json]... [--output name[=file]]... [--format json|yaml]
decl render <file> --template <path> [--input name=doc.json]... [--output name[=file]]...
```

## 1. Structured formats — `--format`

`--format` selects the text form of every document `evaluate` emits;
`json` (the default) is the canonical JSON of §10.4.

- **`yaml`**: YAML 1.2, core schema, block style. Members in canonical
  order (§7.2), arrays as sequences, objects as mappings. Every string
  that the core schema would read as anything but a string — `true`,
  `no`, `null`, `~`, `1e3`, `0x10`, a date-like or empty string — is
  double-quoted; every key is quoted on the same rule. Integers print
  in full, floats in the shortest round-trip form with a `.0` where JSON
  would carry one, quantities as `{ value, unit }` mappings, references
  as their canonical path strings. No anchors, no tags, no flow style.
  A reader that follows the core schema reads back exactly the JSON
  document; the Norway problem cannot arise because nothing is left
  bare that a schema could coerce.
- **`toml`** (after the phase decides its loss rule): TOML has no null,
  so a document holding `null` cannot be represented; the rule is
  either an error (exit 1, one diagnostic naming the path) or omission
  of the member. Until decided, `toml` is not accepted.

A format other than JSON is a *tool-side conversion* in §10.6's sense:
the mapping is documented here, not normative, and `--input` reads JSON
only.

## 2. Templates — `decl render`

`decl render` evaluates a universe as `evaluate` does, then renders
each `--output` root through the template: the root's document is the
template's context, and the rendered text goes where the document
would have (a file, or standard output for at most one).

The template dialect is Decl's own — a small, fixed subset of the
Jinja family, implemented three times, never delegated to a host
engine: Jinja2, minijinja, and nunjucks differ in whitespace control,
filters, and number printing, and the three implementations must print
identical bytes; Sailfish compiles templates at build time, which a
user's template cannot be.

### 2.1 Syntax

- `{{ expr }}` — an expression, rendered as text (§2.3).
- `{% if expr %} … {% elif expr %} … {% else %} … {% endif %}`
- `{% for x in expr %} … {% endfor %}`, with `loop.index`,
  `loop.index0`, `loop.first`, `loop.last`, `loop.length`; over a
  mapping, `{% for k, v in expr %}` in canonical key order.
- `{# … #}` — a comment.
- Whitespace: `{%-` and `-%}` strip the adjacent whitespace, as in
  Jinja; nothing else is stripped; the template's line endings are kept.
- No `set`, `macro`, `include`, `extends`, `block`, or `raw` in the
  first delivery; a template is one file over one document.

### 2.2 Expressions

An expression is a **Decl expression** (§4) over the context: the
document's members by name (`name`, `services[0].port`,
`params["mtu"]`), the operators, `if … then … else`, comprehensions,
and the standard library (`std.array.count(services)`) — the same
evaluator, so a template computes nothing the language could not. `x
| f(args)` is `f(x, args)` — a pipeline over a function of `std.*` or
of the module; there are no template-only filters.

### 2.3 Text form

A string renders as itself; an integer in full; a float in the
shortest round-trip form; `true` / `false` / `null` as those words; a
quantity as its base-unit value followed by a space and the unit
(`3000 m`); a reference as its canonical path; an array or object as
its canonical JSON. No escaping is applied (the output format is the
template author's); `| json` renders any value as canonical JSON, and
`| yaml` as §1's YAML.

### 2.4 Errors

A template that does not parse is `E7001` (a new band for rendering,
registered in §12) with the line; an expression that fails to evaluate
is reported as the language reports it, with the path
`<template>:<line>`; a root that fails validation is not rendered
(exit 1, the diagnostics as `evaluate` prints them).

## 3. The library and the editors

The three APIs gain `render(path, template, options)` in the
`evaluate` vocabulary, and `evaluate` gains `format`. The extension's
output preview offers the format and, given a template setting, the
rendered text; the REPL's `:evaluate` takes `--format`, and `:render
<template> <root>` prints the text.

## 4. The corpus

`tests/render/` holds, per case, a module, its documents, the template,
and the rendered text the reference produces (reviewed); a `formats/`
manifest pairs golden documents with their YAML form. The harness gains
a `render` section (`decl render` and `--format` rows, byte for byte);
each implementation's suite replays the corpus.

## 5. Status

Planned (Phase 10). Open before the phase starts: TOML's loss rule; the
exact set of `loop` variables; whether `render` reads a template from
standard input. The specification does not change beyond an editorial
pointer from §10.6 to this document.

## 6. Verification

Every case of `tests/render/` identical across the three
implementations in the harness; every construct of §2.1 and every text
form of §2.3 with a corpus case; a YAML document of every golden read
back by a YAML 1.2 reader equal to the golden.

