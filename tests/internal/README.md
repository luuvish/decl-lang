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

`rounds.json` selects the shared reference fixtures for
`engine.reference_round_reuse`. Every case compares the query evaluator with
fresh evaluation. Cases marked `reuse` also require multiple retained rounds
and surviving records; matching output through fallback alone is insufficient.
Cases marked `cutoff` also require equal-result and downstream verification
cutoffs in the actual Engine slot graph.

`programs.json` supplies document and function-produced record cases for
`engine.shared_programs`. Each driver compares values and diagnostics with fresh
compilation-free tree evaluation, then checks that increasing instance counts
does not increase expression or schema compilation counts.

`edits.json` supplies Session operation sequences for `session.retained_edits`.
Each step compares all roots and diagnostics against fresh evaluation. The
cases cover member and container edits, optional presence, rebinding, undo/redo,
function scopes, aggregate comparisons, reference rounds, error recovery and
removal of dependencies whose obsolete computation would otherwise fail.
Work-count assertions also require stable program and record identities, few
executed member programs, bounded preparation, and both result and downstream
verification cutoffs.

`temporary.json` checks repeated temporary expressions and errors in one Session.
The persistent program cache, registered records, and slot graph must not grow;
Rust's explicit materialization cache and Python/Rust input metadata must also
stay bounded. Temporary request programs have their own lifetime.
The same file drives `session.retained_lifetime`: repeated create/remove at
unique map keys must not accumulate removed records, assertion dependencies or
query change stamps. The Session operation log intentionally keeps undo history;
these checks cover auxiliary evaluator caches instead.
