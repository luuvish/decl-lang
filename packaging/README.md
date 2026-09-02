# Packaging and distribution

Decl ships under one user-visible name — the `decl` command — through
four channels. Registry package names differ where `decl` was already
taken (npm and crates.io hold unrelated `decl` packages; Homebrew core
and PyPI do not), so the package is `decl-lang` there and the binary is
`decl` everywhere.

| Channel | Package | Install | Status |
|---|---|---|---|
| npm | `decl-lang` | `npm install -g decl-lang` | prepared (`typescript/`) |
| PyPI | `decl` (the name is free there) | `pip install decl` / `pip install 'decl[node]'` | prepared (`python/`) |
| Homebrew | tap `luuvish/decl`, formula `decl` | `brew install luuvish/decl/decl` | prepared (`homebrew/`) |
| crates.io | `decl-lang` (bin `decl`) | `cargo install decl-lang` | prepared (`rust/`, native runtime) |

npm and Homebrew ship **the same bytes**: `typescript/dist/` — the esbuild
bundles of the CLI, LSP server, and library (web-tree-sitter included,
zero runtime dependencies) plus the two wasm files. PyPI ships those
bytes too, alongside its native runtime; crates.io is the native Rust
runtime alone. Every channel's smoke test drives the installed `decl`
binary the way a user would, and the native runtimes are held
byte-identical to the reference by `python/scripts/differential.py`
(`--rust` for the crate).

## PyPI — `decl`

`python/` is a Python package with a native core: `decl.runtime` is a
pure-Python port of the evaluator (`decl evaluate` / `decl validate`,
`decl.evaluate` / `decl.validate`), and `decl._tree_sitter` compiles
the grammar's C sources into a small extension module. The console
scripts `decl` / `decl-lsp` hand every other command (`check`, `fmt`,
the language server) to the bundled JavaScript under Node.js ≥ 20; the
API's `decl.check` / `decl.format_source` do the same over the CLI's
`--json` reports. Node comes from `$DECL_NODE`, the optional
`nodejs-wheel-binaries` dependency (`pip install 'decl[node]'`), or
`PATH`. `npm run build` in `typescript/` mirrors `dist/` into
`python/decl/_js/` and the grammar sources into
`python/decl/_tree_sitter/src/` (both gitignored, included in the
wheel/sdist as build artifacts).

```bash
cd typescript && npm run build                       # refreshes python/decl/_js and the grammar sources
cd ../python
python -m build                                # platform wheel (C extension) + sdist
make -C .. parity                              # the three implementations byte-identical (tests/parity)
python scripts/e2e.py                          # the reference e2e scenarios on the native runtime
python scripts/smoke.py                        # install the wheel into a venv and drive it
python -m twine upload dist/*                  # publish (first time: create the PyPI project `decl`)
```

The sdist needs a C compiler at install time; publish platform wheels
(e.g. via `cibuildwheel`) for users without one.

## crates.io — `decl-lang`

`rust/` is the native Rust runtime: the grammar is compiled in by
`build.rs` (from `../tree-sitter-decl/src` inside the repository, or
from `grammar/` in the published crate — `npm run build` in `typescript/`
copies the sources there), and the `decl` binary offers `evaluate` and
`validate` with the reference CLI's exact output format.

```bash
cd typescript && npm run build                       # refreshes rust/grammar and rust/LICENSE
cd ../rust
cargo build --release
./target/release/decl validate ../tests/validation
make -C .. parity                              # the three implementations byte-identical (tests/parity)
cargo publish --dry-run                        # then: cargo publish (first time: cargo login)
```

## npm — `decl-lang`

The package root is `typescript/`. `npm run build` bundles the CLI, the LSP
server, and the library entry with esbuild into `dist/` (ESM, Node 20+)
and ships the tree-sitter grammar wasm next to them; `web-tree-sitter`
is the only runtime dependency.

```bash
cd typescript
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
shasum -a 256 typescript/decl-lang-0.2.0.tgz        # or: brew fetch --build-from-source ./Formula/decl.rb
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

1. Bump `version` in `typescript/package.json`, `python/pyproject.toml` and
   `python/decl/__init__.py`, and `rust/Cargo.toml` (spec version + patch).
2. `make verify` — every implementation's tests and the parity harness
   (`tests/parity/differential.py`): every line `same`.
3. `cd typescript && npm run smoke:dist`; `cd python && python scripts/smoke.py`.
4. `npm publish` (first time: `npm login`; the name `decl-lang` is
   unclaimed as of 2026-09-02); `python -m twine upload dist/*`;
   `cargo publish`.
5. Tag the repository: `git tag v0.2.0 && git push --tags`.
6. Update `homebrew/Formula/decl.rb` `url`/`sha256`, push to the tap.
