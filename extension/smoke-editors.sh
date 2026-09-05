#!/usr/bin/env bash
# The editors without an extension (docs/tooling/04_extension.md §17):
# the configurations under extension/ (neovim, helix here; emacs, vim,
# sublime through their own smoke.sh), set up in a scratch directory,
# open fixtures and must show the highlighting, the diagnostics, and
# hover. Needs the editors, expect, the tree-sitter CLI (npx), and a
# built decl-lsp (DECL_LSP, default target/release/decl-lsp).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
lsp="${DECL_LSP:-$root/target/release/decl-lsp}"
work="${TMPDIR:-/tmp}/decl-editors"
rm -rf "$work"; mkdir -p "$work"
screen() { sed 's/\x1b\[[0-9;?:]*[a-zA-Z]//g; s/\x1b\][^\x07]*\x07//g; s/\x1b[()][A-Z0-9]//g' "$1" | tr '\r' '\n'; }

echo "== neovim: $(nvim --version | head -1)"
mkdir -p "$work/nvim/parser" "$work/nvim/queries/decl"
(cd "$root/tree-sitter-decl" && npx --yes tree-sitter build -o "$work/nvim/parser/decl.so" 2>/dev/null)
cp "$root"/tree-sitter-decl/queries/*.scm "$work/nvim/queries/decl/"
DECL_NVIM_CFG="$work/nvim" DECL_LSP="$lsp" DECL_ROOT="$root" \
  nvim --headless --noplugin -i NONE -u "$root/extension/neovim/init.lua" -c "luafile $root/extension/neovim/smoke.lua" 2>&1 | tee "$work/nvim.txt"
grep -q "FAILS" "$work/nvim.txt" && { echo "neovim: a query does not load"; exit 1; }
grep -q "highlight captures: keyword" "$work/nvim.txt" || { echo "neovim: no keyword highlight"; exit 1; }
grep -q "lines inside a fold: [1-9]" "$work/nvim.txt" || { echo "neovim: no fold"; exit 1; }
grep -q "diagnostics after the server answered: [1-9]" "$work/nvim.txt" || { echo "neovim: no diagnostics"; exit 1; }
grep -q "hover on LogLevel: .*type LogLevel" "$work/nvim.txt" || { echo "neovim: no hover"; exit 1; }

echo "== helix: $(hx --version)"
cfg="$work/helix"; mkdir -p "$cfg/helix/runtime/grammars" "$cfg/helix/runtime/queries/decl"
{ echo 'use-grammars = { only = ["decl"] }'; sed "s|DECL_ROOT|$root|g; s|command = \"decl-lsp\"|command = \"$lsp\"|" "$root/extension/helix/languages.toml"; } > "$cfg/helix/languages.toml"
printf '[editor]\nend-of-line-diagnostics = "hint"\n' > "$cfg/helix/config.toml"
cp "$root"/tree-sitter-decl/queries/{highlights,locals,textobjects}.scm "$cfg/helix/runtime/queries/decl/"
cp "$root/tree-sitter-decl/queries/helix/indents.scm" "$cfg/helix/runtime/queries/decl/"
XDG_CONFIG_HOME="$cfg" hx --grammar build 2>&1 | grep -v warning | tail -1
XDG_CONFIG_HOME="$cfg" hx --health decl | sed 's/\x1b\[[0-9;]*m//g' | tee "$work/hx-health.txt"
for want in "decl-lsp" "Tree-sitter parser: ✓" "Highlight queries: ✓" "Textobject queries: ✓" "Indent queries: ✓"; do grep -q "$want" "$work/hx-health.txt" || { echo "helix: health lacks '$want'"; exit 1; }; done
expect "$root/extension/helix/session.exp" "$cfg" "$root/tests/validation/constraints/invalid/assert_no_name.decl" "" "$work/hx-diag.log" >/dev/null
screen "$work/hx-diag.log" | grep -q "syntax error" || { echo "helix: the diagnostic is not on screen"; exit 1; }
echo "helix: diagnostic shown"
expect "$root/extension/helix/session.exp" "$cfg" "$root/docs/examples/02_config.decl" "/LogLevel\r k" "$work/hx-hover.log" >/dev/null
screen "$work/hx-hover.log" | grep -q 'type LogLevel = "debug"' || { echo "helix: no hover"; exit 1; }
echo "helix: hover shown"

# the editors whose checks live with their configuration
for editor in emacs vim sublime; do
  if [ -x "$root/extension/$editor/smoke.sh" ]; then DECL_LSP="$lsp" "$root/extension/$editor/smoke.sh"; fi
done
echo "editors: ok"
