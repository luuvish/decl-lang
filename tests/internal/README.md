# Internal checks

The corpora hold the language's observable behavior; this layer holds
what they cannot see: invariants of the implementation's own functions
(the number and string writers, path parsing, diagnostic ordering, the
package hash, the session log) and one check at each module boundary
(parse, semantics, infer, checker, engine, module, package, fmt,
conformance, pipeline, session, yaml, render), so that a corpus failure can be
localized to the module that regressed.

`checks.json` defines every check once — its module, its name, and the
statement each implementation must establish:

```json
{ "module": "engine", "name": "cycle",
  "check": "`type T = { a = b, b = a }` with an output of T reports the dependency cycle as E5007" }
```

Each implementation carries the check under its own name, in its own
language, against its own internal API — the criteria are shared, the
code is not:

| Language | File | The check `engine.cycle` |
|---|---|---|
| TypeScript | `decl-ts/tests/internal/engine_test.ts` | `check('cycle', …)` |
| Rust | `decl-rs/tests/internal/engine_test.rs` (declared in `internal/main.rs`) | `#[test] fn cycle()` |
| Python | `decl-py/tests/internal/engine_test.py` | `def test_cycle()` |

`coverage.py` reads the three and reports every check a language lacks
(and every check a language carries that `checks.json` does not name);
the parity harness runs it as its last section, so the gate fails when
the three drift apart. A check enters this file only when a tool
surface (the command line, the REPL's `:type` / `:trace` / `:time`, the
server's syntax tree) cannot observe it, or when it names the module a
corpus failure would otherwise not localize; everything else is a
corpus entry.
