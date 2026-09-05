# Decl in Vim

Not an extension: classic Vim (9.1+) gets Decl through a runtime
directory here and the language server through a plugin. Copy this
directory's contents into `~/.vim/` (or add it as a package), and the
syntax, filetype settings, and indentation load for every `*.decl`
file:

```
~/.vim/ftdetect/decl.vim   ~/.vim/syntax/decl.vim
~/.vim/ftplugin/decl.vim    ~/.vim/indent/decl.vim
```

The syntax file is a regex approximation of the grammar's
`highlights.scm` (keywords, types, members, strings with `${...}`
interpolation, patterns, unit literals, `$` context variables); Vim has
no tree-sitter, so it is not the same parse the other editors use.

For the language server, install a general LSP client. **yegappan/lsp**
(Vim9, no dependencies) is the recommendation; with `decl-lsp` on
`PATH`, add to your `.vimrc`:

```vim
autocmd User LspSetup call LspAddServer([#{
      \   name: 'decl',
      \   filetype: 'decl',
      \   path: 'decl-lsp',
      \ }])
```

That gives diagnostics, hover, go-to-definition, references, rename,
symbols, and formatting — everything the server provides. On Vim 8 use
**prabirshrestha/vim-lsp** instead (register `decl-lsp` for the `decl`
filetype).

`smoke.sh` is the check run by hand before a release (see
`extension/smoke-editors.sh` for Neovim and Helix): it asserts the
syntax groups in Ex mode, opens an invalid fixture and reads the
diagnostic off the location list, and drives `:LspHover` over
yegappan/lsp, asserting the hover round-trip in the protocol log. Vim's
hover popup does not render reliably under a headless pseudo-terminal,
so that last check reads the protocol, not the screen; `session.exp`
drives the pseudo-terminal.
