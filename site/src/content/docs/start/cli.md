---
title: Command line
description: decl check, evaluate, validate, fmt, and the decl-lsp language server.
sidebar:
  order: 2
---

```bash
decl check schema.decl                       # parse + static checks, module-aware (exit 1 on errors)
decl evaluate site.decl                      # evaluate every output -> {"name": value, ...}
decl evaluate site.decl --root site          # one output -> its canonical JSON
decl evaluate site.decl --json               # {"ok", "value", "diagnostics"} report
decl evaluate cfg.decl --input deployed=doc.json --root deployed   # bind a document, emit its completed value
decl validate cfg.decl --input deployed=doc.json     # bind a document to an input root (diagnostics only)
decl validate cfg.decl --input deployed=doc.json --expect-errors E4001
decl validate tests/validation               # judge a fixture corpus (valid/ and invalid/)
decl fmt src/*.decl                          # canonical formatting in place
decl fmt --check src/*.decl                  # exit 1 when a file is not canonical
decl-lsp                                     # stdio language server for editors
```

## Diagnostics

Every problem is reported as a diagnostic:

```text
cfg.decl: error [E4001] at deployed.port: out of range 1..65535
cfg.decl: error [E6001] TlsConfig.cert_present at deployed.tls: cert_path is required when tls is enabled
cfg.decl: warn [W6001] Service.scaled at deployed.workers: replicas 100 is outside the recommended range
```

`file: severity [code] id at path: message`. Codes are stable and registered in the specification ([12. Errors](/decl-lang/specification/12_errors/)); `id` names the assertion or the typed `else` message that fired; `path` is the canonical place in the document. With `--json`, the same fields arrive as a JSON array (`check`, `validate`) or inside the `{ok, value, diagnostics}` report (`evaluate`).

Failures never cascade: a member that fails to bind is *invalidated*, and everything that depends on it stays silent instead of reporting a second, misleading error ([06. Constraints](/decl-lang/specification/06_constraints/) §6.7).

## Editors

`decl-lsp` (from any of the three packages) speaks the Language Server Protocol over stdio and provides diagnostics, hover, and go-to-definition. Point your editor's generic LSP client at it for `.decl` files:

- **VS Code**: a generic LSP client extension configured with `command: decl-lsp`, `languageId: decl`.
- **Neovim**: `vim.lsp.start({ name = 'decl', cmd = { 'decl-lsp' }, root_dir = vim.fs.root(0, { 'decl.toml', '.git' }) })` from a `FileType decl` autocommand.
- **Helix**: add a `[[language]]` entry with `language-servers = ["decl-lsp"]` and `[language-server.decl-lsp] command = "decl-lsp"`.

Syntax highlighting comes from the tree-sitter grammar in the repository (`tree-sitter-decl/`), which editors with tree-sitter support can load directly.
