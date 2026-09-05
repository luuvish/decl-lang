# zed-decl

The Zed extension for [Decl](https://github.com/luuvish/decl-lang):
the tree-sitter grammar (fetched from this repository's
`tree-sitter-decl/` and compiled by Zed), the language definition and
queries under `languages/decl/` (`highlights.scm` is the grammar's
`queries/highlights.scm` verbatim; the others are Zed's own), and the small module in `src/lib.rs`
that tells Zed where `decl-lsp` is — `lsp.decl-lsp.binary.path`, then
`decl-lsp` on `PATH`, else the prebuilt binary of the latest GitHub
release for your platform. The design is
[docs/tooling/04_extension.md](../../docs/tooling/04_extension.md) §14–§16.

## Install

Until the extension is in the Zed registry: *Extensions → Install Dev
Extension* and choose this directory. Zed compiles the Rust module with
your toolchain (`rustup target add wasm32-wasip1` once, and `cargo` on
the PATH Zed sees — a login-shell `~/.zprofile` with `. "$HOME/.cargo/env"`)
and fetches the grammar with git at the commit `extension.toml` pins
(`rev`): bump it when `tree-sitter-decl/` changes. The manifest names
the public https URL of the repository.

## Settings

```json
{
  "lsp": {
    "decl-lsp": {
      "binary": { "path": "/usr/local/bin/decl-lsp", "arguments": [] },
      "settings": { "inputs": { "deployed": "doc.json" } }
    }
  }
}
```

`settings` is forwarded to the server as its `decl` configuration — the
same keys the VS Code extension's `decl.*` settings carry.

## Tasks

`runnables.scm` tags an `output` declaration and a fixture's `@expect-*`
header; bind the tags in `.zed/tasks.json`:

```json
[
  { "label": "decl evaluate $ZED_SYMBOL", "command": "decl", "args": ["evaluate", "$ZED_FILE", "--output", "$ZED_SYMBOL"], "tags": ["decl-evaluate"] },
  { "label": "decl validate $ZED_DIRNAME", "command": "decl", "args": ["validate", "$ZED_DIRNAME/.."], "tags": ["decl-fixture"] }
]
```
