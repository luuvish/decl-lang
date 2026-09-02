# decl

**Decl** is a declarative language for describing, generating, and
validating structured data — a JSON superset with a strong static type
system, constraints with first-class diagnostics, references, physical
quantities, generics, and modules. Pure, deterministic, terminating.

This package ships the `decl` command-line tool, the canonical
formatter, the `decl-lsp` language server, and a small Python API.

`decl evaluate` and `decl validate` (and the `decl.evaluate` /
`decl.validate` API) run on a **native Python runtime** — the
tree-sitter grammar compiled as a C extension plus a pure-Python port
of the evaluator, byte-identical to the reference implementation. The
static checker (`decl check`), the formatter, and the language server
run the bundled reference implementation (JavaScript + tree-sitter
wasm) under Node.js ≥ 20.

```bash
pip install decl              # evaluate/validate need no Node; check/fmt/lsp use the node on your PATH
pip install 'decl[node]'      # or let pip provide Node.js too
```

## Command line

```bash
decl check schema.decl                   # parse + static checks (module-aware)
decl evaluate site.decl --root site      # evaluate outputs -> JSON
decl validate cfg.decl --input deployed=doc.json --expect-errors E4001
decl validate tests/validation           # judge a fixture corpus
decl fmt --check src/*.decl              # canonical formatting
decl-lsp                                 # stdio language server for editors
```

## Python API

```python
import decl

value = decl.evaluate("site.decl", root="site")       # dict / list / scalar
problems = decl.check("schema.decl")                   # [] when clean
report = decl.validate("cfg.decl", input=("deployed", "doc.json"))
text = decl.format_source("const x=1+2\n")            # 'const x = 1 + 2\n'
```

Every call runs the same implementation as the CLI and returns its
machine-readable report; `decl.DeclError.diagnostics` carries the
diagnostics (`file`, `severity`, `code`, `id`, `path`, `message`) when
an operation fails.

## A taste of the language

```decl
type Service = {
    name: /[a-z][a-z0-9-]*/
    port: 1024..65535 = 8080
    replicas: 1..64 = 1
    const endpoint = `${name}:${port}`
    assert grpc_ports: name != "grpc" || port >= 9000
        else warn `grpc convention is 9000+`
}

export output demo: Service[] = [
    { name: "gateway" }
    { name: "auth", port: 9001, replicas: 2 }
]
```

Specification, guide, and sources: https://github.com/luuvish/decl-lang
