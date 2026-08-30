# Decl Documentation

**Status: draft.** The specification is being written from scratch (see
[ROADMAP.md](../ROADMAP.md), Phase 0 — §0.2 in progress). The guide
follows once the chapters settle.

## Index

### Design documents (why the language is shaped this way)

| Document | Description |
|---|---|
| [00. Vision and Background](design/00_vision.md) | Why this language exists: the config-language landscape, the agent-era turn, lessons and defect review from the previous Decl iteration, and the checklist of issues the new spec must resolve (informative) |
| [01. Design Requirements](design/01_requirements.md) | Goals, non-goals, capability requirements (describe / generate / validate / modules / tooling), the generality benchmark, and the quality bar — with the vision checklist promoted to requirements |
| [02. Design Decisions](design/02_design_decisions.md) | The charter: principles P1–P7, decisions D1–D29, the rejected-syntax table, the comprehensive example, vision-checklist traceability, and the two spike-gated open questions — spec chapters must not contradict it |

### Language specification (normative — the single source of truth)

| Chapter | Contents |
|---|---|
| [01. Introduction](specification/01_introduction.md) | What Decl is, core concepts, authority and precedence, conformance, chapter map |
| [02. Lexical Structure](specification/02_lexical.md) | Source text, comments, identifiers, keywords, literals, separators, operators |
| [03. Type System](specification/03_types.md) | All type forms, dimensions/units, subsumption (⊑), assignability, uninhabited types |
| [04. Expressions](specification/04_expressions.md) | Operator precedence, arithmetic and numeric safety, `match`, comprehensions, absence, pipeline, `with` |
| [05. Declarations and Schemas](specification/05_declarations.md) | `const`/`func`/`type`/`output`/`input`, the four member kinds, constraint-member placement, inheritance, annotations |
| [06. Constraints and Diagnostics](specification/06_constraints.md) | `assert`/`when`, `diagnostic` templates, type-level `else`, severities, invalidation and root-cause reporting, paths and ordering |
| [07. Relationships](specification/07_relationships.md) | Composition vs reference, canonical paths and their order, context variables, reference construction and integrity, `$referrers` |
| [08. Modules and Packages](specification/08_modules.md) | Exports/imports/re-export, provenance rules, `decl.toml`, the lock file, multi-module evaluation |
| [09. Evaluation Semantics](specification/09_semantics.md) | Pipeline, dependency graph, laziness, determinism and numeric rules, invalidation, partial evaluation, termination |
| [10. Data Interchange](specification/10_interchange.md) | Input binding, serialization policy, canonical JSON text, total round-trip idempotence, JSON-only scope |
| [11. Grammar](specification/11_grammar.md) | The formal grammar (EBNF): declarations, types, members, expressions, data documents, disambiguation notes — wins over prose on conflict |
| [12. Errors and Diagnostic Codes](specification/12_errors.md) | Code scheme and bands, machine-readable report format, ordering and conformance scope, the full v0.1 registry |
| [13. Standard Library](specification/13_stdlib.md) | The complete `std.*` surface: array/math/int/float/string/object/map functions, the SI unit catalog, reserved `std.graph` |
