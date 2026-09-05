#!/usr/bin/env bash
# Emacs over the grammar and decl-lsp (docs/tooling/04_extension.md §17):
# builds the grammar library into a scratch directory, then runs
# smoke.el in batch Emacs — the mode, fontification, indentation, eglot,
# the diagnostics, hover. Needs emacs (29+ with tree-sitter), the
# tree-sitter CLI (npx), and a built decl-lsp (DECL_LSP, default
# target/release/decl-lsp).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
lsp="${DECL_LSP:-$root/target/release/decl-lsp}"
work="${TMPDIR:-/tmp}/decl-emacs"
rm -rf "$work"; mkdir -p "$work/tree-sitter"
echo "== emacs: $(emacs --version | head -1)"
case "$(uname -s)" in Darwin) ext=dylib ;; *) ext=so ;; esac
(cd "$root/tree-sitter-decl" && npx --yes tree-sitter build -o "$work/tree-sitter/libtree-sitter-decl.$ext" 2>/dev/null)
DECL_ROOT="$root" DECL_LSP="$lsp" DECL_WORK="$work" \
  emacs --batch -Q -l "$here/decl-mode.el" -l "$here/smoke.el" 2>"$work/stderr.txt" || { echo "== emacs stderr"; tail -20 "$work/stderr.txt"; exit 1; }
# a font-lock rule the grammar rejects is only a warning to Emacs (the feature is silently dropped): a failure here
if grep -q "treesit-font-lock-rules-mismatch\|obsolete" "$work/stderr.txt"; then echo "== emacs warnings"; grep -A6 "Warning" "$work/stderr.txt"; exit 1; fi
echo "emacs: ok"
