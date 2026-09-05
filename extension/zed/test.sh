#!/usr/bin/env bash
# The Zed extension's tests (docs/tooling/04_extension.md §20): the wasm
# module builds for Zed's target, the manifest names what Zed needs, and
# every query file parses against the tree-sitter grammar and matches on
# the fixture corpus (an unknown node or field is a query error).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
cd "$here"

echo "== the wasm module"
rustup target list --installed | grep -q wasm32-wasip1 || rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1 --quiet
test -f target/wasm32-wasip1/release/zed_decl.wasm

echo "== the manifest"
grep -q '^id = "decl"' extension.toml
grep -q '^\[grammars.decl\]' extension.toml
grep -q '^path = "tree-sitter-decl"' extension.toml
grep -q '^\[language_servers.decl-lsp\]' extension.toml
grep -q '^name = "Decl"' languages/decl/config.toml

echo "== the queries against the grammar"
# the grammar's queries/ are the source: Zed's highlights are that file verbatim
cmp -s "$root/tree-sitter-decl/queries/highlights.scm" "$here/languages/decl/highlights.scm" || { echo "languages/decl/highlights.scm differs from tree-sitter-decl/queries/highlights.scm"; exit 1; }
cd "$root/tree-sitter-decl"
# the grammar's own parser (tree-sitter builds it for the query check)
npx --yes tree-sitter build >/dev/null 2>&1 || npx tree-sitter generate >/dev/null
fixtures=$(ls "$root"/tests/validation/*/valid/*.decl | head -40)
check_query() {
  # a query that names an unknown node or field fails to load; one that loads is run over the fixtures
  # (the CLI warns about its global parser config; the grammar in this directory is the one it uses)
  if ! npx tree-sitter query "$1" $fixtures >/dev/null 2>/tmp/decl-query.err; then grep -v "parser directories\|init-config\|configuration file" /tmp/decl-query.err; echo "query $1 failed"; exit 1; fi
  echo "ok   ${1#$root/}"
}
# the grammar's queries (Neovim, Helix, and every tree-sitter editor), Helix's indent dialect, then Zed's
for f in "$root"/tree-sitter-decl/queries/*.scm "$root"/tree-sitter-decl/queries/helix/*.scm "$here"/languages/decl/*.scm; do check_query "$f"; done
echo "zed extension: ok"
