" Decl filetype settings: the code style of AGENTS.md (4 spaces, no
" tabs, 100 columns) and `//` comments.
if exists('b:did_ftplugin') | finish | endif
let b:did_ftplugin = 1

setlocal commentstring=//\ %s
setlocal comments=s1:/*,mb:*,ex:*/,:///,://
setlocal expandtab shiftwidth=4 softtabstop=4 tabstop=4 textwidth=100
setlocal formatoptions-=t formatoptions+=croql

let b:undo_ftplugin = 'setlocal commentstring< comments< expandtab< shiftwidth< softtabstop< tabstop< textwidth< formatoptions<'
