# Decl — three implementations, one behavior.
#
# From the repository root:
#
#   make verify          the gate: each implementation's own tests, then the
#                        parity harness that holds the Rust and Python
#                        implementations to the TypeScript reference byte for byte
#   make lint            each language's type checker, linter, and formatter in
#                        check mode (tsc, eslint, prettier; clippy, rustfmt;
#                        mypy, ruff) — CI runs it beside the gate
#   make format          rewrite every source in its language's canonical form
#   make version         the release version, checked to agree in every place it lives
#   make bump VERSION=x.y.z   set it everywhere (manifests, lockfiles, the reported version)
#   make test-<lang>     one implementation's tests      (typescript | rust | python)
#   make lint-<lang>     one language's checks
#   make format-<lang>   one language's formatters
#   make parity          the harness alone (tests/parity/differential.py)
#   make site            the website (docs synced, the playground bundled, rendered)
#   make clean           build outputs;  make distclean   environments too
#
# Toolchains: mise.toml pins them (`mise install`, docs/DEVELOPMENT.md); the
# grammar is C, compiled into the Python extension by the platform's compiler.

SHELL := /bin/bash
PY    ?= python3
VENV  := decl-py/.venv
VPY   := $(abspath $(VENV)/bin/python)

.PHONY: verify lint format parity site clean distclean version bump \
        build-typescript test-typescript lint-typescript format-typescript \
        build-rust test-rust lint-rust format-rust \
        python-env test-python lint-python format-python

# ---------------------------------------------------------------- the gate
verify: test-typescript test-rust test-python parity
	@echo "verify: all three implementations agree"

lint: lint-typescript lint-rust lint-python version
	@echo "lint: clean"

# ---------------------------------------------------------------- the version
version:
	node scripts/version.mjs

bump:
	@test -n "$(VERSION)" || { echo "usage: make bump VERSION=x.y.z"; exit 2; }
	node scripts/version.mjs --set $(VERSION)

format: format-typescript format-rust format-python

# ---------------------------------------------------------------- TypeScript (the reference)
# one npm workspace at the root: decl-ts, tree-sitter-decl, site, extension/vscode.
# The web extension's test runner would download browsers on install; the CI
# job that runs those tests installs them itself (npx playwright install).
node_modules: package-lock.json
	PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci

build-typescript: node_modules
	npm run build -w decl-lang

test-typescript: node_modules
	npm test -w decl-lang

lint-typescript: node_modules
	npm run typecheck
	npm run lint
	npm run format:check

format-typescript: node_modules
	npm run format
	npm run lint:fix

# ---------------------------------------------------------------- Rust
# the Cargo workspace at the root (decl-rs); extension/zed is outside it
build-rust:
	cargo build --locked --release --examples

test-rust: build-rust
	cargo test --locked --release

lint-rust:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked -p decl-lang
	cd extension/zed && cargo fmt --check
	cd extension/zed && cargo clippy --all-targets --locked -- -D warnings

format-rust:
	cargo fmt --all
	cd extension/zed && cargo fmt

# ---------------------------------------------------------------- Python
# the venv holds the package (editable, with its dev tools) and the compiled
# grammar; the reference build first, since it syncs the grammar sources
python-env: build-typescript
	test -x $(VENV)/bin/python || $(PY) -m venv $(VENV)
	$(VPY) -m pip install -q --upgrade pip setuptools
	$(VPY) -m pip install -q -e './decl-py[dev]'
	cd decl-py && $(VPY) setup.py -q build_ext --inplace

test-python: python-env
	cd decl-py && $(VPY) -m pytest -q

lint-python: python-env
	cd decl-py && $(VPY) -m ruff check .
	cd decl-py && $(VPY) -m ruff format --check .
	cd decl-py && $(VPY) -m mypy src/decl

format-python: python-env
	cd decl-py && $(VPY) -m ruff format .
	cd decl-py && $(VPY) -m ruff check --fix .

# ---------------------------------------------------------------- parity
parity: build-typescript build-rust python-env
	DECL_PYTHON=$(VPY) $(VPY) tests/parity/differential.py

# ---------------------------------------------------------------- the website
site: build-typescript
	npm run build -w site

# ---------------------------------------------------------------- cleaning
clean:
	rm -rf decl-ts/dist target extension/zed/target decl-py/build decl-py/src/*.egg-info decl-py/src/decl/_tree_sitter/*.so site/dist

distclean: clean
	rm -rf node_modules $(VENV)
