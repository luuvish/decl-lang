# decl

**Decl** is a declarative language for describing, generating, and
validating structured data — a JSON superset with a strong static type
system, constraints with first-class diagnostics, references, physical
quantities, generics, and modules. Pure, deterministic, terminating.

This package is a **native Python implementation** of the whole
language — the tree-sitter grammar compiled as a C extension plus a
pure-Python port of the static checker, the evaluator, packages, the
canonical formatter, and the language server, byte-identical to the
reference implementation. It ships the `decl` command-line tool, the
`decl-lsp` language server, and a small Python API. No Node.js is
involved.

```bash
pip install decl-lang     # installs as decl-lang, imports as `decl` — the command's name and the module path (decl.runtime)
```

## Command line

```bash
decl check schema.decl                   # parse + static checks (module-aware)
decl evaluate site.decl                  # the exported outputs -> JSON on stdout
decl evaluate site.decl --output site=site.json --output report   # one document to a file, one to stdout
decl evaluate cfg.decl --input deployed=doc.json --output deployed   # bind a document, emit its completed value
decl validate cfg.decl --input deployed=doc.json --expect-errors E4001
decl validate tests/validation           # judge a fixture corpus
decl fmt --check src/*.decl              # canonical formatting
decl repl site.decl                      # an interactive session: expressions, bindings, edits, undo
decl-lsp                                 # stdio language server for editors
```

## Python API

```python
import decl

docs = decl.evaluate("site.decl")                       # {"site": {...}} — the exported outputs, by name
site = decl.evaluate("site.decl", outputs=["site"])["site"]
done = decl.evaluate("cfg.decl", inputs={"deployed": "doc.json"}, outputs=["deployed"])["deployed"]
problems = decl.check("schema.decl")                    # [] when clean
report = decl.validate("cfg.decl", inputs={"deployed": {"host": "h"}})   # a document may be a value, not a file
text = decl.format_source("const x=1+2\n")             # 'const x = 1 + 2\n'
```

The functions are the `decl` command line in its own vocabulary:
`inputs` binds documents by input name (a JSON file path, or the value
itself), `outputs` names the roots to return — outputs, or inputs bound
here or demanded through their fallback — and defaults to the entry
module's exported outputs. The npm package (`evaluate`, `check`,
`validate`, `formatSource`) and the Rust crate (`decl_lang::evaluate`, …)
offer the same functions with the same semantics.

Every call runs the same implementation as the CLI and returns its
machine-readable report; `decl.DeclError.diagnostics` carries the
diagnostics (`file`, `severity`, `code`, `id`, `path`, `message`) when
an operation fails.

## A taste of the language

```decl
type Service = {
    name: /[a-z][a-z0-9-]*/
    port?: 1024..65535 = 8080
    replicas?: 1..64 = 1
    endpoint = `${name}:${port}`
    assert grpc_ports: name != "grpc" || port >= 9000
        else warn `grpc convention is 9000+`
}

export output demo: Service[] = [
    { name: "gateway" }
    { name: "auth", port: 9001, replicas: 2 }
]
```

Specification, guide, and sources: https://github.com/luuvish/decl-lang
