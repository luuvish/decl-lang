# The renderer — `@render`, documents in YAML, and `--format`

Evaluation ends at a resolved value tree and its diagnostics; the
specification defines interchange with JSON alone (§10.6) and the
requirements place rendering in the tool chain, not the language core
(01_requirements §2). Rendering is therefore **tooling**, and this
document is its specification: how a module *declares* the form each
of its outputs is emitted in — canonical or indented JSON, YAML, or
text through a template, one file or one file per element — how the
`decl` command line, the REPL, and the library honor that declaration
and let an invocation override it, how a document written in YAML is
read, and how the three implementations do all of it identically, byte
for byte, held together by a corpus the parity harness replays. This
document is informative in the specification's sense (§1.4); the
language does not change, and what it borrows from the specification
it cites.

```
decl evaluate <file> [--input name=doc.(json|yaml)]... [--output name[=file|dir|-]]...
                     [--format json|yaml] [--indent n | --pretty] [--template [root=]path]... [--json]
decl validate <file> [--input name=doc.(json|yaml)]... [--expect-errors E1,E2] [--json]
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

The vocabulary:

- a **document** on the way in may be JSON or YAML (§2);
- a **form** is how a root is emitted: a *format* (JSON, canonical or
  indented, or YAML), or a *template* into text, for the root as one
  file or *each* of its elements as a file (§3);
- the module **declares** a root's form with `@render` (§3); an
  invocation **overrides** it with options (§3.4).

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
  hexadecimal and octal integers (`0x1F`, `0o17`, exact at any size),
  floats with the core schema's forms — `.inf` and `.nan` are refused,
  since the language has no such value (§9.5, D18) — and everything
  else a string. **YAML 1.1 is not read**: `yes`, `no`, `on`, `off`,
  `y`, `n` are strings, so the Norway problem cannot arise; a
  sexagesimal `1:30` is a string; timestamps are strings; `1_000` is a
  string.
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
with one message form, `bound document is not well-formed YAML:
<file>: <reason> at line L`, the reason naming the construct: `uses a
tag`, `mapping key is not a string`, `mapping repeats the key "k"`,
`stream holds more than one document`, `unsupported YAML version`,
`non-finite float`, `unknown alias *a`, and for the text itself `bad
indentation`, `tab in indentation`, `unterminated quoted scalar`,
`unterminated flow collection`, `missing ':' after a mapping key`,
`unexpected mapping value` (`a: b: c`), `unexpected sequence` (`a: -
b`), `bad escape`, `reserved indicator @`, `unexpected content`, and
`unexpected content after …`. `tests/render/invalid/` holds one
document per reason. The specification's scope rule stands:
interchange is normatively JSON; the YAML reader is a tool-side
conversion whose result is a JSON document, defined here.

## 3. `@render` — the declared form of an output

An `output` declaration may carry one `@render` annotation whose single
argument is an object literal of literal values. Annotations are
metadata (§5.10, D4): `@render` does not touch typing, evaluation, or
serialization; the tools read it when they emit the root.

```decl
@render({ format: "yaml", indent: 2 })
export output site: Site = { … }

@render({ template: "templates/nginx.conf.j2", file: "out/nginx.conf" })
export output gateway: Gateway = { … }

@render({ each: "path", template: "templates/unit.j2" })
export output units: Unit[] = [ { name: "auth", path: "units/auth.service", … } for … ]

@render({ each: "$key", format: "yaml" })
export output manifests: { [string]: Manifest } = { [`k8s/${s.name}.yaml`]: manifest(s) for s in services }

export output report: Report = { … }     // no annotation: canonical JSON
```

| Key | Value | Meaning |
|---|---|---|
| `format` | `"json"` (default) or `"yaml"` | the structured text of §4 |
| `indent` | an integer, `0..16` | the layout of §4: `0` canonical JSON; `n ≥ 1` indented JSON, or YAML's step (default 2) |
| `template` | a string | a template file (§5), relative to the module's directory; the root is emitted as text. `format` and `indent` then apply only inside `render.json` / `render.yaml` |
| `file` | a string | the default destination, relative to the working directory: where `--output name` writes when no `=file` is given |
| `each` | a string | fan-out (§6): the root is an array or a map, and every element is emitted as its own file — the string names the member of each element that holds the file's path, or is `"$key"` for a map's key |
| `delimiters` | an object | the template's delimiters, when the defaults `{= =}` / `{% %}` / `{# #}` collide with the text generated (§5.2) |

A `@render` that is not one object literal of literals, with a key
outside this table, a value of the wrong type, `indent` out of range,
`format` other than the two, `delimiters` with an empty string or two
equal openers, or `each` on a root whose type is neither an array nor a
map, is **E7004** at emission with a message naming the key; the root
is not emitted. `@render` on a declaration other than an
`output` is the ordinary unknown-annotation warning of §5.10 and is
ignored. A root without `@render` is emitted as canonical JSON, one
file, as today.

### 3.1 When the declared form applies

A root's form applies whenever the root is **emitted as a document on
its own**: by `--output name[=…]`, by the REPL's `:evaluate name`, by
the library's `render`, and by the editor's preview. It does not apply
to the aggregate: `decl evaluate <file>` with no `--output` still
prints the one object of every exported output keyed by name, in
canonical JSON (§5.5), so that a caller reading "all the outputs" gets
one JSON document as before; `--format` and `--indent` lay that object
out (§4) but no template applies to it.

### 3.2 Destinations

`--output name` alone writes the root to its declared `file` when it
has one, otherwise to standard output; `--output name=file` writes to
that file; `--output name=-` writes to standard output even when a
`file` is declared. At most one root goes to standard output. For a
fan-out root (§6), the destination is a **directory**: `--output
name=dir`, or the declared `file` read as a directory, and `-` is not
accepted. Directories on the way to a file are created; a file that
cannot be written is reported by name (exit 1), as `evaluate` reports
it today.

### 3.3 Templates and documents are read once

A template named by `@render` or `--template` is read when the root is
emitted, relative to the module's directory (declared) or the working
directory (`--template`); a template that cannot be read is E7003. The
same template file serving several roots is parsed once.

### 3.4 Overrides — the options

The options change the form for one invocation and say nothing the
module could not have said; they are for scripts and for looking:

| Option | Overrides |
|---|---|
| `--format json\|yaml` | the `format` of every root emitted, and of the aggregate |
| `--indent n`, `--pretty` (= `--indent 2` for JSON) | the `indent` likewise |
| `--template [root=]path` | the `template` of that root, or of every root without a `--template root=` of its own; `-` reads the template from standard input, once |
| `--output name=file\|dir\|-` | the destination (§3.2) |

`--json` with `--format yaml` is a usage error, and `--indent` with
`--pretty` too; `--json` reports are JSON always, and their `value` is
the document itself in canonical JSON whatever the declared or
requested layout — a template root's `value` is its text as a JSON
string. A `--template root=` naming a root that is not emitted, or the
same root twice, is a usage error (exit 2). The options do not switch
`each` on or off: fan-out is a property of the root's shape and its
declaration.

## 4. Structured text — formats and layouts

The **format** is the text form of one document; the **layout** is
`indent`. A document always ends with exactly one newline. Whatever the
layout, a reader of the format reads back **exactly the canonical JSON
document**: order (§7.2), number text (D29), string escapes, and the
`{ "value", "unit" }` form of a quantity are the same in every layout;
only whitespace differs between JSON layouts, and the YAML form is
defined member by member below.

### 4.1 JSON

`indent: 0` is the canonical text of §10.4, on one line. `indent: n`
for `n ≥ 1` is the canonical text with whitespace: after `[` and `{` a
newline and one more level of `n` spaces, `,` followed by a newline at
the same level, `:` followed by one space, the closing bracket on its
own line at the enclosing level; an empty array is `[]` and an empty
object `{}` on one line. Strings, numbers, and escapes are untouched
— this is `JSON.stringify(value, null, n)`, Python's
`json.dumps(indent=n)`, and `serde_json`'s pretty printer, which agree
on this layout.

### 4.2 YAML

YAML 1.2, block style throughout, the mapping keys in canonical member
order (§7.2), sequences as `- ` items, the indentation step from
`indent` (default 2), a sequence nested in a mapping indented under its
key. The document starts at column 0 with no `---`.

| Value | YAML text |
|---|---|
| `null` | `null` |
| `true`, `false` | `true`, `false` |
| integer | its decimal digits |
| float | the shortest round-trip form (D29), with `.0` where the JSON carries one; `1e+21` stays `1e+21` |
| string | plain when it is *safe* (below), otherwise double-quoted with JSON's escapes |
| empty array, empty object | `[]`, `{}` |
| array | a block sequence, one `- ` per item; an item that is a mapping starts on the `- ` line |
| object, map | a block mapping, `key: value`, the key on the same rule as strings |
| quantity | the mapping `value: …` / `unit: …` (the interchange form, §3.16) |
| reference | its canonical path as a string (`$.services[1]`, quoted by the string rule since it starts with `$`) |

A string is **plain** only when a YAML 1.2 reader with the core schema
reads it back as exactly that string and a YAML 1.1 reader has nothing
to reinterpret: it starts with an ASCII letter or `_` (so every string
that looks like a number, a date, or a path, and every empty or
space-led string, is quoted); it is not a word either schema reads as
a bool or a null (`true`, `null`, and the 1.1 words `yes`, `no`, `on`,
`off`, `y`, `n` in their spellings); it contains none of `: ` and
` #`, none of `[ ] { } , & * ! | > ' " % @ \` #`, no tab, line break,
control character, or other unprintable; and it ends in neither `:`
nor a space. Everything else is double-quoted, escaped as in JSON
(`\n`, `\t`, `\"`, `\\`, `\uXXXX` for control characters). Keys follow
the same rule. Never an anchor, an alias, a tag, a flow collection
except the two empties, a block scalar, or a folded line: a YAML 1.2
reader with the core schema reads the text back to the canonical JSON
document, and a YAML 1.1 reader is given nothing bare that it could
reinterpret. So `my-service`, `with space`, and `a_b` are plain;
`2024-01-01`, `10.0.0.0/8`, `yes`, `12`, `a: b`, and `-x` are quoted.

## 5. Templates

A root with a `template` is emitted as the text the template produces
over the root's document. A root that fails validation is not
rendered: as `evaluate` has it, an error-severity diagnostic of the
run means no document is emitted and exit 1. Rendering errors (§5.8)
are diagnostics too, on standard error, exit code 1, that root's text
discarded; the other roots are still emitted.

### 5.1 The dialect, and where it comes from

The template dialect is Decl's own — a small, fixed language
implemented three times and never delegated to a host engine, because
Jinja2, Nunjucks, minijinja, and Tera differ in whitespace control,
filters, and number printing, and the three implementations must print
identical bytes. Its surface is the Jinja family's, which template
authors already know: `{% %}` for statements, `{# #}` for comments,
`-` for whitespace control, `if` / `for` / `set` / `include` / `raw`, a
`loop` object — and `{= =}` for values, where the family writes
`{{ }}` (§5.2 says why). What is deliberately different:

- **Expressions are Decl expressions** (§4), evaluated by the language's
  own evaluator over the document — not a template expression
  language. There are no template filters: a Jinja filter is a
  function call, and Jinja's `x | f` is the language's pipeline
  `x |> f` (§4.9, first-argument insertion), so `{= services |>
  std.array.count =}` and `{= name |> std.string.length =}` read as a
  Jinja author expects and mean what the language means.
- **Nothing is coerced.** A condition must be a `bool`; a value with no
  text form does not render; an absent member is an error unless the
  expression gives it a default (`??`, §4.10). Jinja's truthiness and
  its silent `Undefined` are the class of defect the language exists to
  remove.
- **Files compose by inclusion, not inheritance.** `include` is in:
  a header, a footer, or a fragment rendered in the current scope, and
  with `set` before it, a parameterized fragment. `extends` / `block`
  and `macro` are not: template inheritance is the largest and least
  used part of the Jinja family outside HTML pages, and a Decl module's
  `func` declarations, in scope in the template, already carry the
  logic a macro would. Fan-out (§6) covers "one template, many files".
- **Whitespace is predictable** by two rules stated in §5.2, with
  Jinja's `-` and `+` to override them.

The engines consulted: Jinja2 and Nunjucks (the syntax, `loop`,
whitespace control, `raw`), minijinja and Tera (the Rust ports, and
what they cut), Liquid (the case for a small statement set), Mustache
and Handlebars (logic-less templates, rejected: a document needs
conditions and loops over its own structure), Go's `text/template`
(the case against inventing an expression language), Sailfish
(compile-time templates, inapplicable to a user's file), and Pkl's
`output` block (the case for declaring the form in the module, which
`@render` takes up).

### 5.2 Lexical structure

A template is a UTF-8 text file. Its line endings are kept as written
in the text between tags; the rendered output uses the same line
ending the template used at that place.

| Delimiters | Meaning |
|---|---|
| `{= expr =}` | the text form of `expr` (§5.5) |
| `{% stmt %}` | a statement (§5.3) |
| `{# … #}` | a comment; produces nothing, may span lines, does not nest |
| `{% raw %} … {% endraw %}` | the text between, verbatim, delimiters included; the way to write `{=` or `{%` literally |

The value delimiter is `{= =}`, not the Jinja family's `{{ }}`: the
languages a Decl module most often generates — Verilog and
SystemVerilog, with their concatenation `{a, b}` and replication
`{n{x}}` — are full of `{{` and `}}`, and a template for them must be
able to write those as ordinary text. Everything that is not a
delimiter is text, `{{` and `}}` included.

The delimiters can be declared per root, for a template that generates
another template language or a text where `{%` or `{#` occur:

```decl
@render({ template: "chart.yaml.j2", delimiters: { value: ["<%=", "%>"], statement: ["<%", "%>"], comment: ["<%#", "%>"] } })
```

Each of the three is a pair of non-empty strings; the three openers
are distinct and are matched longest first at every position, so an
opener may extend another (`<%=` beside `<%`). The whitespace
modifiers of this section attach inside whichever delimiters are in
force (`<%-`, `-%>`). The declared delimiters apply to the root's
template and to every file it includes (§5.3).

Whitespace around statements follows two default rules, which are
Jinja's `trim_blocks` and `lstrip_blocks` switched on:

1. the newline that immediately follows a statement tag `%}` is
   removed, so a statement on a line of its own leaves no blank line;
2. whitespace between a line start and a statement tag `{%` on that
   line is removed, so an indented statement leaves no indentation.

Neither rule touches `{= =}` or `{# #}`. The Jinja modifiers override
both: `{%-` strips all whitespace before the tag (newlines included),
`-%}` all whitespace after it; `{%+` and `+%}` keep the whitespace the
default rules would remove. `{=-` and `-=}` strip around a value tag
the same way. Inside `{% raw %}` nothing is trimmed.

### 5.3 Statements

| Statement | Meaning |
|---|---|
| `{% if e %} … {% elif e %} … {% else %} … {% endif %}` | `e` must evaluate to `bool` (E4001, as the language reports a non-`bool` condition); branches render in order; `elif` repeats |
| `{% for x in e %} … {% else %} … {% endfor %}` | `e` an array: the body once per element with `x` bound; the `else` body when the array is empty |
| `{% for x in e if c %}` | only the elements for which `c` (a `bool` over `x`) holds, as in a comprehension filter (§4.8); `loop` counts the kept ones |
| `{% for k, v in e %}` | `e` an object or map: the body once per member, `k` the key (a string), `v` the value, in canonical order (§7.2) |
| `{% set x = e %}` | binds `x` to `e` for the rest of the enclosing body (the template, or the `for`/`if` body it appears in); a name may be set once per scope |
| `{% include "path" %}` | renders the named template file in place, with the current scope (names in scope are visible to it; a `set` inside it does not escape); the path is relative to the including file's directory; a cycle of includes is E7001 |
| `{% raw %} … {% endraw %}` | verbatim text |

Inside a `for` body, `loop` is bound to a record with exactly five
members: `loop.index` (from 1), `loop.index0` (from 0), `loop.first`,
`loop.last`, `loop.length` (the count of iterated elements). A nested
`for` binds its own `loop`; the enclosing one is not reachable (no
`parentloop`, no `revindex`, no `cycle`). `loop` cannot be assigned.

Statements nest freely; every `if` and `for` is closed by its `endif`
/ `endfor`, and a tag out of place is E7001.

### 5.4 Names and scope

A template evaluates over the **context** of what it renders:

- for a root emitted as one file, the root's name is bound to the
  root's completed document (`site`, `gateway`) — the only name when
  the document is an array or a scalar — and, when the document is a
  record, each of its members is bound by name as well, so
  `{= services[0].port =}` and `{= site.services[0].port =}` are the
  same value;
- for one element of a fan-out root (§6), `item` is bound to the
  element, `key` to its map key (a string) or array index (an integer),
  the element's members by name when it is a record, and the root's
  name to the whole root;
- the entry module's `const` and `func` declarations are in scope, as
  are `std` and the `render` namespace (§5.6);
- `for` variables, `set` names, and `loop` are bound in their body.

The language's rule that a name is declared once applies (E3019 for a
`for` variable or a `set` that repeats a name in scope, including a
member's name); `loop` is the one name a nested `for` rebinds. The
context variables `$this`, `$parent`, `$root`, `$key`, `$path`, and
`$referrers` are not available in a template (a template is not a
member expression); the document is reached by its members and its
names.

### 5.5 The text form of `{= expr =}`

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
| absent | E7002 — write `{= x ?? "…" =}` to give it a text |
| a function | E7002 |

The first four are §4.11's conversions, so `{= x =}` and the module's
own `` `${x}` `` agree; the rest are the renderer's, defined here
because a template, unlike a string template, is where a whole value
is often wanted.

### 5.6 The `render` namespace

Three functions the renderer provides, callable directly or through
the pipeline:

| Function | Meaning |
|---|---|
| `render.json(v)`, `render.json(v, indent: int)` | `v` as JSON: canonical on one line, or laid out with `indent` spaces per level (§4.1) |
| `render.yaml(v)`, `render.yaml(v, indent: int)` | `v` as YAML (§4.2); the text has no trailing newline, so it composes inline |
| `render.indent(s: string, n: int)` | `s` with `n` spaces inserted after every newline (not before the first line): the way to nest a rendered block under a key in YAML or under an indented line |

`{= site |> render.yaml |> render.indent(2) =}` places a document under
a parent key. There are no others; string work is `std.string`, and a
template that needs more expresses it as a `func` in the module.

### 5.7 Functions in a template

A template defines no functions of its own: the module's `func` and
`const` declarations are in scope, and that is where computation
belongs, so that `decl evaluate --output gateway` shows every value a
template will place. Three idioms cover what a Jinja author reaches
for `macro` and filters to do.

A **value** is computed by a `func`, called directly or through the
pipeline, with lambdas where a `std` function takes one:

```decl
// site.decl
func label(s: Service): string = `${s.name}:${s.port}`
func cidr(net: Subnet): string = `${net.base}/${net.prefix}`
func ports_csv(ss: Service[]): string =
    std.string.join([std.string.of(s.port) for s in ss], ",")

@render({ template: "nginx.conf.j2" })
export output gateway: Gateway = { … }
```

```
{# nginx.conf.j2 #}
upstream backend {
{% for s in services if s.public %}
    server {= label(s) =} weight={= s.replicas =};
{% endfor %}
}
allow {= network |> cidr =};
# ports: {= services |> std.array.filter((s) => s.public) |> ports_csv =}
```

A **temporary** is bound with `set`, once per scope:

```
{% set public = services |> std.array.filter((s) => s.public) %}
{% set n = public |> std.array.count %}
{= n =} public services
```

A **fragment of text with statements in it** — what a `func` cannot
return — is a file included with its parameters set beforehand; the
included file sees the scope, and what it sets stays inside it:

```
{% for s in services %}
{% set svc = s %}
{% include "server-block.j2" %}
{% endfor %}
```

```
{# server-block.j2: svc is in scope #}
    server {
        name {= svc.name =};
        listen {= svc.port =};
    }
```

The limits follow from the language's: a `func` does not recurse (§5.3)
and holds no statements, so a deep tree is flattened into a value by a
comprehension in the module and then walked by `for` in the template.

### 5.8 Errors

| Code | Condition |
|---|---|
| E7001 | the template does not parse: an unclosed or unknown tag, a tag out of place, an expression that is not a Decl expression; the message names the construct |
| E7002 | a value with no text form in `{= =}`: absent, or a function |
| E7003 | a template file cannot be read (also for `-` when standard input is not available) |
| E7004 | an invalid `@render` annotation (§3): the message names the key |
| E7005 | a fan-out path that is not a string, is empty, is absolute, leaves the destination directory, or repeats (§6) |

A rendering diagnostic's `file` is the template's path as given — the
`@render` string, the `--template` argument, `-`, or the `include`
string for an included file — or the module's for E7004 and E7005; its
`path` is `L:C`, the line and column of the tag it arose in (for E7003
on a root's own template and for E7004, the root's name; for E7005,
the element's document path), and it has no `id`. An expression that
fails to evaluate is reported with the language's own code, anchored
the same way: E3003 for an unknown name, E5001 for a division by zero,
E3019 for a `for` variable or a `set` that repeats a name in scope,
and E4001 for a value of the wrong kind — a condition or filter that
is not a `bool`, a `for` over something that is not an array (or, with
two variables, not an object or a map), an argument a `render`
function refuses. Diagnostics of the evaluation itself (binding,
assertions) keep their document paths, as `evaluate` prints them. The
report is ordered as §12.3 orders it, the template's diagnostics after
the document's.

## 6. Fan-out — `each`

`each` emits one file per element of the root. The root must be an
array or a map; for an array, `each` names the member of every element
that holds the file's path, and the elements must be records that have
it; for a map, `each: "$key"` takes the key as the path, or `each`
names a member of every value as for arrays. The path is a string,
relative, using `/`, non-empty, not escaping the destination directory
(no leading `/`, no `..` segment), and distinct across the elements;
anything else is E7005 at that element's document path, and no file of
the root is written. The paths are the module's to compute — a derived
member (`path = \`units/${name}.service\``), or the map's key in a
comprehension — so that they appear in the evaluated JSON and are
tested like any value.

Each element is emitted in the root's `format` / `indent`, or through
its `template` with the context of §5.4 (`item`, `key`, the members,
the root's name). The destination is a directory (§3.2); the files are
written in element order, directories created on the way; nothing else
in the directory is touched or removed. The aggregate (`decl evaluate`
with no `--output`) still carries the root as one JSON value.

In the REPL, and when the library returns the texts, a fan-out root is
a map from path to text.

## 7. The library

The three APIs grow in the `evaluate` vocabulary
([TypeScript / npm](../../decl-ts/README.md), [Python](../../decl-py/README.md),
[Rust](../../decl-rs/README.md)):

- `render(path, { inputs?, outputs?, format?, indent?, templates? })`
  → `{ [root]: text | { [file]: text } }`: each root in its declared
  form with the options as overrides (`templates` a map from root name
  to a template — the path of a template file, or `{ text }` for its
  text; `"*"` for every root without one of its own), a single text
  for a one-file root and a map from path to text for a fan-out root.
  Nothing is written to disk; a failure throws / raises / returns the
  same error type as `evaluate` with the diagnostics of §5.8 (E7003
  and E7004 included). `tests/api/` holds the cases.
- `toJson(value, indent?)` and `toYaml(value, indent?)` — the text of a
  JSON value in the layouts of §4, pure functions with no universe
  behind them, for a program that has a document and wants its text.
  The value may be given as canonical JSON text, which passes through
  with its number texts (`1.0` stays a float); the Rust crate takes the
  text only, since its documents are texts.
- `evaluate` and `validate` accept a document path ending in `.yaml` /
  `.yml` and read it by §2; a document given as a value is unchanged.

The internal modules gain one file each, in the three implementations
under the same name (AGENTS.md): `yaml` (the reader and the writer) and
`render` (the annotation, the template parser and renderer, the
fan-out).

## 8. The REPL and the editors

The REPL ([02. REPL](02_repl.md)) binds `:bind name=doc.yaml` by §2;
`:evaluate root…` prints each root in its declared form (a fan-out root
as its files in order, each preceded by a line `# <path>`; a rendering
diagnostic, then `(invalid)`), and accepts `--format`, `--indent`, and
`--template path` before the roots as overrides; `:evaluate` with no
root prints the aggregate as before. `:help` says so; `tests/repl/render`
is the session.

The language server's `decl.evaluate` command ([03. Language server](03_lsp.md)
§12) answers the root's declared form beside its document, and the VS
Code extension's output preview ([04. Extension](04_extension.md))
shows it: YAML, indented JSON, or a template's text with the buffer's
language set to match, a fan-out root as its files each under a
`# <path>` line, a rendering error in the document's place; no setting
is needed. Zed and the other configurations have no preview and change
nothing. `tests/lsp/render-preview` is the session.

## 9. The corpus

`tests/render/` holds the shared data, one driver per implementation
(`decl-ts/tests/render_test.ts`, `decl-rs/tests/render_test.rs`,
`decl-py/tests/render_test.py`), and the harness replays it:

- `cases.json` — one entry per case, in the shape of
  `tests/cli/cases.json`: the files of the case (the module with its
  `@render`, its templates and documents), the command line, and the
  recorded outcome — exit status, standard output and error, the
  files left. Every key of §3, every statement of §5.3, every text form
  of §5.5, every function of §5.6, every error of §5.8, the whitespace
  rules of §5.2, the template sources of §3.4, and fan-out over an
  array and a map have a case.
- `formats.json` — pairs of a golden document (`tests/golden/`) and its
  YAML form under `yaml/`, plus its indented JSON forms for the indents
  the entry names; the harness runs `evaluate --format yaml` /
  `--indent n` on the aggregate and diffs.
- `inputs/` — the YAML twin of every document the golden corpus binds
  (`tests/golden/inputs/`): the harness binds the twin and expects the
  JSON golden, which proves the reader; `invalid/` holds documents the
  reader must reject, each with its E6004 message.
- the parity harness gains sections `yaml-input`, `format`, and
  `render` (byte for byte, exit code, both streams, and the files
  written under a temporary directory, each implementation in its own
  copy of the case), and the REPL corpus gains a session exercising
  `:evaluate` on declared forms.

The expected texts are produced once by the reference and reviewed,
like every golden (tests/golden/README.md).

## 10. What the specification changes

The language does not change. Three texts of the specification do, as
one revision (v0.3.1, REVISIONS.md):

- §5.10 lists `@render` among the known annotations, metadata only,
  its keys fixed by this document.
- §10.6 gains an editorial pointer: interchange stays JSON; documents
  in YAML and documents written as YAML or as text are tool-side
  conversions defined by this document.
- §12.1 names the band **E7xxx — rendering** and §12.4 registers
  E7001–E7005 (append-only); the E6004 condition is reworded to "a
  document that cannot be read, or is not well-formed JSON or YAML, or
  uses a YAML construct outside the core schema".
- The charter records the decision (D35): a module declares the form
  its outputs are emitted in; rendering — formats, templates, fan-out —
  is tooling, fixed by this document and held identical across the
  three implementations by the corpus.

## 11. Decisions and open questions

Decided for this phase:

- **The form is declared in the module** (`@render`), the options are
  overrides, and there is one verb: `evaluate` emits each root in its
  declared form. There is no `decl render`.
- **Destinations are the invocation's** (`--output`), with `file` as a
  declared default; a fan-out root's paths are values the module
  computes.
- **TOML is not in this delivery.** TOML has no null and its root must
  be a table, so a loss rule is needed, and a writer would be
  hand-built three times; YAML serves the need for a second structured
  form and templates serve the rest. If it comes, it comes fail-closed:
  an error naming the first path that cannot be represented.
- The value delimiter is `{= =}` and the delimiters can be declared
  per root; `{{ }}` is text.
- The `loop` object has exactly five members; `set`, `include`, and
  `raw` are in; `extends` / `block` / `macro` are not, until a template
  in the wild needs inheritance; the default whitespace rules are
  Jinja's trim/lstrip switched on; a template may come from standard
  input as `-`.

Open: whether `render.indent` should also take a first-line prefix;
whether a `newline` key (`lf` | `crlf`) is wanted for templates on
Windows (the template's own endings are kept for now); whether a
fan-out root should also be able to emit an index file.

## 12. Status

Delivered (Phase 10, 2026-09-06): documents in YAML, `@render` with
every key of §3, the layouts of §4, the template dialect of §5, the
fan-out of §6, the library of §7, the REPL and the preview of §8, and
the corpus of §9, identical across the three implementations under the
parity harness; the specification's revision v0.3.1 (§10) recorded. The
open questions of §11 stay open.

## 13. Verification

- Every case of `tests/render/` identical across the three
  implementations in the harness — exit code, standard output, standard
  error, and the files written.
- Every key of §3, every construct of §5.2–5.3, every text form of
  §5.5, every function of §5.6, every error of §5.8, and fan-out over an
  array and a map with a case.
- Every golden's YAML form read back by a YAML 1.2 reader (the harness
  uses PyYAML in 1.2 core-schema mode and `serde_yaml`'s reader through
  the Rust suite) equal to the golden; every golden's indented JSON
  parsed back equal to the golden.
- Every YAML twin under `tests/render/inputs/` binding to the JSON
  golden; every document under `invalid/` rejected with its message.
- The REPL corpus session and the extension's preview exercised.
