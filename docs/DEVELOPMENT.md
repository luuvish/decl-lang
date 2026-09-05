# Development handbook

How the repository is set up and worked on: the
layout and its configuration files, the toolchains and their versions,
getting a machine ready, building and testing, the quality tools,
continuous integration, releases, editors, and conventions. The rules
for agents are in `AGENTS.md`; the language itself is
`docs/specification/`; the plan is `ROADMAP.md`.

## 1. Layout and configuration files

One repository, three implementations of one language, one grammar, the
editors, the website, and the packaging — side by side at the top level:

| Path | What it is | Its configuration |
|---|---|---|
| `decl-ts/` | the TypeScript reference implementation, npm `decl-lang` | `package.json` (scripts, `engines`), `tsconfig.json` (type-check only; the sources run through Node's type stripping and esbuild bundles `dist/`), `scripts/build.mjs` |
| `decl-rs/` | the Rust implementation, crates.io `decl-lang` (`decl`, `decl-lsp`) | `Cargo.toml` (`rust-version`, `[lints] workspace = true`), `build.rs` (compiles the grammar's `parser.c`) |
| `decl-py/` | the Python implementation, PyPI `decl-lang` | `pyproject.toml` (package, `requires-python`, the `dev` extra, `[tool.ruff]`, `[tool.mypy]`, `[tool.pytest.ini_options]`), `setup.py` (the C extension around the grammar) |
| `tree-sitter-decl/` | the grammar all three use | `grammar.js`, `tree-sitter.json`, generated `src/` (committed), `queries/` (every editor's), the wasm (committed) |
| `extension/vscode/`, `extension/zed/` | the editor extensions | `package.json` (`engines.vscode`), `extension.toml` (grammar pin, server) |
| `extension/neovim/`, `helix/`, `emacs/`, `vim/`, `sublime/` | configurations for editors that need no extension, with their smoke checks | `extension/smoke-editors.sh` |
| `tests/` | the shared corpora and the parity harness (`tests/README.md`) | `tests/golden/manifest.json`, `tests/subsume/cases.txt`, `tests/repl/*/` |
| `site/` | the website (Astro Starlight), generated from `docs/` | `site/package.json`, `astro.config.mjs` |
| `packaging/` | the distribution channels and the Homebrew formula (`packaging/README.md`) | `homebrew/Formula/decl-lang.rb` |
| `docs/` | design, the frozen specification, the guide, the tooling documents, this handbook | `docs/README.md` is the index |

The files at the root that configure the whole:

| File | Configures |
|---|---|
| `mise.toml` | the pinned toolchains: Node, Python, Rust (§2); `mise install` |
| `rust-toolchain.toml` | the Rust pin for rustup itself (mirrors `mise.toml`; CI checks they agree) |
| `Cargo.toml`, `Cargo.lock` | the Cargo workspace (`decl-rs`; `extension/zed` excluded, with its own lock) and the workspace clippy lints, each allow with its reason |
| `rustfmt.toml` | rustfmt: edition 2021, width 100 |
| `package.json`, `package-lock.json` | the npm workspace (`decl-ts`, `tree-sitter-decl`, `site`, `extension/vscode`), one lockfile, the root scripts `typecheck`, `lint`, `lint:fix`, `format`, `format:check` |
| `eslint.config.mjs`, `.prettierrc`, `.prettierignore` | ESLint (flat config) and Prettier over the TypeScript packages; what each ignores and why |
| `Makefile` | the gate and every make target (§4) |
| `.github/workflows/*.yml`, `.github/dependabot.yml` | CI (§6) and the monthly dependency updates |
| `.gitignore` | build outputs, synced grammar copies, environments, caches, editor build artifacts (`extension/zed/extension.wasm`, `.vsix`), the local Zed settings |
| `AGENTS.md` (`CLAUDE.md` points at it) | the working rules for agents and people |

## 2. Toolchains and versions

### Version policy

Two different questions get two different answers:

- **What a user needs** is a *minimum*, declared in the manifest, kept
  conservative, and verified with exactly that version: `rust-version`
  in `decl-rs/Cargo.toml` (users of the crate), `engines.node` in
  `decl-ts/package.json` (users of the npm package), `requires-python`
  in `decl-py/pyproject.toml` (users of the wheels), `engines.vscode`
  in the extension. The `minimums` job of `.github/workflows/verify.yml`
  builds and tests with the minimum of each. A minimum moves only for a
  reason — the version reached end of life upstream, or a dependency
  needs more — and the move is a visible change, noted in the release.
  The floors: a supported upstream release (Node 20 and Python 3.9 are
  end of life, so Node ≥ 22 and Python ≥ 3.10); the Rust floor is what
  the dependencies require (`tree-sitter-language` needs 1.90).
- **What the repository builds with** is a *pin*: one exact toolchain
  version per language, in a file the tools read themselves, moved
  forward deliberately (a pull request that passes the gate), and
  identical on every developer machine and in CI, so a release is
  reproducible: **`mise.toml`** — `mise install` puts the pinned Node,
  Python, and Rust (through rustup) on a developer machine, and CI reads
  the same file (`verify.yml` installs with `jdx/mise-action`; the other
  workflows parse the pins into `setup-node`, `setup-python`, and
  `dtolnay/rust-toolchain`). `rust-toolchain.toml` mirrors the Rust pin
  for cargo without mise, and CI fails when the two disagree.
  Dependencies are pinned by the lockfiles
  (`package-lock.json`, `Cargo.lock`, `extension/zed/Cargo.lock`),
  installed with `npm ci` and `cargo --locked`; the Python package has
  one runtime dependency with a floor (`tree-sitter>=0.25`, the
  installed version is what the wheels are tested with).
- **Latest is neither.** The pin is a recent stable, not "whatever is
  current": Dependabot (`.github/dependabot.yml`) opens, per ecosystem
  per month, one pull request of minor and patch updates and one of
  majors, and the gate decides. A release is built from the pins; it
  *supports* the minimums.

Where each version lives:

| Question | Rust | Node | Python | Others |
|---|---|---|---|---|
| minimum a user needs | `rust-version` in `decl-rs/Cargo.toml` (1.90) | `engines.node` in `decl-ts/package.json` (≥ 22) | `requires-python` in `decl-py/pyproject.toml` (≥ 3.10) | `engines.vscode` (≥ 1.90) in `extension/vscode/package.json`; editors: §Editors below |
| the pin the repository builds with | `mise.toml` (1.97.1; mirrored by `rust-toolchain.toml`) | `mise.toml` (24) | `mise.toml` (3.12) | — |
| dependencies | `Cargo.lock`, `extension/zed/Cargo.lock` | `package-lock.json` | `tree-sitter>=0.25`; build: `setuptools>=77` | GitHub Actions pinned to a major (`@v4`) |
| the release builds with | the pin, `--locked`, six targets | the pin | the pin runs `cibuildwheel`; wheels for every CPython the minimum admits (3.10–3.14) | `dtolnay/rust-toolchain@master` with the pinned channel |
| checked at the minimum | `minimums` job: `cargo check --all-targets` on 1.90 | `minimums` job: the reference's tests on Node 22 | `minimums` job: `make test-python` on 3.10 | — |

The "required" column below is the minimum or the pin as declared;
the "used here" column is the development machine. `make verify` is
the gate on both.

### Platform

| | Used here |
|---|---|
| OS | macOS 26.6.2, arm64 (Apple Silicon) |
| C toolchain | Apple clang 21.0.0 (Xcode command line tools) |
| Package manager | Homebrew 6.0 |
| git | 2.55 |
| CI | GitHub-hosted runners: `ubuntu-latest`, `ubuntu-24.04-arm`, `macos-13`, `macos-latest`, `windows-latest`, `windows-11-arm` |

### Language toolchains

| Toolchain | Required | Used here | CI |
|---|---|---|---|
| Rust (`decl-rs`, the Zed extension) | minimum 1.90 (`rust-version`), pin 1.97.1 (`mise.toml`, mirrored by `rust-toolchain.toml`), edition 2021 | rustc 1.97.1, cargo 1.97.1, rustup 1.29 | the pin through `dtolnay/rust-toolchain@master`, `Swatinem/rust-cache@v2`; `minimums` on 1.90 |
| Rust targets | the six release targets and `wasm32-wasip1` (Zed) | `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`, `aarch64-pc-windows-msvc`, `x86_64-pc-windows-msvc`, `wasm32-wasip1` (+ gnu and wasm32-unknown-unknown / wasip2, unused) | one target per matrix entry; `musl-tools` on Linux |
| Node (`decl-ts`, the grammar, the site, the VS Code extension) | minimum Node ≥ 22 (`engines`), pin 24 (`mise.toml`); the reference runs its `.ts` sources with Node's type stripping (22.18+), so there is no TypeScript compiler | Node 24.20.0, npm 11.19 | `mise-action` (verify) or `actions/setup-node@v4` with the pin; `minimums` on 22 |
| Python (`decl-py`) | minimum Python ≥ 3.10 (`requires-python`), pin 3.12 (`mise.toml`) | Python 3.14.7 (Homebrew) in `decl-py/.venv`, pip 26.2, setuptools 84 | `mise-action` (verify) or `actions/setup-python@v5` with the pin; `minimums` on 3.10; wheels for CPython 3.10–3.14 |

### Direct dependencies

| Component | Dependency | Version |
|---|---|---|
| `decl-rs` | `tree-sitter` (the runtime; the grammar's `parser.c` is compiled in by `build.rs`) | 0.25 (lock: 0.25.10) |
| | `tree-sitter-language` | 0.1 |
| | `num-bigint` | 0.4 |
| | `rustyline` (the REPL's line editor) | 15 (lock: 15.0.0) |
| `extension/zed` | `zed_extension_api` | 0.6.0 |
| `decl-ts` | no runtime dependency; dev: `esbuild` (the bundles), `web-tree-sitter` (the grammar's wasm runtime) | 0.25.12, 0.25.10 |
| `tree-sitter-decl` | dev: `tree-sitter-cli` (`generate`, `build`, `test`, `query`) | 0.25.10 |
| `extension/vscode` | `vscode-languageclient`; VS Code engine `^1.90.0`; dev: `@types/vscode`, `mocha`, `@vscode/test-electron`, `@vscode/test-web`, `esbuild` | 9.0.1; 1.136.0, 10.8.2, 3.1.0, 0.0.81 |
| `site` | `astro`, `@astrojs/starlight`, `codemirror` (the playground editor) | 7.2.10, 0.41.11, 6.0.2 |
| `decl-py` | `tree-sitter` (the runtime; the grammar is a small C extension, `binding.c` + `parser.c` + `scanner.c`, exposing the language to it); build: `setuptools ≥ 77`, `wheel` | ≥ 0.25 (installed: 0.26.0) |
| packaging | `cibuildwheel` (CI only), `@vscode/vsce` and `ovsx` (through `npx`) | latest at run time |

One lockfile per ecosystem: the root `package-lock.json` (npm workspace), the root `Cargo.lock` (Cargo workspace) and `extension/zed/Cargo.lock` (the extension is excluded from the workspace), none for Python (no pinned runtime dependencies).

## 3. Getting started

What a development machine needs, in the order to install it. The
toolchains themselves (Node, Python, Rust) come from `mise`; the rest
is the platform's.

**1. The platform's tools** — git, GNU make, and a C compiler (the
grammar is C: the Python extension compiles it, and so do cargo's
build script and the tree-sitter CLI).

| Platform | Install |
|---|---|
| macOS | `xcode-select --install` (git, make, clang), then [Homebrew](https://brew.sh) for the rest |
| Debian / Ubuntu | `sudo apt install git build-essential curl` |
| Fedora | `sudo dnf install git make gcc` |
| Windows | Git for Windows (its bash runs the scripts), Visual Studio Build Tools with the C++ workload (MSVC, the compiler cargo and pip use), `make` from `winget install GnuWin32.Make` or the MSYS2 shell — the release builds run on GitHub's Windows runners; local development on Windows is possible but not what the maintainers use |

**2. mise** — one tool that installs the pinned toolchains:
[mise.jdx.dev](https://mise.jdx.dev/getting-started.html).

```sh
brew install mise                         # macOS
curl https://mise.run | sh                # Linux (and macOS without Homebrew)
winget install jdx.mise                   # Windows
echo 'eval "$(mise activate zsh)"' >> ~/.zshrc     # or bash / fish; makes the pins active in every shell
```

Then, from the repository root:

```sh
mise trust && mise install     # Node 24, Python 3.12, Rust 1.97.1 (through rustup) — from mise.toml
rustup target add wasm32-wasip1          # the Zed extension's target
```

Without mise, install the same versions any other way (`nvm`, `pyenv`,
`rustup`); `rust-toolchain.toml` makes rustup pick the Rust pin by
itself, and CI's `minimums` job shows the oldest versions that work.

**3. Build and verify**

```sh
npm ci                            # the npm workspace (decl-ts, the grammar, the site, the VS Code extension), once, at the root
cargo build --release --locked    # decl and decl-lsp in target/release
make verify                       # creates decl-py/.venv, compiles the grammar into it, runs every suite and the parity harness (~6 min)
make lint                         # the type checkers, linters, and formatters in check mode
```

`make verify` is the gate before every commit; `make lint` is what
CI runs beside it.

**4. Optional tools** — only for the checks done by hand:

| Tool | Needed for | Install |
|---|---|---|
| `expect` | the editor smoke on a pseudo-terminal (`extension/smoke-editors.sh`), REPL sessions in the harness | `brew install expect` / `apt install expect` |
| the editors | the same smoke: Neovim, Helix, Emacs, Vim, Sublime Text (§Editors) | `brew install neovim helix emacs`, `brew install --cask sublime-text` |
| Docker | confirming a musl binary is static, in an Alpine container | Docker Desktop |
| VS Code, Zed | running the extensions from the tree (docs/tooling/04_extension.md) | the apps |
| Playwright's Chromium | the web extension suite (`npm run test:web -w vscode-decl`) | `node node_modules/playwright/cli.js install chromium`; when Playwright's CDN times out, the same build from Google's Chrome for Testing bucket, unpacked into `~/Library/Caches/ms-playwright/` |

The tree-sitter CLI comes with `npm ci` (`npx tree-sitter`); the
Python venv, its tools, and the compiled grammar are made by `make`.

## 4. Building and testing

`make` at the root drives everything; the targets, from `Makefile`:

| Target | Runs |
|---|---|
| `make verify` | **the gate**, before every commit: `test-typescript`, `test-rust`, `test-python`, then `parity` |
| `make test-typescript` | `npm test -w decl-lang`: the corpus drivers under `decl-ts/test/` and `src/conformance.ts` through `node --test`, sequentially |
| `make test-rust` | `cargo test --locked --release`, then `decl validate tests/validation` |
| `make test-python` | `scripts/e2e.py`, `decl validate tests/validation`, `pytest` — in `decl-py/.venv`, which `make` creates with the package, its `dev` extra, and the compiled grammar |
| `make parity` | `tests/parity/differential.py`: every command line, REPL session, and language-server request of the reference against the Rust and Python implementations, byte for byte (exit code, stdout, stderr, answers), and the goldens |
| `make lint`, `make format` | §5, per language or all (`lint-<lang>`, `format-<lang>`) |
| `make site` | the website: docs synced, the playground bundled, rendered |
| `make clean`, `make distclean` | build outputs; environments too |

The rules behind the targets (`AGENTS.md`): every behavior change lands
in all three implementations in one change, the TypeScript reference
first; a parity difference is a defect in whichever implementation
diverges from the specification, never a reason to change the
expectation; tests are shared data under `tests/` (`validation/`,
`modules/`, `packages/`, `subsume/`, `golden/`, `repl/`) that all three
run — a per-language test only drives those corpora or covers a surface
that exists in that language alone.

The editors are checked by hand before a release:
`extension/smoke-editors.sh` (Neovim, Helix, Emacs, Vim, Sublime Text)
and the extensions' own tests (`npm test -w vscode-decl`,
`extension/zed/test.sh`).

## 5. Quality tools

`make lint` runs each language's tools in check mode; `make format`
rewrites the sources; CI's `lint` job runs the former. Every rule that
is switched off is switched off in the configuration file with a
one-line reason, never inline.

| Language | Type checker | Linter | Formatter | Tests | Configuration |
|---|---|---|---|---|---|
| TypeScript (`decl-ts`, `extension/vscode`) | `tsc --noEmit` (`typecheck`) | ESLint 10, `typescript-eslint` recommended, type-checked (`lint`) | Prettier (`format`, `format:check`; single quotes, width 100) | `node --test`, one test per corpus driver, sequential (`npm test -w decl-lang`) | `decl-ts/tsconfig.json`, `extension/vscode/tsconfig.json`, `eslint.config.mjs`, `.prettierrc`, `.prettierignore` |
| Rust (`decl-rs`, `extension/zed`) | the compiler, warnings denied | clippy, `-D warnings`, all targets | rustfmt (`max_width = 100`) | `cargo test --locked --release`; `cargo doc` warning-free | `rustfmt.toml`, `[workspace.lints]` in `Cargo.toml` |
| Python (`decl-py`) | mypy `strict` (`warn_return_any` off: the AST and values are `dict[str, Any]`) | ruff (`E F W I UP B SIM RUF`, line 100, py310) | ruff format | pytest (`tests/`: the corpus drivers as subprocesses, the API directly); the drivers themselves (`scripts/e2e.py`) | `[tool.ruff]`, `[tool.mypy]`, `[tool.pytest.ini_options]` in `pyproject.toml`; `dev` extra |

## 6. Continuous integration

Four workflows under `.github/workflows/`, on GitHub-hosted runners;
each installs the pinned toolchains (§2) — `verify.yml` through
`jdx/mise-action` from `mise.toml`, exactly as a developer does, the
others by reading the same pins into `actions/setup-node`,
`actions/setup-python`, and `dtolnay/rust-toolchain`.

| Workflow | When | Jobs |
|---|---|---|
| `verify.yml` | every push to `main`, every pull request | `verify`: `make verify`, and that `rust-toolchain.toml` agrees with `mise.toml` · `lint`: `make lint` · `minimums`: `cargo check` on Rust 1.90, the reference's tests on Node 22, `make test-python` on Python 3.10 — the minimums the manifests declare, each with exactly that version |
| `extension.yml` | pushes touching `extension/`, `decl-ts/`, the grammar | `vscode`: build and the desktop tests in a downloaded VS Code · `web`: the browser suite with Playwright's Chromium · `zed`: `extension/zed/test.sh` (wasm build, manifest, every query over the fixtures) · `vscode-rust`: the desktop tests against the Rust server, on dispatch |
| `site.yml` | pushes touching `docs/`, `examples/`, `site/`, the reference, the READMEs | `build`: the site from the docs and the playground bundle · `deploy`: GitHub Pages |
| `release.yml` | a `v*` tag (or dispatch, which builds without publishing) | §7 |

Caches: npm through `actions/setup-node`, Cargo through
`Swatinem/rust-cache`, mise's tools through `mise-action`.
Secrets: `VSCE_PAT` and `OVSX_PAT` for the extension marketplaces
(publication is skipped without them); nothing else — the GitHub
release uses the workflow's own token. Dependabot opens, per ecosystem
per month (cargo for the workspace and the Zed extension, npm, pip,
GitHub Actions), one pull request with the minor and patch updates —
mergeable when the gate passes — and one with the majors, which are a
planned update: they change code, and tree-sitter moves the grammar,
the three implementations, and the Zed API together.

## 7. Release and distribution

Channels and the checklist are `packaging/README.md`; this is the shape.

**The version** is one string in seven places, bumped together:
`decl-ts/package.json`, `decl-rs/Cargo.toml`, `decl-py/pyproject.toml`
and `decl-py/decl/__init__.py`, `extension/vscode/package.json`,
`extension/zed/extension.toml` and `extension/zed/Cargo.toml`; the
Homebrew formula follows the npm publication (url and sha256).

**The order**: bump, `make verify` and `make lint` green, the package
smokes (`npm run smoke:dist`, `scripts/smoke.py`), publish the language
packages by hand (`npm publish`, `twine upload`, `cargo publish`), then
tag — `git tag v0.3.0 && git push origin v0.3.0` — which runs
`release.yml`:

| Job | Builds |
|---|---|
| `binaries` (six runners) | `decl` and `decl-lsp` per platform; a Linux binary must be statically linked (the job refuses a dynamic one); every binary evaluates two examples against their goldens on the platform it was built for |
| `wheels` (six runners) | `cibuildwheel` for every CPython `requires-python` admits; `decl --version` in each wheel |
| `vsix` | the VS Code extension; published to the Marketplace and Open VSX when the tokens exist |
| `release` | the GitHub release with every asset, notes generated |

Two consequences of the repository being private: the Zed extension
downloads `decl-lsp` from the release without authentication, so that
path (and the Zed registry) needs a public repository — `PATH` or a
setting works meanwhile; and macOS and Windows runner minutes count
against the plan at 10× and 2×.

### Release targets

| Asset | Platforms |
|---|---|
| `decl-<os>-<arch>`, `decl-lsp-<os>-<arch>` | macOS arm64 / x86_64; Linux arm64 / x86_64, static against musl; Windows arm64 / x86_64 (`.exe`) |
| Python wheels | the same six platforms; CPython 3.10–3.14 (Windows arm64 from 3.11); Linux as manylinux and musllinux |
| `vscode-decl.vsix` | platform-neutral (the server is bundled JavaScript) |

## 8. Editors

The extensions and configurations are documented in
`docs/tooling/04_extension.md`; the versions they were checked with:

The versions the extensions and configurations were checked with;
`extension/smoke-editors.sh` reproduces the checks for the editors
without an extension.

| Editor | Version | How Decl is attached |
|---|---|---|
| VS Code | 1.136.1 | `extension/vscode` (Marketplace / Open VSX; engine ≥ 1.90) |
| Zed | 1.18.1 | `extension/zed` (dev extension; the registry after the first release) |
| Neovim | 0.12.5 (0.10+ needed) | `extension/neovim` — built-in tree-sitter and LSP client |
| Helix | 25.07.1 | `extension/helix` — `languages.toml`, the grammar built by `hx --grammar build` |
| Emacs | 31.1 (29+ needed) | `extension/emacs` — `decl-ts-mode` over `treesit`, eglot |
| Vim | 9.1 | `extension/vim` — regex syntax; yegappan/lsp for the server |
| Sublime Text | Build 4200 | `extension/sublime` — a package; the LSP package from Package Control |

### Tools for the checks done by hand

| Tool | Version | Used for |
|---|---|---|
| `expect` | 5.45 | driving Helix, Vim, and the REPLs on a pseudo-terminal |
| Docker | 29.8 | the Alpine container that confirmed the musl binaries are static |
| syntect | 5.3 (built from git by `extension/sublime/smoke.sh`) | running the Sublime syntax tests headlessly |
| tree-sitter CLI | 0.25.10 (`npx tree-sitter`) | building the parser libraries for Neovim and Emacs, loading every query against the grammar |

## 9. Conventions

- Documents are written in English; the specification (`docs/specification/`,
  v0.1, frozen) is the single source of truth, and a post-freeze change
  is a revision: the charter decision, every affected chapter, and
  `docs/REVISIONS.md` in one change. `docs/README.md` indexes every
  document; `ROADMAP.md` owns the phase plan.
- Commit messages are short, lower-case, descriptive, without scopes
  (`refine docs`, `require byte-identical exit codes`).
- Decl files: 4-space indentation, no tabs, 100-char lines; fixtures
  are `snake_case` under `valid/` and `invalid/`, the latter with
  `@expect-error` / `@expect-message` / `@expect-phase` headers.
- Never committed: `.claude/`, build outputs, the compiled grammar
  copies, `.vsix` and `extension.wasm`, the local Zed settings — the
  `.gitignore` says which.
