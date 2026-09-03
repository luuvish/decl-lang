# Decl Language

**Decl** is a general-purpose declarative language for describing, generating,
and validating structured data. The three capabilities are not separate modes
but three views of a single evaluation semantics — evaluation always produces
a pair of *(resolved values, diagnostics)*.

- **Describe** — declare types, schemas, constraints, and diagnostics in a
  form people can read and review.
- **Generate** — deterministically evaluate defaults, derived properties, and
  comprehensions into fully resolved value trees, exportable to standard
  formats such as JSON.
- **Validate** — check both language-defined values and externally supplied
  data against the same rules, producing diagnostics with stable ids.

Design goals:

- **JSON superset** — every JSON document is a valid Decl value; external
  data becomes a validation target without conversion.
- **Pure, deterministic, terminating** — no side effects and no recursion, so
  every evaluation terminates; the same input yields the same values and the
  same diagnostics regardless of implementation.
- **First-class diagnostics** — constraints and diagnostics are language
  constructs with stable ids, severities, and message templates.
- **No domain-specific features** — domain semantics (hardware, networking,
  configuration, …) are expressed at the library level with types, derived
  properties, functions, and constraints, never as language keywords.

## Install

The `decl` command ships through several channels:

```bash
npm install -g decl-lang          # npm — the TypeScript reference implementation (Node.js ≥ 20)
pip install decl-lang             # PyPI — the native Python implementation (no Node.js)
cargo install decl-lang           # crates.io — the native Rust implementation
brew install luuvish/tap/decl-lang    # Homebrew tap
```

```bash
decl check schema.decl                     # parse + static checks (module-aware)
decl evaluate site.decl                    # the exported outputs -> JSON on stdout
decl evaluate site.decl --output site=site.json --output report   # one document to a file, one to stdout
decl evaluate cfg.decl --input deployed=doc.json --output deployed   # bind a document, emit its completed value
decl validate cfg.decl --input deployed=doc.json --expect-errors E4001
decl fmt --check src/*.decl                # canonical formatting
```

Python users also get an API (`decl.evaluate`, `decl.check`,
`decl.validate`, `decl.format_source`); JavaScript users import the
library entry of `decl-lang`. See [packaging/README.md](packaging/README.md)
for the channels and release procedure.

## Implementations

Decl is implemented three times — `decl-typescript/`, `decl-rust/`, `decl-python/` — and the three must be
indistinguishable: **one behavior, three implementations**. The Node
side is one npm workspace (root `package.json`), Rust a Cargo workspace
(root `Cargo.toml`); `packaging/` holds distribution channels, not source.

| Directory | Language | Package | Scope |
|---|---|---|---|
| [`decl-typescript/`](decl-typescript/README.md) | TypeScript — the **reference implementation** | npm `decl-lang` | the whole language: parser binding, static checker, evaluator, validation, packages, formatter, `decl-lsp`; `decl-lang/core` runs in browsers too |
| [`decl-rust/`](decl-rust/README.md) | Rust — native implementation | crates.io `decl-lang` | the whole language, natively: `decl check` / `evaluate` / `validate` / `fmt`, packages, and `decl-lsp` — no Node or wasm |
| [`decl-python/`](decl-python/README.md) | Python — native implementation + API | PyPI `decl-lang` | the whole language, natively: `decl check` / `evaluate` / `validate` / `fmt`, packages, `decl-lsp`, and a Python API — no Node.js |
| `tree-sitter-decl/` | C (tree-sitter) | — | the one grammar every implementation compiles or loads |
| `tests/` | — | — | the shared conformance corpus (`validation/`, `modules/`, `packages/`) and the parity harness (`parity/`) |

Each implementation mirrors the same module layout (`parse`, `semantics`,
`subsume`, `infer`, `checker`, `engine`, `module`, `package`, `fmt`, `lsp`,
`cli`), so a rule of the language lives in the same-named file in all three. A change to the language's behavior
lands in all three in one change, and the gate is:

```bash
make verify        # each implementation's tests, then tests/parity/differential.py
```

The parity harness diffs the Rust and Python implementations against the
reference byte for byte: static diagnostics over every fixture and
example (`check`), evaluation reports (`ok`, canonical JSON,
diagnostics) over every output-bearing module, documents bound to
`input` roots, formatter output for every parseable module, package
resolution and lock diagnostics, and one scripted language-server
session. CI runs it on every push (`.github/workflows/verify.yml`). The
website's playground runs the reference implementation's platform-neutral
core (`decl-lang/core`) in the browser.

## Status

**Specification v0.3 (2026-09-04)** — v0.1 was frozen on 2026-08-31,
revised through the v0.2 cycle (D31–D33, D29 amended), then v0.3: member
kinds by shape and hidden members `x$ = e` (D4 amended, D34;
[docs/REVISIONS.md](docs/REVISIONS.md)). All roadmap phases are
complete: the tree-sitter grammar, the TypeScript reference
implementation with its full static checker, modules and packages with
a reproducible lock, the complete standard library, the `decl` CLI /
formatter / LSP, and real-world validation on three domain examples
(`examples/`). Ten test suites (`decl-typescript/`, `npm test`) and the fixture
corpus (`tests/validation`) are the conformance baseline.

## Documents

- **Website** — <https://luuvish.github.io/decl-lang/>: the guide, the
  specification, the examples, and a browser playground (built from
  `docs/` by [site/](site/README.md))
- [ROADMAP.md](ROADMAP.md) — development roadmap; owns the plan and progress
- [docs/](docs/README.md) — language documentation: design charter,
  the normative specification (13 chapters), the guide, and the
  benchmark cases
- [decl-typescript/](decl-typescript/README.md) — the reference implementation, CLI,
  formatter, and LSP server; [decl-rust/](decl-rust/README.md) and
  [decl-python/](decl-python/README.md) — the native runtimes
- [examples/](examples/) — domain examples used for real-world validation
- [packaging/](packaging/README.md) — distribution channels and releases
