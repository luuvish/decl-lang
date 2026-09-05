# Decl in Sublime Text

A package, not an extension with code: the syntax definition
(`Decl.sublime-syntax`, the regex counterpart of the grammar's
`queries/highlights.scm`), its syntax tests, the syntax-specific
settings (4-space indentation, a ruler at 100), comment toggling, and
the language-server client settings for the
[LSP](https://packagecontrol.io/packages/LSP) package.

Install by copying this directory into `Packages/Decl`:

```sh
cp -R extension/sublime "$HOME/Library/Application Support/Sublime Text/Packages/Decl"   # macOS
# Linux: ~/.config/sublime-text/Packages/Decl   Windows: %APPDATA%\Sublime Text\Packages\Decl
```

`.decl` files then open with the Decl syntax. For diagnostics, hover,
completion, navigation, rename, and formatting, install **LSP** from
Package Control (`Package Control: Install Package` → `LSP`) and put
`decl-lsp` on `PATH` (`cargo install decl-lang`, `pip install
decl-lang`, or `npm install -g decl-lang`); the package's
`LSP.sublime-settings` registers the server for `source.decl`, so
nothing else is needed. To point at a binary elsewhere, override
`clients.decl-lsp.command` in `Packages/User/LSP.sublime-settings`.

The syntax tests run inside Sublime: open `syntax_test_decl.decl` and
run **Build** (the "Syntax Tests" build system; results in the output
panel). `smoke.sh` runs the same tests headlessly through
[syntect](https://github.com/trishume/syntect), the Rust engine that
implements the Sublime syntax format.

Limits of a regex syntax: a named type is recognised by convention
(capitalised names are types, all-caps names constants), a pattern
`/…/` where an operand begins, a derived member at the start of a line
— the language server, not the syntax, knows the actual declarations.
