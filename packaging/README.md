# Packaging and distribution

Decl ships under one user-visible name — the `decl` command — through
four channels. The package is `decl-lang` on every registry (the
project's name; `decl` itself is taken on npm and crates.io), the
import name in Python is `decl`, and the binary is `decl` everywhere.

| Channel | Package | Install | Status |
|---|---|---|---|
| GitHub release | `v0.3.0`: `decl` and `decl-lsp` for six platforms, the wheels, the `.vsix` | [releases/tag/v0.3.0](https://github.com/luuvish/decl-lang/releases/tag/v0.3.0) | **published 2026-09-05** by `release.yml` |
| npm | `decl-lang` | `npm install -g decl-lang` | **published 2026-09-05** (0.3.0; `decl-ts/`) |
| PyPI | `decl-lang` | `pip install decl-lang` / `pip install 'decl-lang[node]'` | prepared (`decl-py/`) |
| Homebrew | tap `luuvish/tap`, formula `decl-lang` | `brew install luuvish/tap/decl-lang` | **published 2026-09-05**: [luuvish/homebrew-tap](https://github.com/luuvish/homebrew-tap), the formula mirrored from `homebrew/` |
| crates.io | `decl-lang` (bin `decl`) | `cargo install decl-lang` | prepared (`decl-rs/`, native runtime) |
| Visual Studio Marketplace, Open VSX | `luuvish.vscode-decl` (the VS Code extension, bundling npm `decl-lang`) | Extensions view: "Decl" | packaged: the `.vsix` is on the v0.3.0 release (`extension/vscode/`, [docs/tooling/04_extension.md](../docs/tooling/04_extension.md); the marketplaces need `VSCE_PAT` / `OVSX_PAT` |
| Zed extension registry | `decl` (the Zed extension: grammar, queries, `decl-lsp` pointer) | Zed: extensions, "Decl" | packaged: the `decl-lsp` binaries are on the v0.3.0 release (`extension/zed/`, [docs/tooling/04_extension.md](../docs/tooling/04_extension.md); the registry and the download need a public repository |

npm and Homebrew ship **the same bytes**: `decl-ts/dist/` — the esbuild
bundles of the CLI, LSP server, and library (web-tree-sitter included,
zero runtime dependencies) plus the two wasm files. PyPI and crates.io
ship the native Python and Rust implementations of the same language. Every channel's smoke test drives the installed `decl`
binary the way a user would, and the native runtimes are held
byte-identical to the reference by `tests/parity/differential.py`
(`--rust` for the crate).

## PyPI — `decl-lang`

`decl-py/` is a fully native Python implementation: `decl.runtime` is
a pure-Python port of the whole language (checker, evaluator, packages,
formatter, language server) behind the console scripts `decl` /
`decl-lsp` and the API (`decl.check` / `decl.evaluate` /
`decl.validate` / `decl.format_source`), and `decl._tree_sitter`
compiles the grammar's C sources into a small extension module. No
Node.js is involved. Node comes from `$DECL_NODE`, the optional
`nodejs-wheel-binaries` dependency (`pip install 'decl-lang[node]'`), or
`npm run build` in `decl-ts/` copies the grammar sources into
`decl-py/decl/_tree_sitter/src/` (gitignored; `setup.py` copies them
itself from a fresh checkout).

```bash
npm run build -w decl-lang                     # refreshes the grammar sources
cd ../python
python -m build                                # platform wheel (C extension) + sdist
make -C ../.. parity                              # the three implementations byte-identical (tests/parity)
python scripts/e2e.py                          # the reference e2e scenarios on the native runtime
python scripts/smoke.py                        # install the wheel into a venv and drive it
python -m twine upload dist/*                  # publish (first time: create the PyPI project `decl-lang`)
```

The sdist needs a C compiler at install time; publish platform wheels
(e.g. via `cibuildwheel`) for users without one.

## crates.io — `decl-lang`

`decl-rs/` is the native Rust implementation: the grammar is compiled in by
`build.rs` (from `../tree-sitter-decl/src` inside the repository, or
from `grammar/` in the published crate — `npm run build` in `decl-ts/`
copies the sources there); the `decl` binary offers `check`, `evaluate`,
`validate`, and `fmt` with the reference CLI's exact output format, and
`decl-lsp` is the language server.

```bash
npm run build -w decl-lang                     # refreshes decl-rs/grammar and decl-rs/LICENSE
cd ../rust
cargo build --release                          # from the repository root (Cargo workspace)
./target/release/decl validate ../tests/validation
make -C ../.. parity                              # the three implementations byte-identical (tests/parity)
cargo publish --dry-run                        # then: cargo publish (first time: cargo login)
```

## npm — `decl-lang`

The package root is `decl-ts/`. `npm run build` bundles the CLI, the LSP
server, and the library entry with esbuild into `dist/` (ESM, Node 20+)
and ships the tree-sitter grammar wasm next to them; `web-tree-sitter`
is the only runtime dependency.

```bash
cd decl-ts
npm run build          # dist/cli.js, dist/lsp.js, dist/index.js, dist/tree-sitter-decl.wasm
npm test               # the ten suites
npm run smoke:dist     # npm pack -> install into a scratch project -> drive the installed binaries
npm publish            # runs prepublishOnly = build + test + smoke
```

Consumers get:

```bash
npm install -g decl-lang
decl check schema.decl
decl evaluate site.decl --output site
decl validate tests/validation
decl fmt --check src/*.decl
decl-lsp                # stdio language server for editors
```

and the library (`import { initParser, parseSource, checkModule, loadModules, runUniverse } from 'decl-lang'`; browsers import the platform-neutral `decl-lang/core` and pass the grammar wasm's URL to `initParser({ grammar })`).

## Homebrew — tap `luuvish/tap`

Homebrew core requires an established project (public repository,
tagged stable releases, a notability bar), so distribution starts from a
tap. The formula lives in `homebrew/Formula/decl-lang.rb`; a tap is simply a
GitHub repository named `homebrew-tap` holding that directory.

```bash
# one-time: create the tap repository
gh repo create luuvish/homebrew-tap --public --description "Homebrew tap for the decl language"
git clone git@github.com:luuvish/homebrew-tap.git
cp -r packaging/homebrew/Formula homebrew-tap/
# after `npm publish`, pin the tarball checksum:
shasum -a 256 decl-ts/decl-lang-0.2.0.tgz        # or: brew fetch --build-from-source ./Formula/decl-lang.rb
# edit sha256 in Formula/decl-lang.rb, then
cd homebrew-tap && brew install --build-from-source ./Formula/decl-lang.rb && brew test decl-lang
git add Formula && git commit -m "decl-lang 0.2.0" && git push
```

Users then run:

```bash
brew tap luuvish/tap
brew install decl-lang
```

The formula depends on `node` and installs the npm tarball under
`libexec` (`std_npm_args`), linking `decl` and `decl-lsp` into `bin` —
the same artifact npm users get, verified by the formula's `test` block.
Submitting to homebrew-core later keeps the formula name `decl-lang` (free
in core as of 2026-09-02).

## Release checklist

1. Bump `version` in `decl-ts/package.json`, `decl-py/pyproject.toml` and
   `decl-py/decl/__init__.py`, and `decl-rs/Cargo.toml` (spec version + patch).
2. `make verify` — every implementation's tests and the parity harness
   (`tests/parity/differential.py`): every line `same`.
3. `cd decl-ts && npm run smoke:dist`; `cd decl-py && python scripts/smoke.py`.
4. `npm publish` (first time: `npm login`; the name `decl-lang` is
   unclaimed as of 2026-09-02); `python -m twine upload dist/*`;
   `cargo publish -p decl-lang --locked --allow-dirty` from the root, after
   `npm run build -w decl-lang` (the crate packages the grammar sources and
   the LICENSE that build copies into `decl-rs/`; they are ignored files,
   hence `--allow-dirty`). `--dry-run` first: it packages and verifies
   the build.
5. Tag the repository: `git tag v0.3.0 && git push --tags` — the tag
   runs `.github/workflows/release.yml`, which attaches to the GitHub
   release:
   - `decl-<os>-<arch>` and `decl-lsp-<os>-<arch>` (`.exe` on Windows)
     for macOS, Linux, and Windows on arm64 and x86_64 — the Linux
     binaries built against **musl and statically linked** (no glibc
     dependency; the workflow refuses a dynamic one), the Zed
     extension's download; every binary evaluates an example against
     its golden on the platform it was built for;
   - the Python wheels for the same platforms (`cibuildwheel`: macOS
     arm64/x86_64, Linux manylinux and musllinux for aarch64/x86_64,
     Windows x86_64 and arm64) for every CPython `requires-python`
     admits (3.10–3.14; docs/DEVELOPMENT.md);
   - the VS Code `.vsix`, published to the Marketplace and Open VSX when
     `VSCE_PAT` / `OVSX_PAT` are configured.
6. Update `homebrew/Formula/decl-lang.rb` `url`/`sha256`, push to the tap.
