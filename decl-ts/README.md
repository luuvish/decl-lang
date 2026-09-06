# decl-lang

**Decl** is a declarative language for describing, generating, and
validating structured data — a JSON superset with a strong static type
system, constraints with first-class diagnostics, references, physical
quantities, generics, and modules. Pure, deterministic, terminating.

This package is the **reference implementation** of the whole language:
the `decl` command-line tool, the canonical formatter, the `decl-lsp`
language server, and a library — in TypeScript, on Node.js ≥ 22, with
no runtime dependencies (the tree-sitter grammar ships as wasm). Its
platform-neutral core also runs in a browser.

```bash
npm install -g decl-lang
```

## Command line

```bash
decl check schema.decl                         # static checks, module-aware (exit 1 on errors)
decl evaluate site.decl                        # evaluate every exported output -> {"name": value, ...}
decl evaluate site.decl --output site          # one output -> its canonical JSON on stdout
decl evaluate site.decl --json                 # {"ok", "value", "diagnostics"} report
decl evaluate cfg.decl --input deployed=doc.json --output deployed=out.json   # bind a document, write its completed value
decl validate cfg.decl --input deployed=doc.json   # bind a document to an input root (diagnostics only)
decl validate tests/validation                 # judge a fixture corpus
decl fmt src/*.decl                            # canonical formatting in place (--check: exit 1 if not canonical)
decl repl site.decl                            # an interactive session: expressions, bindings, edits, undo
decl-lsp                                       # language server over stdio
```

Diagnostics go to stderr as `file: severity [code] id at path: message`,
or into the JSON report with `--json`. The exit code is 1 when any error
was reported. The command line, the REPL, and the server are documented
in the repository (`docs/tooling/`), and the VS Code and Zed extensions
are clients of `decl-lsp`.

## Library

```ts
import { evaluate, check, validate, formatSource, DeclError } from 'decl-lang';

const docs = await evaluate('site.decl');                         // { site: {...} } — the exported outputs, by name
const { site } = await evaluate('site.decl', { outputs: ['site'] });
const done = await evaluate('cfg.decl', { inputs: { deployed: 'doc.json' }, outputs: ['deployed'] });
const problems = await check('schema.decl');                      // [] when clean
const report = await validate('cfg.decl', { inputs: { deployed: { host: 'h' } } });   // a document may be a value
const text = await formatSource('const x=1+2\n');                 // 'const x = 1 + 2\n'
```

The functions are the `decl` command line in its own vocabulary:
`inputs` binds documents by input name (a JSON file path, or the value
itself), `outputs` names the roots to return — outputs, or inputs bound
here or demanded through their fallback — and defaults to the entry
module's exported outputs; a failure throws `DeclError`, whose
`diagnostics` carry the report. The PyPI package (`decl.evaluate`, …)
and the Rust crate (`decl_lang::evaluate`, …) offer the same functions
with the same semantics; the modules the functions are built from are
exported as well.

The platform-neutral core is a second entry, `decl-lang/core`, for
browsers and other non-Node hosts: everything that runs anywhere
JavaScript runs, over an in-memory host, with the grammar wasm's
location passed in instead of found on disk.

```js
import { initParser, evaluateSource, format } from 'decl-lang/core';
await initParser({ grammar: '/assets/tree-sitter-decl.wasm', runtime: '/assets/tree-sitter.wasm' });
const report = evaluateSource('output x: int = 1\n');   // { ok, outputs: [{ name, json }], diagnostics, ... }
```

## Library layout

The package is the same modules as the other two implementations, one
language rule in the same-named file everywhere (`AGENTS.md` in the
repository), and each is exported:

| Module | Holds |
|---|---|
| `api` | the high-level API above: `evaluate`, `check`, `validate`, `evaluateSource`, `formatSource`, `DeclError` |
| `ast` | the syntax tree (specification chapter 11): declarations, types, members, expressions, source ranges |
| `parse` | the tree-sitter binding: `initParser`, `parseSource`, source text to `ast` |
| `semantics` | values, the environment (`Env`), resolved types, diagnostics (`Diag`), canonical paths, the JSON reader and writers |
| `subsume` | the subsumption judgment ⊑ (§3.17) and structural emptiness (§3.19) |
| `infer` | expression inference and the static assignability of §4 |
| `checker` | the static checks of a module (`checkModule`) |
| `engine` | binding, lazy evaluation, validation, serialization (`Engine`) |
| `pipeline` | one module end to end (`runPipeline`, `evaluateSource`) |
| `module` | modules and the universe (`loadModules`, `runUniverse`) |
| `package` | manifests, the resolver, the lock file (§8.6–8.7) |
| `fmt` | the canonical formatter (`format`) |
| `conformance` | the fixture corpus judge |
| `session` | the evaluation session behind the REPL and the server (`Session`) |
| `repl` | `decl repl` |
| `lsp-core`, `lsp`, `lsp-web` | the language server's every answer; `decl-lsp` over stdio; the server in a web worker |
| `cli` | `decl` |
| `host`, `node` | the file system behind the core: the disk under Node, memory elsewhere; the Node entry that locates the grammar |
| `core`, `index` | the two entries: `decl-lang/core` (platform-neutral) and `decl-lang` (Node) |

## Scope

The package covers the whole language: parsing, the static checks of
chapters 3–4 (type resolution with generics and dimension algebra,
inference, assignability, the absence discipline, `match`
exhaustiveness), binding with lazy slots and cycle detection,
`$referrers`, assertions with diagnostic templates, canonical
serialization, modules and packages (`decl.toml`, `decl.lock`), the
canonical formatter, the REPL, and the language server. It is the
reference the other two implementations are held to: the repository's
parity harness (`tests/parity/differential.py`, run by `make verify`)
diffs the Rust and Python runtimes against it, byte for byte, over
every example and fixture that produces output.

## Building from source

```bash
npm install                                 # once, at the repository root (npm workspaces)
npm test -w decl-lang                       # the corpora under tests/, one driver each, and the internal checks
npm run build -w decl-lang                  # dist/: the command line, the server, both entries, the grammar wasm
node decl-ts/src/cli.ts evaluate docs/examples/02_config.decl --output prod   # without building
```

The grammar comes from `../tree-sitter-decl` as the committed
`tree-sitter-decl.wasm`, consumed through `web-tree-sitter` — no native
build step.

## License

MIT — see `LICENSE`.
