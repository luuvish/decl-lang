# Decl in Emacs

Not an extension: Emacs 29+ has tree-sitter (`treesit`) and an LSP
client (`eglot`) built in. `decl-mode.el` here is the whole
configuration: `decl-ts-mode` with fontification, indentation, imenu,
and `which-function` over the grammar, `.decl` files bound to it, and
`decl-lsp` registered with eglot.

1. Build the grammar library where `treesit` looks for it:

   ```sh
   mkdir -p ~/.emacs.d/tree-sitter
   cd tree-sitter-decl && tree-sitter build -o ~/.emacs.d/tree-sitter/libtree-sitter-decl.dylib   # .so on Linux
   ```

   or, after loading the mode, `M-x treesit-install-language-grammar RET
   decl` (the mode adds the repository to `treesit-language-source-alist`).
   Other directories go in `treesit-extra-load-path`.

2. Load the mode: `(load "/path/to/decl-lang/extension/emacs/decl-mode.el")`
   or put the file on `load-path` and `(require 'decl-mode)`.

3. `decl-lsp` on `PATH` (`cargo install decl-lang`, `pip install
   decl-lang`, or `npm install -g decl-lang`); `M-x eglot` in a `.decl`
   buffer starts it, or `(add-hook 'decl-ts-mode-hook #'eglot-ensure)`
   starts it on open. Diagnostics arrive through flymake, hover through
   eldoc, and the rest (completion, definition, references, rename,
   formatting, code actions) through eglot's usual commands.

The font-lock rules mirror the grammar's `queries/highlights.scm`
(`treesit` reads Lisp rules, not `.scm` files); the features on by
default are comments, definitions, keywords, strings, and types, with
constants, numbers, calls, properties, and built-ins at
`treesit-font-lock-level` 3 and operators, brackets, and delimiters at
level 4.

`smoke.sh` builds the library into a scratch directory and runs
`smoke.el` in batch Emacs: the mode, fontification, indentation, eglot
connecting to the server, the fixture's diagnostic through flymake, and
hover on a type name. `extension/smoke-editors.sh` runs it with the
other editors' checks.
