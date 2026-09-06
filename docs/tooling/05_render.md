# The renderer — documents in YAML, `--format`, and `decl render`

Evaluation ends at a resolved value tree and its diagnostics; the
specification defines interchange with JSON alone (§10.6) and the
requirements place rendering in the tool chain, not the language core
(01_requirements §2). Rendering is therefore **tooling**, and this
document is its specification: how the `decl` command line reads a
document written in YAML, emits an evaluated document as JSON or YAML
in the text form the user asks for, and renders a document through a
template into text — and how the three implementations do all of it
identically, byte for byte, held together by a corpus the parity harness
replays. This document is informative in the specification's sense
(§1.4); the language does not change, and what it borrows from the
specification it cites.

```
decl evaluate <file> [--input name=doc.(json|yaml)]... [--output name[=file]]... [--format json|yaml] [--indent n | --pretty] [--json]
decl validate <file> [--input name=doc.(json|yaml)]... [--expect-errors E1,E2] [--json]
decl render   <file> --template [root=]path... [--input name=doc.(json|yaml)]... [--output name[=file]]... [--json]
decl repl     [file.decl] [--input name=doc.(json|yaml)]... [--script session.txt | --script -] [--compact]
```

## 1. Conventions

The conventions of [01. Command line](01_cli.md) §1 hold: two streams
(documents and rendered text on standard output, everything addressed
to a person on standard error), options and files in any order, an
option's value in the next argument, repeatable `--input` / `--output`
/ `--template`, files by the path given, exit codes 0 / 1 / 2. Every
form on this page is answered by the three implementations with the
same bytes on both streams and the same exit code.

Three things are new in the vocabulary:

- a **document** on the way in may be JSON or YAML (§2);
- a **format** is the text form of a document on the way out: JSON,
  canonical or indented, or YAML (§3);
- a **template** turns a document into text of any shape (§4).

## 2. Documents in YAML

`--input name=doc.yaml` (or `.yml`) binds a document written in YAML;
any other extension, `.json` included, is read as JSON as before. The
extension decides — nothing is sniffed — and the `--input` of every
command, the REPL's `:bind name=doc.yaml`, and the library's document
paths follow the same rule.

A YAML document is read into the JSON data model and from there on is
indistinguishable from the same document written in JSON: the paths in
diagnostics, the binding rules (§10.2), the completed document, the
round trip. The reader is **YAML 1.2, core schema**, and exactly that:

- mappings become objects in the order written; sequences become
  arrays; plain scalars resolve by the core schema's rules — `true` /
  `false` (the four spellings `true`, `True`, `TRUE`, and the same for
  `false`), `null` / `Null` / `NULL` / `~` / an empty value, decimal and
  hexadecimal and octal integers (`0x1F`, `0o17`), floats with the core
  schema's forms including `.inf` and `.nan` — which are then rejected
  as the language rejects them (§9.5, D18) — and everything else a
  string. **YAML 1.1 is not read**: `yes`, `no`, `on`, `off`, `y`, `n`
  are strings, so the Norway problem cannot arise; a sexagesimal `1:30`
  is a string; timestamps are strings.
- quoted scalars (single, double) and block scalars (`|`, `>`, with
  their chomping and indentation indicators) are strings.
- anchors and aliases are resolved: an alias is a copy of the anchored
  value, so `&a`/`*a` round-trips to two equal values. Merge keys (`<<`)
  are YAML 1.1 and are not read: `<<` is an ordinary key.
- tags are not read. The core schema's implicit typing is all there is;
  an explicit tag — `!!str`, `!!binary`, `!custom`, `!!timestamp` — is
  an error.
- a key must be a string (the JSON model has no other key), and a key
  that repeats in one mapping is an error (§10.2's rule for JSON).
- a stream is one document. A second document (`---`) is an error; a
  leading `---` and a trailing `...` are allowed; a `%YAML` directive is
  allowed only for 1.2.
- a comment (`#`) is ignored; the byte order mark is ignored; the file
  is UTF-8.

Every error above is **E6004** — the code the command line already
reports for a document that cannot be read or is not well-formed —
with a message naming the construct (`document uses a tag`, `mapping
key is not a string`, `stream holds more than one document`, `not
well-formed YAML: <reason> at line L`). The specification's scope rule
stands: interchange is normatively JSON; the YAML reader is a tool-side
conversion whose result is a JSON document, defined here.

## 3. Structured output — `--format`, `--indent`, `--pretty`

`--format` selects the text form of every document `evaluate` emits in
one invocation — the one object keyed by root name when there is no
`--output` (§5.5), or each root's own document when there is — and
`--indent` / `--pretty` select its layout. A file named by `--output
name=file` takes the same format and layout; its extension does not
change them (a `.yaml` file written with `--format json` holds JSON).
The `--json` report is JSON regardless, and `--json` with `--format
yaml` is a usage error (exit 2).

| Option | Meaning | Default |
|---|---|---|
| `--format json` | JSON, the canonical text of §10.4 | yes |
| `--format yaml` | YAML 1.2, block style (§3.2) | |
| `--indent n` | JSON: `0` is the canonical one-line form; `n ≥ 1` lays the document out with `n` spaces per level. YAML: the indentation step, `n ≥ 1` | JSON `0`, YAML `2` |
| `--pretty` | JSON: `--indent 2`. YAML: no effect | |

`--indent` outside `0..16`, or `--indent 0` with YAML, is a usage
error. A document always ends with exactly one newline. Whatever the
layout, a reader of the format reads back **exactly the canonical
JSON document**: order (§7.2), number text (D29), string escapes, and
the `{ "value", "unit" }` form of a quantity are the same in every
layout; only whitespace differs between JSON layouts, and the YAML
form is defined member by member below.

### 3.1 Indented JSON

The canonical text with whitespace: after `[` and `{` a newline and one
more level of indentation, `,` followed by a newline at the same level,
`:` followed by one space, the closing bracket on its own line at the
enclosing level. An empty array is `[]` and an empty object `{}` on
one line. Strings, numbers, and escapes are untouched — this is
`JSON.stringify(value, null, n)` and Python's `json.dumps(indent=n)`
and `serde_json`'s pretty printer, which agree on this layout.

### 3.2 YAML

Block style throughout, the mapping keys in canonical member order
(§7.2), sequences as `- ` items, the indentation step from `--indent`
(default 2), a sequence nested in a mapping indented under its key.

| Value | YAML text |
|---|---|
| `null` | `null` |
| `true`, `false` | `true`, `false` |
| integer | its decimal digits |
| float | the shortest round-trip form (D29), with `.0` where the JSON carries one; `1e+21` stays `1e+21` |
| string | plain when it is *safe* (below), otherwise double-quoted with JSON's escapes |
| empty array, empty object | `[]`, `{}` |
| array of scalars or containers | a block sequence, one `- ` per item; an item that is a mapping starts on the `- ` line |
| object | a block mapping, `key: value`, the key on the same rule as strings |
| quantity | the mapping `value: …` / `unit: …` (the interchange form, §3.16) |
| reference | its canonical path as a string (`$.services[1]`, quoted by the string rule since it starts with `$`) |

A string is **plain** only when the core schema would read it back as
exactly that string: it is non-empty; it does not resolve to a bool,
null, integer, or float under the core schema (so `true`, `null`, `~`,
`12`, `1e3`, `0x10`, `.inf`, `.5`, `+1` are quoted); it starts with a
letter or `_`; it contains none of `: ` / ` #` / `#` at the start /
the indicators `- ? : , [ ] { } & * ! | > ' " % @ \``; it has no
leading or trailing whitespace, no newline, no tab, and no character
outside the printable range; and it is not `---` or `...`. Everything
else is double-quoted, escaped as in JSON (`\n`, `\t`, `\"`, `\\`,
`\uXXXX` for control characters). Keys follow the same rule. Never an
anchor, an alias, a tag, a flow collection except the two empties, a
block scalar, or a folded line: a YAML 1.2 reader with the core schema
reads the text back to the canonical JSON document, and a YAML 1.1
reader is given nothing bare that it could reinterpret.

The document written by `--format yaml` starts at column 0 with no
`---` and ends with one newline.

## 4. Templates — `decl render`

`decl render` evaluates a universe exactly as `evaluate` does — the
same loading, binding, checking, evaluation, and diagnostics — and then
renders each root it emits through a template into text, the text
going where the document would have gone: to the file of `--output
name=file`, or to standard output for at most one root without a
file. With no `--output`, every exported output of the entry module is
rendered, in declaration order, and the texts are written to standard
output one after another with nothing between them.

Templates are named per root: `--template root=path` renders that
root; `--template path` (no root) is the template for every root that
has no template of its own; `--template root=-` or `--template -`
reads the template from standard input, once. A root that is rendered
without any template is a usage error (exit 2), as is a root named by
`--template root=` that the invocation does not render, or the same
root named twice.

```bash
decl render site.decl --template site.conf.j2                         # every exported output through one template, to stdout
decl render site.decl --template site=site.conf.j2 --output site=out/site.conf
decl render site.decl --template site=site.conf.j2 --template report=report.md.j2 \
    --output site=out/site.conf --output report=out/report.md
decl render cfg.decl --input deployed=doc.yaml --template deployed=nginx.j2 --output deployed
```

A root that fails validation is not rendered: its diagnostics are
reported as `evaluate` reports them, the exit code is 1, and no text
is written for it (a file is not created); the other roots are still
rendered. Rendering errors (§4.7) are diagnostics too, on standard
error, exit code 1, the partial text discarded.

### 4.1 The dialect, and where it comes from

The template dialect is Decl's own — a small, fixed language
implemented three times and never delegated to a host engine, because
Jinja2, Nunjucks, minijinja, and Tera differ in whitespace control,
filters, and number printing, and the three implementations must print
identical bytes. Its surface is the Jinja family's, which template
authors already know: `{{ }}` for values, `{% %}` for statements, `{#
#}` for comments, `-` for whitespace control, `if` / `for` / `set` /
`raw`, a `loop` object. What is deliberately different:

- **Expressions are Decl expressions** (§4), evaluated by the language's
  own evaluator over the document — not a template expression
  language. There are no template filters: a Jinja filter is a
  function call, and Jinja's `x | f` is the language's pipeline
  `x |> f` (§4.9, first-argument insertion), so `{{ services |>
  std.array.count }}` and `{{ name |> std.string.length }}` read as a
  Jinja author expects and mean what the language means.
- **Nothing is coerced.** A condition must be a `bool`; a value with no
  text form does not render; an absent member is an error unless the
  expression gives it a default (`??`, §4.10). Jinja's truthiness and
  its silent `Undefined` are the class of defect the language exists to
  remove.
- **One file, one document.** No `include`, `extends`, `block`,
  `macro`, `import`, or `call` in this delivery: a template is one file
  over one document, and a document is one root. The language's own
  `func` declarations, in scope in the template, are the abstraction
  mechanism.
- **Whitespace is predictable** by two rules stated in §4.2, with
  Jinja's `-` and `+` to override them.

The engines consulted: Jinja2 and Nunjucks (the syntax, `loop`,
whitespace control, `raw`), minijinja and Tera (the Rust ports, and
what they cut), Liquid (the case for a small statement set), Mustache
and Handlebars (logic-less templates, rejected: a document needs
conditions and loops over its own structure), Go's `text/template`
(the case against inventing an expression language), and Sailfish
(compile-time templates, inapplicable to a user's file).

### 4.2 Lexical structure

A template is a UTF-8 text file. Its line endings are kept as written
in the text between tags; the rendered output uses the same line
ending the template used at that place.

| Delimiters | Meaning |
|---|---|
| `{{ expr }}` | the text form of `expr` (§4.5) |
| `{% stmt %}` | a statement (§4.3) |
| `{# … #}` | a comment; produces nothing, may span lines, does not nest |
| `{% raw %} … {% endraw %}` | the text between, verbatim, delimiters included; the only way to write `{{` or `{%` literally |

Whitespace around statements follows two default rules, which are
Jinja's `trim_blocks` and `lstrip_blocks` switched on:

1. the newline that immediately follows a statement tag `%}` is
   removed, so a statement on a line of its own leaves no blank line;
2. whitespace between a line start and a statement tag `{%` on that
   line is removed, so an indented statement leaves no indentation.

Neither rule touches `{{ }}` or `{# #}`. The Jinja modifiers override
both: `{%-` strips all whitespace before the tag (newlines included),
`-%}` all whitespace after it; `{%+` and `+%}` keep the whitespace the
default rules would remove. `{{-` and `-}}` strip around a value tag
the same way. Inside `{% raw %}` nothing is trimmed.

### 4.3 Statements

| Statement | Meaning |
|---|---|
| `{% if e %} … {% elif e %} … {% else %} … {% endif %}` | `e` must evaluate to `bool` (E4001, as the language reports a non-`bool` condition); branches render in order; `elif` repeats |
| `{% for x in e %} … {% else %} … {% endfor %}` | `e` an array: the body once per element with `x` bound; the `else` body when the array is empty |
| `{% for x in e if c %}` | only the elements for which `c` (a `bool` over `x`) holds, as in a comprehension filter (§4.8); `loop` counts the kept ones |
| `{% for k, v in e %}` | `e` an object or map: the body once per member, `k` the key (a string), `v` the value, in canonical order (§7.2) |
| `{% set x = e %}` | binds `x` to `e` for the rest of the enclosing body (the template, or the `for`/`if` body it appears in); a name may be set once per scope |
| `{% raw %} … {% endraw %}` | verbatim text |

Inside a `for` body, `loop` is bound to a record with exactly five
members: `loop.index` (from 1), `loop.index0` (from 0), `loop.first`,
`loop.last`, `loop.length` (the count of iterated elements). A nested
`for` binds its own `loop`; the enclosing one is not reachable (no
`parentloop`, no `revindex`, no `cycle`). `loop` cannot be assigned.

Statements nest freely; every `if` and `for` is closed by its `endif`
/ `endfor`, and a tag out of place is E7001.

### 4.4 Names and scope

A template evaluates over the **context of the root** it renders:

- the root's name is bound to the root's completed document (`site`,
  `deployed`) — this is the only name when the document is an array or
  a scalar;
- when the document is a record, each of its members is also bound by
  name (`services`, `port`), so `{{ services[0].port }}` and
  `{{ site.services[0].port }}` are the same value;
- the entry module's `const` and `func` declarations are in scope, as
  are `std` and the `render` namespace (§4.6);
- `for` variables, `set` names, and `loop` are bound in their body.

The language's rule that a name is declared once applies (E3019 for a
`for` variable or a `set` that repeats a name in scope, including a
member's name); `loop` is the one name a nested `for` rebinds. The
context variables `$this`, `$parent`, `$root`, `$key`, `$path`, and
`$referrers` are not available in a template (a template is not a
member expression); the document is reached by its members and its
name.

### 4.5 The text form of `{{ expr }}`

`expr` is evaluated as the language evaluates an expression (§9), and
its value is written as text:

| Value | Text |
|---|---|
| `string` | the string itself, no escaping |
| `int` | decimal digits |
| `float` | the shortest round-trip form (D29): `0.25`, `1e+21`, `12000.0` |
| `bool` | `true` / `false` |
| `null` | `null` |
| quantity | the base-unit magnitude then a space then the unit symbol: `3000.0 m`, `0.25 s` (the magnitude as a float) |
| reference | its canonical path (`$.services[1]`) |
| array, object, map | its canonical JSON on one line (§10.4) |
| absent | E7002 — write `{{ x ?? "…" }}` to give it a text |
| a function | E7002 |

The first four are §4.11's conversions, so `{{ x }}` and the module's
own `` `${x}` `` agree; the rest are the renderer's, defined here
because a template, unlike a string template, is where a whole value
is often wanted.

### 4.6 The `render` namespace

Three functions the renderer provides, callable directly or through
the pipeline:

| Function | Meaning |
|---|---|
| `render.json(v)`, `render.json(v, indent: int)` | `v` as JSON: canonical on one line, or laid out with `indent` spaces per level (§3.1) |
| `render.yaml(v)`, `render.yaml(v, indent: int)` | `v` as YAML (§3.2); the text has no trailing newline, so it composes inline |
| `render.indent(s: string, n: int)` | `s` with `n` spaces inserted after every newline (not before the first line): the way to nest a rendered block under a key in YAML or under an indented line |

`{{ site |> render.yaml |> render.indent(2) }}` places a document under
a parent key. There are no others; string work is `std.string`, and a
template that needs more expresses it as a `func` in the module.

### 4.7 Errors

| Code | Condition |
|---|---|
| E7001 | the template does not parse: an unclosed or unknown tag, a tag out of place, an expression that is not a Decl expression; the message names the construct |
| E7002 | a value with no text form in `{{ }}`: absent, or a function |
| E7003 | a template file cannot be read (also for `-` when standard input is not available) |

A rendering diagnostic's `file` is the template's path as given (`-`
for standard input), its `path` is `L:C`, the line and column of the
tag it arose in, and it has no `id`. An expression that fails to check
or to evaluate is reported with the language's own code (E3003 for an
unknown name, E4001 for a non-`bool` condition, E5001 for a division by
zero, …), anchored the same way. Diagnostics of the evaluation itself
(binding, assertions) keep their document paths, as `evaluate` prints
them. The report is ordered as §12.3 orders it, the template's
diagnostics after the document's.

## 5. The library

The three APIs grow in the `evaluate` vocabulary
([JavaScript / npm](../../decl-ts/README.md), [Python](../../decl-py/README.md),
[Rust](../../decl-rs/README.md)):

- `render(path, { templates, inputs?, outputs? })` → `{ [root]: text }`.
  `templates` is a map from root name to a template — a file path, or
  the template text itself — with the key `"*"` for the default
  template; the rest as `evaluate`. A failure throws / raises / returns
  the same error type with the diagnostics of §4.7.
- `toJson(value, indent?)` and `toYaml(value, indent?)` — the text of a
  JSON value in the layouts of §3, pure functions with no universe
  behind them, for a program that has a document and wants its text.
- `evaluate` and `validate` accept a document path ending in `.yaml` /
  `.yml` and read it by §2; a document given as a value is unchanged.

The internal modules gain one file each, in the three implementations
under the same name (AGENTS.md): `yaml` (the reader and the writer of
§2–3) and `render` (the template parser and renderer of §4).

## 6. The REPL and the editors

The REPL ([02. REPL](02_repl.md)) binds `:bind name=doc.yaml` by §2 and
gains two things: `:evaluate` accepts `--format yaml` and `--indent n`
before the roots, printing each root's document in that form, and
`:render <template> [root…]` renders the named roots (every exported
output when none) through the template file, printing the text. The
`:render` command follows the session's documents and edits like
`:evaluate` does. `:help` lists them.

The VS Code extension's output preview ([04. Extension](04_extension.md))
takes two settings, `decl.preview.format` (`json` | `yaml`, default
`json`) and `decl.preview.template` (a path; empty for none): with a
template set the preview shows the rendered text of the previewed root.
The setting is read by the extension; the server is unchanged. Zed and
the other configurations have no preview and change nothing.

## 7. The corpus

`tests/render/` holds the shared data, one driver per implementation
(`decl-ts/tests/render_test.ts`, `decl-rs/tests/render_test.rs`,
`decl-py/tests/render_test.py`), and the harness replays it:

- `cases.json` — one entry per rendering case: the module, its
  documents (`inputs`), the templates by root (`templates`, with `"*"`
  for the default, paths under `templates/`), the roots rendered
  (`outputs`), and for each the expected text under `expected/`, or
  `rejected` with the expected standard error. Every statement of
  §4.3, every text form of §4.5, every function of §4.6, every error of
  §4.7, and the whitespace rules of §4.2 have a case.
- `formats.json` — pairs of a golden document (`tests/golden/`) and its
  YAML form under `yaml/`, plus its indented JSON forms for the indents
  the entry names; the harness runs `evaluate --format yaml` /
  `--indent n` and diffs.
- `inputs/` — the YAML twin of every document the golden corpus binds
  (`tests/golden/inputs/`): the harness binds the twin and expects the
  JSON golden, which proves the reader; `invalid/` holds documents the
  reader must reject, each with its E6004 message.
- the parity harness gains sections `yaml-input`, `format`, and
  `render` (byte for byte, exit code, both streams), and the REPL corpus
  gains a session exercising `:render` and `:evaluate --format`.

The expected texts are produced once by the reference and reviewed,
like every golden (tests/golden/README.md).

## 8. What the specification changes

The language does not change. Two texts of the specification do, as
one revision (v0.3.1, REVISIONS.md):

- §10.6 gains an editorial pointer: interchange stays JSON; documents
  in YAML and documents written as YAML are tool-side conversions
  defined by this document.
- §12.1 names the band **E7xxx — rendering** and §12.4 registers
  E7001–E7003 (append-only); the E6004 condition is reworded to "a
  document that cannot be read, or is not well-formed JSON or YAML, or
  uses a YAML construct outside the core schema".
- The charter records the decision (D35): rendering is tooling; formats
  and the template dialect are tool-side, fixed by this document and
  held identical across the three implementations by the corpus.

## 9. Decisions and open questions

Decided for this phase:

- **TOML is not in this delivery.** TOML has no null and its root must
  be a table, so a loss rule is needed (an error naming the path, or
  omission), and a writer would be hand-built three times; YAML serves
  the need for a second structured form and templates serve the rest.
  If it comes, it comes fail-closed: an error naming the first path
  that cannot be represented.
- The `loop` object has exactly five members.
- A template may come from standard input, as `-`.
- `set` and `raw` are in; `include` / `extends` / `macro` are not.
- The default whitespace rules are Jinja's trim/lstrip switched on.

Open: whether `render.indent` should also take a first-line prefix;
whether a `--newline lf|crlf` option is wanted for templates on
Windows (the template's own endings are kept for now).

## 10. Status

Planned (Phase 10). This document is the specification the
implementation follows; its Status becomes *Delivered* when the corpus
of §7 passes in the three implementations and the harness.

## 11. Verification

- Every case of `tests/render/` identical across the three
  implementations in the harness — exit code, standard output, standard
  error.
- Every construct of §4.2–4.3, every text form of §4.5, every function
  of §4.6, and every error of §4.7 with a case.
- Every golden's YAML form read back by a YAML 1.2 reader (the harness
  uses PyYAML in 1.2 core-schema mode and `serde_yaml`'s reader through
  the Rust suite) equal to the golden; every golden's indented JSON
  parsed back equal to the golden.
- Every YAML twin under `tests/render/inputs/` binding to the JSON
  golden; every document under `invalid/` rejected with its message.
- The REPL corpus session and the extension's preview settings
  exercised.
