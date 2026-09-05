# Decl Documentation

**Status: v0.3 (2026-09-04)** — v0.1 frozen 2026-08-31, revised through
the v0.2 cycle (D31–D33, D29 amended, clarifications), then v0.3: member
kinds read off `?` and `= e` with no `const` in record bodies (D4
amended), and hidden members `x$ = e` (D34) — see
[REVISIONS.md](REVISIONS.md). The normative specification below is the
single source of truth for every implementation phase.

**Post-freeze changes are revisions**: a change touches the design
charter (a new or amended decision), every affected chapter, and
[REVISIONS.md](REVISIONS.md) — in one commit. Divergence between
charter and chapters remains a defect to fix on sight (§1.4).

## Index

### Design documents (why the language is shaped this way)

| Document | Description |
|---|---|
| [00. Vision and Background](design/00_vision.md) | Why this language exists: the config-language landscape, the agent-era turn, lessons and defect review from the previous Decl iteration, and the checklist of issues the new spec must resolve (informative) |
| [01. Design Requirements](design/01_requirements.md) | Goals, non-goals, capability requirements (describe / generate / validate / modules / tooling), the generality benchmark, and the quality bar — with the vision checklist promoted to requirements |
| [02. Design Decisions](design/02_design_decisions.md) | The charter: principles P1–P7, decisions D1–D33, the rejected-syntax table, the comprehensive example, vision-checklist traceability, and the revision-tracked decision log — spec chapters must not contradict it |
| [03. v0.2 Revision Candidates](design/03_v02_revision_candidates.md) | Findings from implementing Phases 2–4 and the Phase 5 real-world sweeps, adjudicated 2026-09-01 into revisions v0.1.4–v0.1.8 (the v0.2 cycle); each entry records its outcome |

### Language specification (normative — the single source of truth)

| Chapter | Contents |
|---|---|
| [01. Introduction](specification/01_introduction.md) | What Decl is, core concepts, authority and precedence, conformance, chapter map |
| [02. Lexical Structure](specification/02_lexical.md) | Source text, comments, identifiers, keywords, literals, separators, operators |
| [03. Type System](specification/03_types.md) | All type forms, dimensions/units, subsumption (⊑), assignability, uninhabited types |
| [04. Expressions](specification/04_expressions.md) | Operator precedence, arithmetic and numeric safety, `match`, comprehensions, absence, pipeline, `with` |
| [05. Declarations and Schemas](specification/05_declarations.md) | `const`/`func`/`type`/`output`/`input`, the four member kinds and hidden members, constraint-member placement, inheritance, annotations |
| [06. Constraints and Diagnostics](specification/06_constraints.md) | `assert`/`when`, `diagnostic` templates, type-level `else`, severities, invalidation and root-cause reporting, paths and ordering |
| [07. Relationships](specification/07_relationships.md) | Composition vs reference, canonical paths and their order, context variables, reference construction and integrity, `$referrers` |
| [08. Modules and Packages](specification/08_modules.md) | Exports/imports/re-export, provenance rules, `decl.toml`, the lock file, multi-module evaluation |
| [09. Evaluation Semantics](specification/09_semantics.md) | Pipeline, dependency graph, laziness, determinism and numeric rules, invalidation, partial evaluation, termination |
| [10. Data Interchange](specification/10_interchange.md) | Input binding, serialization policy, canonical JSON text, total round-trip idempotence, JSON-only scope |
| [11. Grammar](specification/11_grammar.md) | The formal grammar (EBNF): declarations, types, members, expressions, data documents, disambiguation notes — wins over prose on conflict |
| [12. Errors and Diagnostic Codes](specification/12_errors.md) | Code scheme and bands, machine-readable report format, ordering and conformance scope, the append-only registry |
| [13. Standard Library](specification/13_stdlib.md) | The complete `std.*` surface: array/math/int/float/string/object/map functions, the SI unit catalog, reserved `std.graph` |

### Guide (informative)

| Document | Description |
|---|---|
| [Decl by Example](guide/01_overview_by_example.md) | One scenario end to end — describe → generate → validate — with the evaluated JSON and the diagnostics it produces |

### Tooling (informative)

| Document | Description |
|---|---|
| [01. Command line](tooling/01_cli.md) | `decl check` / `evaluate` / `validate` / `fmt`: universes, `--input` / `--output`, diagnostics and the `--json` report, exit codes |
| [02. REPL](tooling/02_repl.md) | `decl repl`: a session over the evaluation universe — bare expressions as partial evaluation, session outputs, document edits with exact undo/redo, incremental re-evaluation, and the commands that mirror the CLI verbs |
| [03. Language server](tooling/03_lsp.md) | `decl-lsp`: the server's capabilities, one by one — diagnostics, hover, completion, navigation, hierarchies, rename, code actions, hints, lenses, semantic tokens, workspaces |
| [04. Editor extensions](tooling/04_extension.md) | `vscode-decl` and `zed-decl`: the editor faces of the server and the REPL — VS Code's language contribution, server management, live output preview, bound inputs, trace view, tasks, Test Explorer, and web extension; Zed's grammar, queries, runnables, and server pointer; other editors |
| [05. Renderer](tooling/05_render.md) | `--format yaml` and `decl render`: structured formats as tool-side conversions, and the template dialect implemented three times (planned, Phase 10) |
| [Development handbook](DEVELOPMENT.md) | How the repository is set up and worked on: layout and configuration files, toolchains and versions, getting started, building and testing, quality tools, CI, releases, editors, conventions |

### Validation cases (§0.5 desk-check artifacts)

| Document | Description |
|---|---|
| [examples/](examples/README.md) | The generality benchmark cases in the new syntax, with the desk-check findings they produced |
