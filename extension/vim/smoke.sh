#!/usr/bin/env bash
# Vim (9.1+) over extension/vim and decl-lsp through the yegappan/lsp
# plugin (docs/tooling/04_extension.md §17): the syntax file names the
# groups a Decl file needs; the server's diagnostics reach the editor's
# location list; and a hover on a symbol round-trips through the plugin
# (the plugin issues textDocument/hover, the server returns the symbol's
# declaration). Needs vim, expect, git, and a built decl-lsp (DECL_LSP,
# default target/release/decl-lsp). Scratch: ${TMPDIR:-/tmp}/decl-vim.
#
# Hover's UI (a popup) does not render reliably under a headless pty, so
# the hover check reads the protocol through a tee wrapper rather than the
# screen; diagnostics are checked on the editor surface (the location list).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
lsp="${DECL_LSP:-$root/target/release/decl-lsp}"
work="${TMPDIR:-/tmp}/decl-vim"
mkdir -p "$work"
fail() { echo "vim: $1"; exit 1; }

echo "== vim: $(vim --version | head -1)"
[ -x "$lsp" ] || fail "no decl-lsp at $lsp (build it or set DECL_LSP)"

# (a) the syntax file, in Ex mode: the filetype, the group at a comment, a
#     keyword, a type, a string, a member of docs/examples/02_config.decl,
#     and the ftplugin's commentstring and shiftwidth
cat > "$work/syntax.vim" <<VIM
set nocompatible
set runtimepath^=$here
filetype plugin indent on
syntax on
edit $root/docs/examples/02_config.decl
call writefile([&filetype, synIDattr(synID(1,1,1),'name'), synIDattr(synID(5,1,1),'name'), synIDattr(synID(5,13,1),'name'), synIDattr(synID(5,24,1),'name'), synIDattr(synID(8,5,1),'name'), &commentstring, &shiftwidth], '$work/syntax.txt')
qa!
VIM
vim -es -u NONE -S "$work/syntax.vim" || true
got=(); while IFS= read -r line; do got+=("$line"); done < "$work/syntax.txt"   # (bash 3.2 has no mapfile)
want=(decl declLineComment declKeyword declType declString declProperty "// %s" 4)
for i in "${!want[@]}"; do
  [ "${got[$i]:-}" = "${want[$i]}" ] || fail "syntax: expected '${want[$i]}' at position $i, got '${got[$i]:-}'"
done
echo "vim: syntax groups ok (comment, keyword, type, string, member; commentstring, shiftwidth)"

# the language server through yegappan/lsp; log its traffic through a tee
[ -d "$work/pack/decl/start/lsp" ] || git clone --depth 1 -q https://github.com/yegappan/lsp "$work/pack/decl/start/lsp"
cat > "$work/tee-lsp.sh" <<TEE
#!/bin/sh
exec tee "$work/in.log" | "$lsp" | tee "$work/out.log"
TEE
chmod +x "$work/tee-lsp.sh"

setup() {   # $1 = the decl_out path for the vimrc
  cat <<VIM
set nocompatible
set runtimepath^=$here
set packpath^=$work
set noswapfile
filetype plugin indent on
syntax on
let g:decl_out = '$1'
let s:server = [{'name': 'decl', 'filetype': 'decl', 'path': '$work/tee-lsp.sh', 'args': []}]
autocmd User LspSetup call LspOptionsSet({'hoverOnCursorHold': v:false})
autocmd User LspSetup call LspAddServer(s:server)
VIM
}

# (b) diagnostics on the editor surface: LspDiagsUpdated fires when the
#     publishDiagnostics notification is stored; fill the location list and
#     write its messages (event-driven, so the read never races the server)
{ setup "$work/diag.txt"; cat <<'VIM'
function! s:DumpDiags() abort
  silent! LspDiag show
  call writefile(map(getloclist(0), {_, v -> get(v, 'text', '')}), g:decl_out)
endfunction
autocmd User LspDiagsUpdated call s:DumpDiags()
VIM
} > "$work/diag.vimrc"
rm -f "$work/diag.txt" "$work/in.log" "$work/out.log"
expect "$here/session.exp" "$work/diag.vimrc" "$root/tests/validation/constraints/invalid/assert_no_name.decl" "$work/diag.log" >/dev/null
grep -q "syntax error" "$work/diag.txt" 2>/dev/null || fail "the diagnostic did not reach the location list (screen: $work/diag.log)"
echo "vim: diagnostic in the location list (LspDiag show): $(head -1 "$work/diag.txt")"

# (c) hover round-trip: put the cursor on LogLevel and request hover; the
#     protocol log must carry the plugin's textDocument/hover and the
#     server's answer (the type's declaration)
setup "$work/hover.txt" > "$work/hover.vimrc"
rm -f "$work/in.log" "$work/out.log"
expect "$here/session.exp" "$work/hover.vimrc" "$root/docs/examples/02_config.decl" "$work/hover.log" '/LogLevel
' ':LspHover
' >/dev/null
tr -d '\r' < "$work/in.log" | grep -q '"textDocument/hover"' || fail "the plugin sent no textDocument/hover (screen: $work/hover.log)"
tr -d '\r' < "$work/out.log" | grep -q 'export type LogLevel = ' || fail "the server's hover answer did not come back (protocol: $work/out.log)"
shown="$(python3 -c "import json,re,sys; d=open(sys.argv[1]).read(); m=re.search(r'\"value\":(\"(?:[^\"\\\\]|\\\\.)*\")', d); print(json.loads(m.group(1)).strip().splitlines()[1] if m else '')" "$work/out.log")"
echo "vim: hover round-trip (LspHover -> textDocument/hover -> $shown)"
echo "vim: ok"
