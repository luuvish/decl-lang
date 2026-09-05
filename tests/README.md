# Tests — one behavior, one corpus

The three implementations (`decl-ts`, `decl-rs`, `decl-py`) are held to
the same tests by keeping the tests as **data under this directory**
that every implementation runs, plus the parity harness that compares
their command lines byte for byte. A per-language test file exists only
to drive these corpora or to cover a surface that exists in one language
alone (a library API's shape, a package's packaging).

| Corpus | What it holds | Who runs it |
|---|---|---|
| `validation/` | the conformance fixtures: `valid/` must parse, check, and evaluate clean; `invalid/` must fail in the phase and with the code (and message) their `@expect-*` headers name | `decl validate tests/validation` in all three; every fixture is also a `check` / `evaluate` row of the harness |
| `modules/` | multi-module universes: imports, re-export, the graph's error cases | the harness's `check` rows; `basic/` is a golden |
| `packages/` | packages with manifests and locks, and `cases.json`: the errors the command line reports and the lock's reproducibility and drift ([README](packages/README.md)) | `decl-ts/tests/packages_test.ts`, `decl-rs/tests/packages_test.rs`, `decl-py/tests/packages_test.py`; the harness's rows; `app/` is a golden |
| `subsume/` | `prelude.decl` (types) and `cases.txt`: the subsumption judgment and structural emptiness, one case per line, the sides written as types of the language | `decl-ts/tests/subsume_test.ts`, `decl-rs/tests/subsume_test.rs`, `decl-py/tests/subsume_test.py` |
| `golden/` | the expected bytes of every evaluation `manifest.json` names ([README](golden/README.md)): the examples and module entries, the guide assembled from its markdown, documents bound to inputs (`inputs/`: the benchmarks' round trips and corrupted variants, a fabric site and its corruptions, small modules for `match`, generics, and dimension algebra) | the three suites replay the manifest (`decl-ts/tests/golden_test.ts`, `decl-rs/tests/golden_test.rs`, `decl-py/tests/golden_test.py`); the harness's `golden` section: every implementation — the reference included — must print those bytes |
| `repl/` | scripted REPL sessions with their transcripts and the files they leave, each replayed in a fresh copy of its directory ([README](repl/README.md)) | the three suites (also under `DECL_FULL_RECOMPUTE=1`) and the harness |
| `fmt/` | the formatter's canonical-form cases ([README](fmt/README.md)) | the three formatter suites, which then check idempotence and AST preservation over the corpora; the harness |
| `cli/` | the command line case by case, with the outcome recorded from the reference ([README](cli/README.md)) | `decl-ts/tests/cli_test.ts`, `decl-rs/tests/cli_test.rs`, `decl-py/tests/cli_test.py`; the harness |
| `api/` | the library API's cases and the reviewed answers ([README](api/README.md)): `evaluate`, `check`, `validate`, `format_source` in one vocabulary | one driver per language (`decl-ts/scripts/api-corpus.ts`, `decl-rs/examples/api_corpus.rs`, `decl-py/scripts/api_corpus.py`), each suite against `expected.json`; the harness runs the three |
| `lsp/` | language-server sessions by capability, with their transcripts ([README](lsp/README.md)) | one replay driver per language (`decl-ts/tests/lsp_test.ts` and `lsp-core.ts` over the in-memory host, `decl-rs/tests/lsp_test.rs`, `tests/lsp/replay.py` for Python and the harness); the harness replays every session over the three servers |
| `internal/` | `checks.json`: the internal checks — invariants no tool surface observes, and one check per module boundary ([README](internal/README.md)); `coverage.py` holds the three suites to the list | `decl-ts/tests/internal/`, `decl-rs/tests/internal/`, `decl-py/tests/internal/`, one file per source module; the harness's last section |
| `parity/` | `differential.py`: every command line (the whole surface: usage, `--version`, `--expect-errors`, `validate <dir>`, `fmt --check`, every error path), REPL session, API case, and language-server session of the reference against the Rust and Python implementations — exit code, stdout, stderr, answers — and the goldens | `make verify`, CI |

The suites mirror each other file for file — one driver per corpus in
every language, under each implementation's `tests/`, named
`<corpus>_test.<ext>`, with shared helpers under `tests/common/`:

| Corpus | `decl-ts/tests/` | `decl-rs/tests/` | `decl-py/tests/` |
|---|---|---|---|
| `validation/` | `validation_test.ts` | `validation_test.rs` | `validation_test.py` |
| `api/` | `api_test.ts` | `api_test.rs` (driver: `../examples/api_corpus.rs`) | `api_test.py` (driver: `../scripts/api_corpus.py`) |
| `cli/` | `cli_test.ts` | `cli_test.rs` | `cli_test.py` |
| `fmt/` | `fmt_test.ts` | `fmt_test.rs` | `fmt_test.py` |
| `golden/` | `golden_test.ts` | `golden_test.rs` | `golden_test.py` |
| `lsp/` | `lsp_test.ts`, `lsp_core_test.ts` | `lsp_test.rs` | `lsp_test.py` (driver: `../../tests/lsp/replay.py`) |
| `packages/` | `packages_test.ts` | `packages_test.rs` | `packages_test.py` |
| `repl/` | `repl_test.ts` | `repl_test.rs` | `repl_test.py` |
| `subsume/` | `subsume_test.ts` | `subsume_test.rs` | `subsume_test.py` |

Beside the corpus drivers, each suite carries the internal checks under
`tests/internal/<module>_test.<ext>` — the same list, `tests/internal/checks.json`,
in each language's own code against its own internal API.

What a suite carries beyond that is a surface one language has:
the reference's in-memory host behind the browser's worker
(`lsp_core_test.ts` replays the session corpus over it, then pushes files
with `decl/files`), the Python API's path-like and iterable arguments
(`api_test.py`).

Rules:

- A new behavior lands with its corpus entry (a fixture, a case, a
  golden, a session), never with a test in one language only.
- A golden is reviewed data: when an implementation disagrees with it,
  the specification decides which one is wrong; regenerate a golden only
  for a deliberate change, in the same commit as the change.
- A parity difference is a defect in the implementation that diverges
  from the specification — fix the implementation, never the expectation.
