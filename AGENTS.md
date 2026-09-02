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
- `../../research/oic-design-suite` - Real-world NoC spec tooling:
  editor/LSP contract docs, real design fixtures (JSON)

## Working Rules

- Documents are written in English.
- The specification under `docs/specification/` (v0.1, **frozen**) is the
  single source of truth; design decisions and spec chapters must never be
  left diverged.
- Post-freeze spec changes are **revisions**: amend the charter decision,
  every affected chapter, and `docs/REVISIONS.md` in one change.
- Update the doc index in `docs/README.md` when adding documentation.

## Implementations — one behavior, three times

| Directory | Role | Package |
|---|---|---|
| `decl-typescript/` | the **reference implementation**: the whole language (parser binding, checker, evaluator, packages, formatter, `decl-lsp`) plus the browser bundle for the website | npm `decl-lang` |
| `decl-rust/` | the whole language natively: `decl` (check / evaluate / validate / fmt, packages) and `decl-lsp` | crates.io `decl-lang` |
| `decl-python/` | the whole language natively: `decl`, `decl-lsp`, and a Python API — no Node.js | PyPI `decl-lang` |
| `tree-sitter-decl/` | the single grammar all three use | — |
| `tests/` | shared corpus (`validation/`, `modules/`, `packages/`) and the parity harness (`parity/`) | — |

The three sit side by side at the top level, named like the grammar
(`tree-sitter-decl/`) — the Node side (`decl-typescript`,
`tree-sitter-decl`, `site`) is one npm workspace rooted at the top-level
`package.json` (single lockfile, `npm ci` once at the root), and
`decl-rust` is a member of the Cargo workspace rooted at the
top-level `Cargo.toml` (`cargo build` at the root, binary in `target/`).
`packaging/` is different: distribution channels and the Homebrew tap,
not source. The three mirror one module layout; a language rule lives
in the same-named file everywhere:

| Concept | `decl-typescript/src` | `decl-rust/src` | `decl-python/decl/runtime` |
|---|---|---|---|
| AST | `ast.ts` | `ast.rs` | dict-shaped, built by `parse.py` |
| CST → AST | `parse.ts` | `parse.rs` | `parse.py` |
| values, environment, type resolution | `semantics.ts` | `semantics.rs` | `semantics.py` |
| subsumption ⊑ | `subsume.ts` | `subsume.rs` | `subsume.py` |
| binding, evaluation, validation, serialization | `engine.ts` | `engine.rs` | `engine.py` |
| modules and the universe | `module.ts` | `module.rs` | `module.py` |
| packages: manifest, resolver, lock | `package.ts` | `package.rs` | `package.py` |
| static checker, expression inference | `checker.ts`, `infer.ts` | `checker.rs`, `infer.rs` | `checker.py`, `infer.py` |
| canonical formatter | `fmt.ts` | `fmt.rs` | `fmt.py` |
| language server (stdio) | `lsp.ts` | `lsp.rs` (+ `lsp_main.rs`) | `lsp.py` |
| command line | `cli.ts` | `cli.rs` (+ `main.rs`) | `cli.py` (+ `__main__.py`) |
| browser bundle for the website | `web.ts` | — | — |

Rules:

- **Every behavior change lands in all three implementations in one
  change** (the TypeScript reference first; then Rust and Python, which
  are faithful ports — keep their structure and names aligned).
- **`make verify` is the gate** and must pass before a commit: each
  implementation's own tests, then `tests/parity/differential.py`,
  which diffs the Rust and Python implementations against the reference
  byte for byte: `check --json` (codes and messages) over every fixture
  and example, `evaluate --json` (`ok`, canonical JSON, diagnostics)
  over every output-bearing module, documents bound to `input` roots,
  `fmt` output over every parseable module, package resolution and lock
  diagnostics, and one scripted `decl-lsp` session. CI runs the same gate (`.github/workflows/verify.yml`).
- A parity difference is a defect in whichever side diverges from the
  specification — fix the implementation, never the expectation.

## Website (`site/`)

The website (Astro Starlight, published to GitHub Pages by
`.github/workflows/site.yml`) is generated **from** `docs/` and the
package READMEs by `site/scripts/sync-docs.mjs` at build time; the
synced pages under `site/src/content/docs/` are gitignored. Never edit
them — edit `docs/`. Hand-written pages are the landing page
(`index.mdx`), `start/`, and `playground.mdx`. The playground bundles
`decl-typescript/src/web.ts` for the browser (`dist/web.js`, built by `npm run build`).
Every ```decl block on the site must evaluate cleanly with the reference
implementation.

## Code Style (for future phases)

- **Decl files**: 4-space indentation, no tabs, 100-char line width
- **Test fixtures**: descriptive `snake_case` filenames under `valid/` and
  `invalid/` per feature; `invalid/` files carry `@expect-error` /
  `@expect-message` / `@expect-phase` metadata comments

## Commit Messages

Use short, lower-case, descriptive messages without scopes (e.g., `refine docs`,
`resolve open items in lexical chapter`).
