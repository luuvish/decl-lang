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
| `modules/`, `packages/` | multi-module universes and packages with manifests and locks | the three suites (loading, linking, resolution, lock drift) and the harness's rows |
| `subsume/` | `prelude.decl` (types) and `cases.txt`: the subsumption judgment and structural emptiness, one case per line, the sides written as types of the language | `decl-ts/test/subsume.ts`, `decl-rs/tests/e2e.rs`, `decl-py/scripts/e2e.py` |
| `golden/` | the expected evaluation (canonical JSON) of every example and module entry, and of documents bound to inputs (`inputs/`: a fabric site and its corrupted variants, whose goldens are the diagnostics that reject them); `manifest.json` names them | the harness's `golden` section: every implementation — the reference included — must print those bytes |
| `repl/` | scripted REPL sessions with their transcripts | the three suites (also under `DECL_FULL_RECOMPUTE=1`) and the harness |
| `parity/` | `differential.py`: every command line, REPL session, and language-server request of the reference against the Rust and Python implementations — exit code, stdout, stderr, answers | `make verify`, CI |

The language server's editor session lives in the harness (`lsp_transcript`)
and is the shared test of the three servers; each suite also drives its
own server through the same requests for a fast local check.

Rules:

- A new behavior lands with its corpus entry (a fixture, a case, a
  golden, a session), never with a test in one language only.
- A golden is reviewed data: when an implementation disagrees with it,
  the specification decides which one is wrong; regenerate a golden only
  for a deliberate change, in the same commit as the change.
- A parity difference is a defect in the implementation that diverges
  from the specification — fix the implementation, never the expectation.
