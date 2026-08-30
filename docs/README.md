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

Chapters 04–13 (expressions, declarations, constraints, relationships,
modules, semantics, interchange, grammar, errors, stdlib) are being
authored in ROADMAP §0.2 order.
