# API cases

The three packages offer the command line as a library — `evaluate`,
`check`, `validate`, `format_source` (`formatSource` in JavaScript) —
in one vocabulary: inputs bound by name, roots returned by name, a
failure carrying the report. `cases.json` lists the calls; `expected.json`
holds the answers the reference gives, reviewed; `documents/` the files
the cases bind.

A case is one call, named by the key present, with repository-relative
paths passed as given (every driver runs from the repository root):

```json
{ "name": "evaluate: outputs selects roots", "evaluate": "docs/examples/02_config.decl", "outputs": ["prod"] }
{ "name": "…", "evaluate": "…", "inputs": { "base": { "file": "tests/api/documents/base.json" } }, "outputs": ["base", "copy"] }
{ "name": "…", "evaluate": "…", "inputs": { "base": { "json": { "host": "v" } } }, "outputs": ["copy"] }
{ "name": "…", "check": ["tests/modules/basic/main.decl", "tests/modules/errors/collision.decl"] }
{ "name": "…", "validate": "docs/examples/02_config.decl", "inputs": { "deployed": { "json": { … } } } }
{ "name": "…", "format_source": "const x=1+2\n" }
```

An input document is a file (`{ "file": path }`) or the value itself
(`{ "json": value }`). A driver runs every case and prints one JSON
array of answers:

```json
{ "name": "…", "ok": true, "value": <the answer> }
{ "name": "…", "ok": false, "message": "no root named nope", "diagnostics": [ … ] }
```

The answer of `evaluate` is the roots by name, **in declaration order**,
each the document; of `check` and `validate` the diagnostics in the
report's field order (§12.2: `file, code, id, severity, message,
path`, absent fields omitted); of `format_source` the text. A failure
is the error's message and its diagnostics. Documents compare by value:
`expected.json` is canonical JSON, and a driver whose language reads
`6.0` as `6` is not wrong.

The drivers, one per language, each compared with `expected.json` by
its suite and run by the parity harness:

| Language | Driver | Suite |
|---|---|---|
| TypeScript | `decl-ts/scripts/api-corpus.ts` | `decl-ts/tests/api_test.ts` |
| Rust | `decl-rs/examples/api_corpus.rs` (Cargo's place for a runnable auxiliary outside the crate; `cargo run --release --example api_corpus`) | `decl-rs/tests/api_test.rs` |
| Python | `decl-py/scripts/api_corpus.py` | `decl-py/tests/api_test.py` |

A new call lands with its case here and its answer in `expected.json`,
regenerated from the reference in the same change and read.
