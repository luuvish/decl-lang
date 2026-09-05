-- Decl in Neovim 0.10+ without plugins (docs/tooling/04_extension.md §17):
-- the parser and the grammar's queries on the runtimepath, the filetype,
-- tree-sitter highlighting and folding, and decl-lsp.
--
--   tree-sitter build -o ~/.config/nvim/parser/decl.so   (in tree-sitter-decl/)
--   cp tree-sitter-decl/queries/*.scm ~/.config/nvim/queries/decl/
--
-- DECL_NVIM_CFG names a directory holding parser/ and queries/ instead of
-- ~/.config/nvim; DECL_LSP names the server instead of `decl-lsp` on PATH.
if vim.env.DECL_NVIM_CFG then vim.opt.runtimepath:prepend(vim.env.DECL_NVIM_CFG) end
vim.filetype.add({ extension = { decl = 'decl' } })
vim.treesitter.language.register('decl', 'decl')
vim.api.nvim_create_autocmd('FileType', {
  pattern = 'decl',
  callback = function(ev)
    vim.treesitter.start(ev.buf)
    vim.bo[ev.buf].commentstring = '// %s'
    vim.wo.foldmethod = 'expr'
    vim.wo.foldexpr = 'v:lua.vim.treesitter.foldexpr()'
    vim.wo.foldlevel = 99
    vim.lsp.start({
      name = 'decl',
      cmd = { vim.env.DECL_LSP or 'decl-lsp' },
      root_dir = vim.fs.root(ev.buf, { 'decl.toml', '.git' }) or vim.fn.getcwd(),
    }, { bufnr = ev.buf })
  end,
})
