# Decl — three implementations, one behavior.
#
#   make verify        the gate: every implementation's own tests, then the
#                      parity check that the TypeScript reference, the Rust
#                      runtime, and the Python runtime are indistinguishable
#   make parity        only the cross-implementation differential
#   make test-<lang>   one implementation (typescript | rust | python)
#
# Layout: packages/typescript (npm workspace member `decl-lang`),
# packages/rust (Cargo workspace member), packages/python (pip).
# Requirements: Node.js >= 20, a Rust toolchain, Python >= 3.10 with a C
# compiler (the grammar is compiled as an extension module).
SHELL := /bin/bash
PY ?= python3
VENV := packages/python/.venv
VPY := $(abspath $(VENV)/bin/python)

.PHONY: verify parity build-typescript test-typescript build-rust test-rust python-env test-python site clean

verify: test-typescript test-rust test-python parity
	@echo "verify: all three implementations agree"

# ---- TypeScript (the reference implementation) ----
node_modules: package-lock.json
	npm ci

build-typescript: node_modules
	npm run build -w decl-lang

test-typescript: node_modules
	npm test -w decl-lang

# ---- Rust (native runtime) ----
build-rust:
	cargo build --release

test-rust: build-rust
	target/release/decl validate tests/validation

# ---- Python (native runtime + package) ----
# `npm run build` first: it copies the grammar sources the extension compiles
python-env: build-typescript
	test -x $(VENV)/bin/python || $(PY) -m venv $(VENV)
	$(VPY) -m pip install -q --upgrade pip setuptools
	$(VPY) -m pip install -q -e ./packages/python
	cd packages/python && $(VPY) setup.py -q build_ext --inplace

test-python: python-env
	cd packages/python && $(VPY) scripts/e2e.py
	$(VPY) -m decl.runtime validate tests/validation

# ---- parity: reference vs both native runtimes, byte for byte ----
parity: build-typescript build-rust python-env
	DECL_PYTHON=$(VPY) $(VPY) tests/parity/differential.py

# ---- the website ----
site: build-typescript
	npm run build -w site

clean:
	rm -rf packages/typescript/dist target packages/python/build packages/python/decl/*.egg-info packages/python/decl/_tree_sitter/*.so site/dist
