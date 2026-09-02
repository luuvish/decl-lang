---
title: Install
description: The decl command through npm, PyPI, crates.io, and Homebrew.
sidebar:
  order: 1
---

The `decl` command ships through four channels. Registry package names differ where `decl` was already taken; the binary is `decl` everywhere.

| Channel | Install | What you get |
|---|---|---|
| npm | `npm install -g decl-lang` | The reference implementation: `decl` (check, evaluate, validate, fmt) and `decl-lsp`. Needs Node.js ≥ 20. |
| PyPI | `pip install decl` | A native Python runtime for `evaluate` / `validate` and a Python API. `check` / `fmt` / `decl-lsp` run the bundled reference under Node (`pip install 'decl[node]'` lets pip provide Node). |
| crates.io | `cargo install decl-lang` | A native Rust runtime: `decl evaluate` and `decl validate`, no Node or wasm. |
| Homebrew | `brew install luuvish/decl/decl` | The npm package through a tap. |

All channels produce **byte-identical output**: the native runtimes are held to the reference by a differential test over every example and fixture in the repository.

## Which one?

- **Editing Decl or running a full toolchain** (static checks, formatting, an editor server): npm or Homebrew.
- **Embedding evaluation in a Python project**: PyPI — `decl.evaluate("site.decl", root="site")` returns plain dicts and lists.
- **A single static binary for CI or deployment**: crates.io.

## From source

```bash
git clone https://github.com/luuvish/decl-lang
cd decl-lang
npm install && npm test              # the reference implementation (npm workspaces)
cargo build --release                # the Rust runtime (Cargo workspace)
pip install -e decl-python       # the Python package (builds the grammar extension)
make verify                          # all three, then the parity check
```

See the [tooling pages](/decl-lang/tooling/javascript/) for each package's API.
