# AGENTS.md

Guidance for coding agents working in this repository.

## Project Overview

Decl is a general-purpose declarative language for describing, generating, and
validating structured data with strong type safety and constraint checking.

This repository is a fresh start. The spec documents previously migrated from
`../decl-lang` have been removed; the language specification will be written
completely anew, using the sibling repositories below as reference material
only. Implementation (tree-sitter parser, TypeScript reference implementation,
CLI, LSP) will be added phase by phase after the spec is settled.

`ROADMAP.md` owns the phase plan, exit criteria, and progress status; update
its phase table when a phase advances.

Sibling repositories used as reference (do not modify from here):

- `../decl-lang` - Previous iteration: Decl 2 spec drafts, legacy Decl 1
  tree-sitter grammar, and 142 validation fixtures
- `../../research/oic-design-suite` - Real-world NoC spec tooling: NoC LSP
  contract docs, real NoC (JSON) fixtures

## Working Rules

- Documents are written in English.
- Once written, the specification under `docs/` is the single source of truth;
  design decisions and spec chapters must never be left diverged.
- Update the doc index in `docs/README.md` when adding documentation.

## Code Style (for future phases)

- **Decl files**: 4-space indentation, no tabs, 100-char line width
- **Test fixtures**: descriptive `snake_case` filenames under `valid/` and
  `invalid/` per feature; `invalid/` files carry `@expect-error` /
  `@expect-message` / `@expect-phase` metadata comments

## Commit Messages

Use short, lower-case, descriptive messages without scopes (e.g., `refine docs`,
`resolve open items in lexical chapter`).
