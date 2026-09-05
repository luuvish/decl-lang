# Command-line cases

The command line (docs/tooling/01_cli.md, 02_repl.md) case by case:
`cases.json` lists an invocation and the outcome recorded from the
reference implementation, reviewed — the exit status, standard output,
standard error, and the files the run leaves.

```json
{ "name": "evaluate: --output name=file writes the document",
  "files": { "demo.decl": "type T = { a: int, b = a * 2 }\nexport output demo: T = { a: 21 }\n" },
  "args": ["evaluate", "<dir>/demo.decl", "--output", "demo=<dir>/demo.json"],
  "exit": 0, "stdout": "", "stderr": "",
  "after": { "demo.json": "{\"a\":21,\"b\":42}\n" } }
{ "name": "decl-lsp --version prints the same version", "program": "decl-lsp", "args": ["--version"],
  "exit": 0, "stdout": "decl-lsp <version>\n", "stderr": "" }
```

- `files` are written into a fresh directory before the run; `<dir>` in
  `args` is that directory, and `<dir>` in the recorded outputs is where
  it appeared;
- `program` is `decl` (the default) or `decl-lsp`; `stdin` is fed to
  the run; every driver runs from the repository root, so a
  repository-relative path (`tests/modules/basic/main.decl`) is passed
  as given;
- `<version>` in the recorded outputs stands for the implementation's
  own version;
- `after` names files read back after the run — their text, or `null`
  for a file that must not exist.

The drivers: `decl-ts/tests/cli_test.ts`, `decl-rs/tests/cli_test.rs`,
`decl-py/tests/cli_test.py`; the parity harness runs every case through
the three command lines, each in its own copy of the files, and
requires the recorded outcome of the reference and identical outcomes
of the natives. A command-line behavior lands with its case here; the
outcome is regenerated from the reference in the same change and read.
