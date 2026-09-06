# decl-lang

**Decl** is a declarative language for describing, generating, and
validating structured data — a JSON superset with a strong static type
system, constraints with first-class diagnostics, references, physical
quantities, generics, and modules. Pure, deterministic, terminating.

This crate is the **native Rust implementation** of the whole language:
the tree-sitter grammar is compiled in, `decl` checks, evaluates,
validates, and formats modules (packages included), and `decl-lsp` is
the language server — no Node.js or wasm involved.

```bash
cargo install decl-lang
```

## Command line

```bash
decl check schema.decl                         # static checks, module-aware (exit 1 on errors)
decl evaluate site.decl                        # evaluate every output -> {"name": value, ...}
decl evaluate site.decl --output site          # one output -> its canonical JSON on stdout
decl evaluate site.decl --json                 # {"ok", "value", "diagnostics"} report
decl evaluate cfg.decl --input deployed=doc.json --output deployed=out.json   # bind a document, write its completed value
decl evaluate cfg.decl --input deployed=doc.yaml --format yaml --pretty      # a document in YAML in, the outputs as YAML out
decl evaluate site.decl --output gateway                # a root in the form its @render declares: YAML, a template's text, one file per element
decl evaluate site.decl --output units=out --template units=unit.j2   # …or with the template and the destination given here
decl validate cfg.decl --input deployed=doc.json   # bind a document to an input root (diagnostics only)
decl validate tests/validation                 # judge a fixture corpus
decl fmt src/*.decl                            # canonical formatting in place (--check: exit 1 if not canonical)
decl repl site.decl                            # an interactive session: expressions, bindings, edits, undo
decl-lsp                                       # language server over stdio
```

Diagnostics go to stderr as `file: severity [code] id at path: message`,
or into the JSON report with `--json`. The exit code is 1 when any error
was reported. An output declares the form it is emitted in with
`@render({ format, indent, template, file, each })` — canonical or
indented JSON, YAML, or the text of a template in a small Jinja-like
dialect with Decl expressions inside, one file per element — and
`--format`, `--indent`, `--template`, `--output` override it
(`docs/tooling/05_render.md` in the repository).

## Library

```rust
use decl_lang::{check, evaluate, format_source, render, to_yaml, validate, DeclError, Document, EvaluateOptions, RenderOptions, Rendered, TemplateSource};

fn example() -> Result<(), DeclError> {
    // the exported outputs, by name in declaration order, as canonical JSON text
    let docs = evaluate("site.decl", &EvaluateOptions::default())?;
    let site = &evaluate("site.decl", &EvaluateOptions { outputs: vec!["site".into()], ..Default::default() })?["site"];
    // a document bound to an input root, and that root returned completed (a .yaml file is read as YAML)
    let done = evaluate("cfg.decl", &EvaluateOptions {
        inputs: vec![("deployed".into(), Document::File("doc.yaml".into()))], // or Document::Json(text)
        outputs: vec!["deployed".into()],
    })?;
    // each root in the form its @render declares: a text, or a fan-out root's files by path
    let texts = render("site.decl", &RenderOptions::default())?;
    if let Rendered::Text(yaml) = &texts["site"] { println!("{yaml}"); }
    let conf = render("site.decl", &RenderOptions {
        outputs: vec!["site".into()],
        templates: vec![("site".into(), TemplateSource::File("nginx.conf.j2".into()))], // or TemplateSource::Text(text)
        ..Default::default()
    })?;
    let problems = check(&["schema.decl"]); // empty when clean
    let report = validate("cfg.decl", &[("deployed".into(), Document::Json("{\"host\":\"h\"}".into()))])?;
    let text = format_source("const x=1+2\n")?; // "const x = 1 + 2\n"
    let y = to_yaml("{\"a\":[1,2]}", 2)?; // "a:\n  - 1\n  - 2" — the layouts, as pure functions over JSON text
    assert_eq!(text, "const x = 1 + 2\n");
    Ok(())
}
```

The functions are the `decl` command line in its own vocabulary:
`inputs` binds documents by input name (a JSON or YAML file, or JSON
text), `outputs` names the roots to return — outputs, or inputs bound
here or demanded through their fallback — and defaults to the entry
module's exported outputs; `render` returns each root's text in its
declared form (`Rendered::Text`, or `Rendered::Files` by path for a
fan-out root) with `RenderOptions` as the overrides, `to_json` and
`to_yaml` lay a document's text out; a failure is a `DeclError` whose
`diagnostics` carry the report. The npm package (`evaluate`, …) and the PyPI package
(`decl.evaluate`, …) offer the same functions with the same semantics;
the modules the functions are built from are public as well.

## Library layout

The crate is the same modules as the reference implementation, one
language rule in the same-named file everywhere (`AGENTS.md` in the
repository), and each is public:

| Module | Holds |
|---|---|
| `api` | the high-level API above: `evaluate`, `render`, `check`, `validate`, `evaluate_source`, `format_source`, `to_json`, `to_yaml`, `DeclError`, `Diagnostic`, `Document`, `EvaluateOptions`, `RenderOptions`, `Rendered`, `TemplateSource`, `Report` |
| `ast` | the syntax tree (specification chapter 11): declarations, types, members, expressions, source ranges |
| `parse` | the tree-sitter binding: `parse_source`, source text to `ast` |
| `semantics` | values, the environment (`Env`), resolved types (`RT`), diagnostics (`Diag`), canonical paths, the JSON reader and writers |
| `yaml` | documents in YAML: the YAML 1.2 core-schema reader into the JSON model, the block-style writer, the JSON layouts |
| `render` | the renderer: `@render`'s form, the template dialect, the fan-out — one root to its text or files |
| `subsume` | the subsumption judgment ⊑ (§3.17) and structural emptiness (§3.19) |
| `infer` | expression inference and the static assignability of §4 |
| `checker` | the static checks of a module (`check_module`) |
| `engine` | binding, lazy evaluation, validation, serialization (`Engine`) |
| `pipeline` | one module end to end (`run_pipeline`, `evaluate_source`) |
| `module` | modules and the universe (`load_modules`, `run_universe`) |
| `package` | manifests, the resolver, the lock file (§8.6–8.7) |
| `fmt` | the canonical formatter (`format`) |
| `conformance` | the fixture corpus judge (`judge_corpus`) |
| `session` | the evaluation session behind the REPL and the server (`Session`) |
| `repl` | `decl repl` |
| `lsp` | `decl-lsp` |
| `cli` | `decl` |

## Scope

The crate covers the whole language: parsing, the static checks of
chapters 3–4 (type resolution with generics and dimension algebra,
inference, assignability, the absence discipline, `match`
exhaustiveness), binding with lazy slots and cycle detection,
`$referrers`, assertions with diagnostic templates, canonical
serialization, modules and packages (`decl.toml`, `decl.lock`), the
canonical formatter, and the language server. Its output is
byte-identical to the reference implementation — the repository's
parity harness (`tests/parity/differential.py`, run by `make verify`)
diffs the Rust and Python runtimes against the reference over every
example and fixture that produces output.

## Building from source

The grammar's C sources are compiled by `build.rs`. Inside the
repository they come from `../tree-sitter-decl/src`; the published crate
carries a copy under `grammar/` (generated by `npm run build` in
`decl-ts/`).

```bash
cargo build --release                       # from the repository root (Cargo workspace)
./target/release/decl evaluate docs/examples/02_config.decl --output prod
```

## License

MIT — see `LICENSE`.
