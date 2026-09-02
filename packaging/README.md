# Packaging and distribution

Decl ships under one user-visible name — the `decl` command — through
three channels. Registry package names differ where `decl` was already
taken (npm and crates.io hold unrelated `decl` packages; Homebrew core
does not), so the package is `decl-lang` and the binary is `decl`.

| Channel | Package | Install | Status |
|---|---|---|---|
| npm | `decl-lang` | `npm install -g decl-lang` | prepared (`impl/`) |
| PyPI | `decl` (the name is free there) | `pip install decl` / `pip install 'decl[node]'` | prepared (`python/`) |
| Homebrew | tap `luuvish/decl`, formula `decl` | `brew install luuvish/decl/decl` | prepared (`homebrew/`) |
| crates.io | `decl-lang` (bin `decl`) | `cargo install decl-lang` | reserved for the Rust runtime (ROADMAP Phase 5 decision pending) |

All channels ship **the same bytes**: `impl/dist/` — the esbuild bundles
of the CLI, LSP server, and library (web-tree-sitter included, zero
runtime dependencies) plus the two wasm files — and every channel's
smoke test drives the installed `decl` binary the way a user would.

## PyPI — `decl`

`python/` is a pure-Python package: console scripts `decl` / `decl-lsp`
that hand the process to the bundled JavaScript under Node.js ≥ 20, and
a small API (`decl.check`, `decl.evaluate`, `decl.validate`,
`decl.format_source`) over the CLI's `--json` reports. Node comes from
`$DECL_NODE`, the optional `nodejs-wheel-binaries` dependency
(`pip install 'decl[node]'`), or `PATH`. `npm run build` in `impl/`
mirrors `dist/` into `python/decl/_js/` (gitignored, included in the
wheel/sdist as build artifacts).

```bash
cd impl && npm run build                       # refreshes python/decl/_js
cd ../python
python -m build                                # dist/decl-0.2.0-py3-none-any.whl + sdist
python scripts/smoke.py                        # install the wheel into a venv and drive it
python -m twine upload dist/*                  # publish (first time: create the PyPI project `decl`)
```

## npm — `decl-lang`

The package root is `impl/`. `npm run build` bundles the CLI, the LSP
server, and the library entry with esbuild into `dist/` (ESM, Node 20+)
and ships the tree-sitter grammar wasm next to them; `web-tree-sitter`
is the only runtime dependency.

```bash
cd impl
npm run build          # dist/cli.js, dist/lsp.js, dist/index.js, dist/tree-sitter-decl.wasm
npm test               # the ten suites
npm run smoke:dist     # npm pack -> install into a scratch project -> drive the installed binaries
npm publish            # runs prepublishOnly = build + test + smoke
```

Consumers get:

```bash
npm install -g decl-lang
decl check schema.decl
decl evaluate site.decl --root site
decl validate tests/validation
decl fmt --check src/*.decl
decl-lsp                # stdio language server for editors
```

and the library (`import { initParser, parseSource, checkModule, loadModules, runUniverse } from 'decl-lang'`).

## Homebrew — tap `luuvish/decl`

Homebrew core requires an established project (public repository,
tagged stable releases, a notability bar), so distribution starts from a
tap. The formula lives in `homebrew/Formula/decl.rb`; a tap is simply a
GitHub repository named `homebrew-decl` holding that directory.

```bash
# one-time: create the tap repository
gh repo create luuvish/homebrew-decl --public --description "Homebrew tap for the decl language"
git clone git@github.com:luuvish/homebrew-decl.git
cp -r packaging/homebrew/Formula homebrew-decl/
# after `npm publish`, pin the tarball checksum:
shasum -a 256 impl/decl-lang-0.2.0.tgz        # or: brew fetch --build-from-source ./Formula/decl.rb
# edit sha256 in Formula/decl.rb, then
cd homebrew-decl && brew install --build-from-source ./Formula/decl.rb && brew test decl
git add Formula && git commit -m "decl 0.2.0" && git push
```

Users then run:

```bash
brew tap luuvish/decl
brew install decl
```

The formula depends on `node` and installs the npm tarball under
`libexec` (`std_npm_args`), linking `decl` and `decl-lsp` into `bin` —
the same artifact npm users get, verified by the formula's `test` block.
Submitting to homebrew-core later keeps the formula name `decl` (free
in core as of 2026-09-02).

## Release checklist

1. Bump `version` in `impl/package.json` (spec version + patch).
2. `cd impl && npm run build && npm test && npm run smoke:dist`.
3. `npm publish` (first time: `npm login`; the name `decl-lang` is
   unclaimed as of 2026-09-02).
4. Tag the repository: `git tag v0.2.0 && git push --tags`.
5. Update `homebrew/Formula/decl.rb` `url`/`sha256`, push to the tap.
