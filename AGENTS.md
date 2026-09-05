# AGENTS.md

Guidance for coding agents working in this repository.

## Project Overview

Decl is a general-purpose declarative language for describing, generating, and
validating structured data with strong type safety and constraint checking.

This repository holds the language's **frozen v0.1 specification**
(docs/, authored from scratch in Phase 0 and gated by the `spike/`
evaluator) and, phase by phase, its implementation: tree-sitter parser
(Phase 1), TypeScript reference implementation (Phase 2), modules and
stdlib (3), CLI/LSP (4), real-world validation (5).

`ROADMAP.md` owns the phase plan, exit criteria, and progress status; update
its phase table when a phase advances.

## Working Rules

- Documents are written in English.
- The specification under `docs/specification/` (v0.1, **frozen**) is the
  single source of truth; design decisions and spec chapters must never be
  left diverged.
- Post-freeze spec changes are **revisions**: amend the charter decision,
  every affected chapter, and `docs/REVISIONS.md` in one change.
- Update the doc index in `docs/README.md` when adding documentation.
- `docs/DEVELOPMENT.md` records the toolchains and versions the
  repository is developed and verified with, and the version policy
  (minimums in the manifests, pins in `mise.toml` — `mise install`
  gives a machine the versions CI uses); update it when a requirement
  changes.

## Implementations — one behavior, three times

| Directory | Role | Package |
|---|---|---|
| `decl-ts/` | the **reference implementation**: the whole language (parser binding, checker, evaluator, packages, formatter, `decl-lsp`); its platform-neutral core is also what the website's playground runs | npm `decl-lang` |
| `decl-rs/` | the whole language natively: `decl` (check / evaluate / validate / fmt, packages) and `decl-lsp` | crates.io `decl-lang` |
| `decl-py/` | the whole language natively: `decl`, `decl-lsp`, and a Python API — no Node.js | PyPI `decl-lang` |
| `tree-sitter-decl/` | the single grammar all three use | — |
| `extension/vscode/`, `extension/zed/` | the editor extensions (docs/tooling/04_extension.md): clients of `decl-lsp`, the grammar's queries; `extension/neovim/`, `extension/helix/`, `extension/emacs/`, `extension/vim/`, `extension/sublime/` are configurations (a mode, a syntax, a package), not extensions, and `extension/smoke-editors.sh` checks them | Marketplace / Open VSX, the Zed registry (planned) |
| `tests/` | the shared corpora (`validation/`, `modules/`, `packages/`, `subsume/`, `golden/`, `repl/`) and the parity harness (`parity/`) — see `tests/README.md` | — |

The three sit side by side at the top level, named by language
(`decl-ts`, `decl-rs`, `decl-py`) beside the grammar
(`tree-sitter-decl/`) and the editor extensions (`extension/vscode`,
`extension/zed`) — the Node side (`decl-ts`, `tree-sitter-decl`, `site`,
`extension/vscode`) is one npm workspace rooted at the top-level
`package.json` (single lockfile, `npm ci` once at the root), and
`decl-rs` is a member of the Cargo workspace rooted at the top-level
`Cargo.toml` (`cargo build` at the root, binary in `target/`).
`packaging/` is different: distribution channels and the Homebrew tap,
not source. The three mirror one module layout; a language rule lives
in the same-named file everywhere:

| Concept | `decl-ts/src` | `decl-rs/src` | `decl-py/decl/runtime` |
|---|---|---|---|
| AST | `ast.ts` | `ast.rs` | dict-shaped, built by `parse.py` |
| CST → AST | `parse.ts` | `parse.rs` | `parse.py` |
| values, environment, type resolution | `semantics.ts` | `semantics.rs` | `semantics.py` |
| subsumption ⊑ | `subsume.ts` | `subsume.rs` | `subsume.py` |
| binding, evaluation, validation, serialization | `engine.ts` | `engine.rs` | `engine.py` |
| single-module pipeline, source-level report (`evaluateSource`) | `pipeline.ts` | `pipeline.rs` | `pipeline.py` |
| modules and the universe | `module.ts` | `module.rs` | `module.py` |
| packages: manifest, resolver, lock | `package.ts` | `package.rs` | `package.py` |
| static checker, expression inference | `checker.ts`, `infer.ts` | `checker.rs`, `infer.rs` | `checker.py`, `infer.py` |
| canonical formatter | `fmt.ts` | `fmt.rs` | `fmt.py` |
| corpus judgment | `conformance.ts` | `conformance.rs` | `conformance.py` |
| session object (universe + operation log), the REPL | `session.ts`, `repl.ts` | `session.rs`, `repl.rs` | `session.py`, `repl.py` |
| language server | `lsp-core.ts` (every answer), `lsp.ts` (stdio), `lsp-web.ts` (a web worker) | `lsp.rs` (+ `lsp_main.rs`) | `lsp.py` |
| command line | `cli.ts` | `cli.rs` (+ `main.rs`) | `cli.py` (+ `__main__.py`) |
| platform: locating the grammar (Rust and Python compile it in; TypeScript loads a wasm, from disk in `node.ts` or from a URL) and reaching files (TypeScript through a host: the disk under Node, memory in a browser) | `host.ts`, `node.ts`, entries `core.ts` (platform-neutral) / `index.ts` (Node) | `lib.rs` | `_tree_sitter/` |

Rules:

- **Every behavior change lands in all three implementations in one
  change** (the TypeScript reference first; then Rust and Python, which
  are faithful ports — keep their structure and names aligned).
- **`make verify` is the gate** and must pass before a commit: each
  implementation's own tests, then `tests/parity/differential.py`,
  which diffs the Rust and Python implementations against the reference
  byte for byte — exit code, standard output, and standard error of every
  command line: `check` and `evaluate` (with and without `--json`) over
  every fixture and example, documents bound to `input` roots
  (`validate --input`, `evaluate --input`), `fmt` output over every
  parseable module, package resolution and lock reports, and one scripted
  `decl-lsp` session. Diagnostic reports therefore have one field order
  (§12.2: `file, code, id, severity, message, path`) and one file-tag
  rule (the entry module by the path given, other modules by absolute
  path) in all three. CI runs the same gate (`.github/workflows/verify.yml`).
- A parity difference is a defect in whichever side diverges from the
  specification — fix the implementation, never the expectation.
- **Tests are shared data** (`tests/README.md`): fixtures, subsumption
  cases, golden outputs, and REPL sessions under `tests/` that all three
  implementations run; a per-language test only drives those corpora or
  covers a surface that exists in that language alone.

## Website (`site/`)

The website (Astro Starlight, published to GitHub Pages by
`.github/workflows/site.yml`) is generated **from** `docs/` and the
package READMEs by `site/scripts/sync-docs.mjs` at build time; the
synced pages under `site/src/content/docs/` are gitignored. Never edit
them — edit `docs/`. Hand-written pages are the landing page
(`index.mdx`), `start/`, and `playground.mdx`. The playground bundles
`decl-lang/core` (the reference implementation's platform-neutral entry,
`dist/core.js` built by `npm run build`) with Vite; the grammar's wasm
files are static assets under `site/public/playground/`.
Every ```decl block on the site must evaluate cleanly with the reference
implementation.

## Code Style

- **Each language's canonical form and lints** (docs/DEVELOPMENT.md,
  Quality tools): Prettier, ESLint, and `tsc` for TypeScript; rustfmt
  and clippy for Rust; ruff and mypy for Python. `make lint` must be
  clean and `make format` leaves the tree unchanged before a commit; a
  rule is switched off only in the configuration file, with its reason.
- **Decl files**: 4-space indentation, no tabs, 100-char line width
- **Test fixtures**: descriptive `snake_case` filenames under `valid/` and
  `invalid/` per feature; `invalid/` files carry `@expect-error` /
  `@expect-message` / `@expect-phase` metadata comments

## Commit Messages

Use short, lower-case, descriptive messages without scopes (e.g., `refine docs`,
`resolve open items in lexical chapter`).
