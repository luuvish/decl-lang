---
title: Install
description: The decl command through npm, PyPI, crates.io, and Homebrew.
sidebar:
  order: 1
---

The `decl` command ships through four channels. Registry package names differ where `decl` was already taken; the binary is `decl` everywhere.

| Channel | Install | What you get |
|---|---|---|
| npm | `npm install -g decl-lang` | The TypeScript reference implementation: `decl` (check, evaluate, validate, fmt) and `decl-lsp`. Needs Node.js ≥ 22. |
| PyPI | `pip install decl-lang` | The native Python implementation: `decl` (check, evaluate, validate, fmt), `decl-lsp`, and a Python API. No Node.js. |
| crates.io | `cargo install decl-lang` | The native Rust implementation: `decl` (check, evaluate, validate, fmt) and `decl-lsp`, no Node or wasm. |
| Homebrew | `brew install luuvish/tap/decl-lang` | The npm package through a tap. |

All channels produce **byte-identical output**: the three implementations are held together by a differential test over every example and fixture in the repository — diagnostics, evaluated JSON, formatter output, packages, and a language-server session.

## Which one?

- **Node.js already around**: npm or Homebrew — the reference implementation itself.
- **Python projects**: PyPI — the same toolchain plus `decl.evaluate("site.decl")` returning the exported outputs as plain dicts and lists.
- **A single static binary for CI, editors, or deployment**: crates.io.

## From source

```bash
git clone https://github.com/luuvish/decl-lang
cd decl-lang
npm install && npm test              # the reference implementation (npm workspaces)
cargo build --release                # the Rust runtime (Cargo workspace)
pip install -e decl-py       # the Python package (builds the grammar extension)
make verify                          # all three, then the parity check
```

See the [tooling pages](/tooling/typescript/) for each package's API.
