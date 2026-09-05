" Decl indentation: a line that opens a bracket indents the next; a line
" that starts with a closer sits back on the opener's column.
if exists('b:did_indent') | finish | endif
let b:did_indent = 1

setlocal indentexpr=GetDeclIndent()
setlocal indentkeys=0{,0},0],0),!^F,o,O
let b:undo_indent = 'setlocal indentexpr< indentkeys<'

if exists('*GetDeclIndent') | finish | endif

function GetDeclIndent() abort
  let lnum = prevnonblank(v:lnum - 1)
  if lnum == 0 | return 0 | endif
  let ind = indent(lnum)
  let prev = substitute(getline(lnum), '//.*$', '', '')
  let opens = len(substitute(prev, '[^{[(]', '', 'g'))
  let closes = len(substitute(prev, '[^}\])]', '', 'g'))
  if opens > closes | let ind += shiftwidth() | endif
  if getline(v:lnum) =~# '^\s*[}\])]' | let ind -= shiftwidth() | endif
  return ind < 0 ? 0 : ind
endfunction
