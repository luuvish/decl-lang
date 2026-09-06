# Why Decl

Decl is a language for three jobs that usually take three tools:
**describing** the shape structured data may take, **generating**
data from that description, and **validating** documents that come
from elsewhere against it. The description is a set of types; a type
is a set of values; a document is valid when the type subsumes it,
written `v ⊑ T`. Every judgement the language makes — a default
applies, a derived member is well-typed, a document passes — is an
instance of that one relation
([03. Types](../specification/03_types.md) §3.17).

This page places Decl beside the languages it is most often compared
with. It states what each is for in its own terms and where Decl
differs, so that you can tell in a minute whether it is the right tool.
The comparison is against the neighbours' public documentation as of
2026; corrections are welcome.

## The neighbours

**CUE** unifies types and values in one lattice: a schema, a
constraint, and a concrete value are all values, and combining them is
unification. It validates data files against definitions (`cue vet`),
generates through comprehensions, has no user-defined functions or
recursion by design, and is packaged through modules with a registry.
One implementation, in Go.

**Pkl** (Apple) is configuration as a program: classes with typed
properties and constraints, modules that amend other modules, and
functions and generators for producing values. Types are checked while
the program evaluates; it renders to JSON, YAML, plist, properties, and
more, and generates data classes for Java, Kotlin, Swift, and Go. It is
implemented on the JVM with native executables, and has a language
server.

**Jsonnet** is JSON templating: a dynamically typed functional language
with object inheritance, late binding, and `assert`, evaluated
hermetically to JSON or YAML. Several implementations exist (C++, Go,
Scala, Rust) and it has a community language server; packaging is left
to external tools.

**Nickel** (Tweag) is configuration with contracts: gradual typing for
the parts you annotate, contracts checked at evaluation for the rest,
record merging as the composition primitive, and clear, labelled error
reports. One implementation, in Rust, with a language server and, more
recently, a package manager.

**JSON Schema** is a vocabulary for describing and validating JSON
documents, not a language: no expressions, no generation, and a
validator in every ecosystem. The 2019-09 and later drafts define
output formats that locate a failure by instance path and keyword.

## Side by side

| | Decl | CUE | Pkl | Jsonnet | Nickel | JSON Schema |
|---|---|---|---|---|---|---|
| For | describe, generate, validate | validate, configure | configure, render | template JSON | configure, contracts | validate JSON |
| Types | static, structural, `⊑` | values in a lattice | checked at evaluation | none | gradual, contracts | the schema |
| Outside documents | `input` roots, same pipeline | `cue vet` | `read()`; Pkl against Pkl | `assert` | contracts | its purpose |
| A failure reports | code, id, severity, message, path; root cause; canonical order | message, position | message, position, expression | message, stack | labelled message | per validator |
| Quantities | `quantity<D>`, SI, user units | no | `Duration`, `DataSize` | no | no | no |
| References | `ref<T>`, checked, paths, `$referrers` | resolved in output | late-bound | `self`, `super` | field references | `$ref` to schemas |
| Generation | comprehensions, functions, `with`, `match` | comprehensions | functions, generators | functions, inheritance | functions, merging | none |
| Modules | import/export, `decl.toml`, lock with hashes | modules, registry | modules, packages | file imports | imports; packages (new) | `$id`, `$ref` |
| Always terminates | yes | yes | no | no | no | yes |
| Output | canonical JSON | JSON, YAML, TOML, Go | JSON, YAML, plist, … | JSON, YAML | JSON, YAML, TOML | — |
| Implementations | three, byte-identical | one, Go | one, JVM + native | several | one, Rust | many |
| Editor | language server ×3; VS Code, Zed, … | language server | language server, IDEs | community server | language server | JSON editors |

The cells are short on purpose; the paragraphs above and below carry
the qualifications.

## Where Decl differs

- **One relation.** There is no separate validation vocabulary. The
  type that generated a value is the type an external document is
  bound to, and binding runs the same pipeline: defaults are filled,
  derived members computed, constraints checked. What you generate and
  what you accept cannot drift apart
  ([Decl by example](01_overview_by_example.md),
  [10. Interchange](../specification/10_interchange.md)).
- **Diagnostics are part of the specification.** A failure is a
  record with a stable code, the declaration that raised it, a
  severity, a message, and a path into the document; only the root
  cause is reported, never the cascade; and the order of the report is
  defined ([06. Constraints](../specification/06_constraints.md),
  [12. Errors](../specification/12_errors.md)). Tools can be built on
  it, and every implementation prints the same report.
- **Quantities are types.** `250ms` is a value of `quantity<Time>`;
  `3km / 500ms` is a `quantity<Length / Time>`; a document may say
  `{ "value": 2, "unit": "TiB" }` and it is checked against the
  dimension, normalized, and compared correctly
  ([03. Types](../specification/03_types.md) §3.16,
  [Quantities and units](04_quantities_and_units.md)).
- **References are data.** A `ref<T>` points at a place inside the
  document, is checked to exist, serializes as a path, and can be
  followed backwards with `$referrers`
  ([07. Relationships](../specification/07_relationships.md)).
- **It stops.** No recursion and bounded comprehensions make every
  module terminate; the same source gives the same bytes on every run
  and every implementation, which the repository checks on every
  change ([09. Semantics](../specification/09_semantics.md)).
- **JSON is the data model.** Every JSON document is a Decl value and
  every Decl value serializes to canonical JSON; a JSON parser is a
  complete front end for documents
  ([10. Interchange](../specification/10_interchange.md)).

## When another tool is the better one

- You need output formats with comments, anchors, or ordering
  guarantees beyond JSON — Pkl and Nickel render more formats today;
  Decl's YAML and template rendering is planned
  ([05. Renderer](../tooling/05_render.md)).
- Your schemas already live in JSON Schema and the surrounding
  tooling (form generation, API documentation) is what you use —
  stay there, or convert as Nickel can.
- You want configuration as a general program with recursion and
  arbitrary computation — Pkl, Jsonnet, and Nickel are programming
  languages; Decl deliberately is not
  ([00. Vision](../design/00_vision.md),
  [01. Requirements](../design/01_requirements.md)).

## Where to go next

- [Decl by example](01_overview_by_example.md): one scenario, end to
  end.
- The tutorials: [validating documents](02_validating_documents.md),
  [generating configuration](03_generating_configuration.md),
  [quantities and units](04_quantities_and_units.md),
  [modules and packages](05_modules_and_packages.md).
- The [specification](../specification/01_introduction.md), read
  linearly.

---

- Index: [Documentation home](../README.md)
