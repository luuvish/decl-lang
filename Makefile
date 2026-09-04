# Decl — three implementations, one behavior.
#
#   make verify        the gate: every implementation's own tests, then the
#                      parity check that the TypeScript reference, the Rust
#                      runtime, and the Python runtime are indistinguishable
#   make parity        only the cross-implementation differential
#   make test-<lang>   one implementation (typescript | rust | python)
#   make site          the website (sync docs, copy the playground bundle, render)
#
# Layout: decl-ts (npm workspace member `decl-lang`),
# decl-rs (Cargo workspace member), decl-py (pip).
# Requirements: Node.js >= 20, a Rust toolchain, Python >= 3.10 with a C
# compiler (the grammar is compiled as an extension module).
SHELL := /bin/bash
PY ?= python3
VENV := decl-py/.venv
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
	cargo test --release
	target/release/decl validate tests/validation

# ---- Python (native runtime + package) ----
# `npm run build` first: it copies the grammar sources the extension compiles
python-env: build-typescript
	test -x $(VENV)/bin/python || $(PY) -m venv $(VENV)
	$(VPY) -m pip install -q --upgrade pip setuptools
	$(VPY) -m pip install -q -e ./decl-py
	cd decl-py && $(VPY) setup.py -q build_ext --inplace

test-python: python-env
	cd decl-py && $(VPY) scripts/e2e.py
	$(VPY) -m decl.runtime validate tests/validation

# ---- parity: reference vs both native runtimes, byte for byte ----
parity: build-typescript build-rust python-env
	DECL_PYTHON=$(VPY) $(VPY) tests/parity/differential.py

# ---- the website ----
site: build-typescript
	npm run build -w site

clean:
	rm -rf decl-ts/dist target decl-py/build decl-py/decl/*.egg-info decl-py/decl/_tree_sitter/*.so site/dist
