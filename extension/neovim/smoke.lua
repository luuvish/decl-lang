-- The headless smoke (extension/smoke-editors.sh): every query loads,
-- the highlights query captures a keyword, the folds query folds, the
-- server attaches and reports the fixture's diagnostic, and hover on a
-- type name answers with its declaration. DECL_ROOT is the repository.
local root = vim.env.DECL_ROOT
local out = {}
local function say(s) table.insert(out, s) end
for _, q in ipairs({ 'highlights', 'locals', 'folds', 'indents', 'textobjects' }) do
  local ok, err = pcall(vim.treesitter.query.get, 'decl', q)
  say(string.format('query %-12s %s', q, ok and 'loads' or ('FAILS: ' .. tostring(err))))
end
local file = root .. '/tests/validation/declarations/invalid/output_type_mismatch.decl'
if vim.fn.filereadable(file) == 0 then file = vim.fn.glob(root .. '/tests/validation/*/invalid/*.decl', false, true)[1] end
vim.cmd('edit ' .. file)
local buf = vim.api.nvim_get_current_buf()
say('file ' .. file:sub(#root + 2) .. ' filetype=' .. vim.bo[buf].filetype)
-- the first keyword on the first non-comment line must be highlighted as one
local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
for i, line in ipairs(lines) do
  local col = line:find('%a')
  if col and not line:match('^%s*//') then
    local names = {}
    local parser = vim.treesitter.get_parser(buf, 'decl')
    local tree = parser:parse()[1]
    local query = vim.treesitter.query.get('decl', 'highlights')
    for id, node in query:iter_captures(tree:root(), buf, i - 1, i) do
      local sr, sc = node:start()
      if sr == i - 1 and sc == col - 1 then table.insert(names, query.captures[id]) end
    end
    say(string.format('line %d %q -> highlight captures: %s (highlighter attached: %s)', i, line:sub(col, col + 5), table.concat(names, ','), tostring(vim.treesitter.highlighter.active[buf] ~= nil)))
    break
  end
end
-- folds from the query
local folded = 0
for i = 1, #lines do if vim.fn.foldlevel(i) > 0 then folded = folded + 1 end end
say('lines inside a fold: ' .. folded)
-- the language server: diagnostics, then hover on the first declaration name
local got = vim.wait(15000, function() return #vim.diagnostic.get(buf) > 0 end, 100)
local diags = vim.diagnostic.get(buf)
say(string.format('diagnostics after %s: %d', got and 'the server answered' or 'timeout', #diags))
for _, d in ipairs(diags) do say(string.format('  %d:%d [%s] %s', d.lnum + 1, d.col + 1, tostring(d.code), d.message)) end
local clients = vim.lsp.get_clients({ bufnr = buf })
say('lsp clients: ' .. #clients .. (clients[1] and (' (' .. clients[1].name .. ')') or ''))
vim.cmd('edit ' .. root .. '/docs/examples/02_config.decl')
buf = vim.api.nvim_get_current_buf()
lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
vim.wait(3000, function() return #vim.lsp.get_clients({ bufnr = buf }) > 0 end, 100)
clients = vim.lsp.get_clients({ bufnr = buf })
say('clean example: lsp clients ' .. #clients)
if clients[1] then
  for i, line in ipairs(lines) do
    local name = line:gsub('^%s*export%s+', ''):match('^%s*%a+%s+([%a_][%w_]*)')
    if name and not line:match('^%s*//') then
      local col = line:find(name, 1, true) - 1
      local res = vim.lsp.buf_request_sync(buf, 'textDocument/hover', { textDocument = vim.lsp.util.make_text_document_params(buf), position = { line = i - 1, character = col } }, 5000)
      local r = res and res[clients[1].id] and res[clients[1].id].result
      local text = r and r.contents and (type(r.contents) == 'table' and (r.contents.value or r.contents[1]) or r.contents) or nil
      say(string.format('hover on %s: %s', name, text and (text:gsub('\n', ' | '):sub(1, 120)) or 'nothing'))
      break
    end
  end
end
io.stdout:write(table.concat(out, '\n') .. '\n')
vim.cmd('qa!')
