# Decl in Helix

Not an extension: Helix has tree-sitter and an LSP client built in.
Append `languages.toml` here to `~/.config/helix/languages.toml` (with
`DECL_ROOT` replaced by the repository's path), put the queries in the
user runtime, and build the grammar:

```sh
mkdir -p ~/.config/helix/runtime/queries/decl ~/.config/helix/runtime/grammars
cp tree-sitter-decl/queries/{highlights,locals,textobjects}.scm ~/.config/helix/runtime/queries/decl/
cp tree-sitter-decl/queries/helix/indents.scm ~/.config/helix/runtime/queries/decl/
hx --grammar build
hx --health decl
```

`decl-lsp` must be on `PATH`. Helix's indent query has its own dialect
(`@indent` / `@outdent`), hence `queries/helix/indents.scm`; the other
queries are the grammar's as they are. `session.exp` drives an editor
session on a pseudo-terminal for `extension/smoke-editors.sh`.
