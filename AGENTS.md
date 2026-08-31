# AGENTS.md

Guidance for coding agents working in this repository.

## Project Overview

Decl is a general-purpose declarative language for describing, generating, and
validating structured data with strong type safety and constraint checking.

This repository holds the language's **frozen v0.1 specification**
(docs/, authored from scratch in Phase 0 and gated by the `spike/`
evaluator) and, phase by phase, its implementation: tree-sitter parser
(Phase 1), TypeScript reference implementation (Phase 2), modules and
stdlib (3), CLI/LSP (4), real-world validation (5). The sibling
repositories below are reference material only.

`ROADMAP.md` owns the phase plan, exit criteria, and progress status; update
its phase table when a phase advances.

Sibling repositories used as reference (do not modify from here):

- `../decl-lang` - Previous iteration: Decl 2 spec drafts, legacy Decl 1
  tree-sitter grammar, and 142 validation fixtures
- `../../research/oic-design-suite` - Real-world NoC spec tooling: NoC LSP
  contract docs, real NoC (JSON) fixtures

## Working Rules

- Documents are written in English.
- The specification under `docs/specification/` (v0.1, **frozen**) is the
  single source of truth; design decisions and spec chapters must never be
  left diverged.
- Post-freeze spec changes are **revisions**: amend the charter decision,
  every affected chapter, and `docs/REVISIONS.md` in one change.
- Update the doc index in `docs/README.md` when adding documentation.

## Code Style (for future phases)

- **Decl files**: 4-space indentation, no tabs, 100-char line width
- **Test fixtures**: descriptive `snake_case` filenames under `valid/` and
  `invalid/` per feature; `invalid/` files carry `@expect-error` /
  `@expect-message` / `@expect-phase` metadata comments

## Commit Messages

Use short, lower-case, descriptive messages without scopes (e.g., `refine docs`,
`resolve open items in lexical chapter`).
