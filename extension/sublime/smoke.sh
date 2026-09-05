#!/usr/bin/env bash
# The Sublime Text syntax, checked headlessly: syntect (the Rust engine
# that implements the Sublime syntax format) loads Decl.sublime-syntax
# and runs syntax_test_decl.decl against it, as Sublime's own "Syntax
# Tests" build does. Needs git and cargo; the clone and the build are
# cached under the scratch directory.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="${TMPDIR:-/tmp}/decl-sublime"
export PATH="$HOME/.cargo/bin:$PATH"
mkdir -p "$work"
if [ ! -d "$work/syntect" ]; then
  echo "sublime: cloning syntect"
  git clone --depth 1 -q https://github.com/trishume/syntect "$work/syntect"
fi
syntest="$work/syntect/target/release/examples/syntest"
if [ ! -x "$syntest" ]; then
  echo "sublime: building syntect's syntax-test runner"
  (cd "$work/syntect" && cargo build --release --example syntest -q)
fi
echo "sublime: syntect $(cd "$work/syntect" && git log -1 --format=%h) over $here/Decl.sublime-syntax"
"$syntest" "$here/syntax_test_decl.decl" "$here" | tee "$work/syntest.txt" | grep -vE "^loading|^Testing|^The test"
grep -q "^Ok(Success" "$work/syntest.txt" || { echo "sublime: syntax tests failed"; exit 1; }
for f in Decl.sublime-settings LSP.sublime-settings; do
  python3 - "$here/$f" <<'PY' || { echo "sublime: $f is not valid JSON (comments and trailing commas aside)"; exit 1; }
import json, re, sys
text = open(sys.argv[1]).read()
text = re.sub(r"//[^\n]*", "", text)
text = re.sub(r",(\s*[}\]])", r"\1", text)
json.loads(text)
PY
done
echo "sublime: settings parse"
echo "sublime: ok"
