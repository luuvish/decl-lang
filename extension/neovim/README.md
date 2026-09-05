# Decl in Neovim

Not an extension: Neovim 0.10+ has tree-sitter and an LSP client built
in. `init.lua` here is the whole configuration — copy its body into
yours — after the parser and the queries are in place:

```sh
cd tree-sitter-decl && tree-sitter build -o ~/.config/nvim/parser/decl.so
mkdir -p ~/.config/nvim/queries/decl && cp tree-sitter-decl/queries/*.scm ~/.config/nvim/queries/decl/
```

`decl-lsp` must be on `PATH` (`cargo install decl-lang`, `pip install
decl-lang`, or `npm install -g decl-lang`). The queries are the
grammar's (`tree-sitter-decl/queries/`): highlights, locals, folds,
indents (nvim-treesitter's dialect), text objects. With nvim-treesitter
the same files serve as the `decl` queries once the parser is
registered.

`smoke.lua` is the headless check `extension/smoke-editors.sh` runs.
