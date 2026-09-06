# The command line — `decl`

`decl` is the command-line tool every implementation ships (npm, PyPI,
crates.io, Homebrew: package `decl-lang`, binary `decl`). Its five
subcommands are the language's three capabilities plus formatting, and
the three implementations answer every command line with the same exit
code, standard output, and standard error, byte for byte
(`tests/parity/differential.py`). This document is informative; the
behavior it describes is fixed by the [specification](../specification/01_introduction.md)
(§10 for documents, §12 for diagnostics).

```
decl --version
decl check <file>... [--json]
decl evaluate <file> [--input name=doc.(json|yaml)]... [--output name[=file|dir|-]]...
                     [--format json|yaml] [--indent n | --pretty] [--template [root=]path]... [--json]
decl validate <file> [--input name=doc.(json|yaml)]... [--expect-errors E1,E2] [--json]
decl validate <dir>
decl fmt <file>... [--check]
decl repl [file.decl] [--input name=doc.(json|yaml)]... [--script session.txt | --script -] [--compact]
```

## 1. Conventions

- **Two streams.** Standard output carries only what a program would
  consume: the documents `evaluate` emits, the `--json` reports, the
  version. Everything addressed to a person — diagnostics, the `ok:` /
  `reformatted` / `FAIL` lines, usage errors, hints — goes to standard
  error. `decl evaluate site.decl > site.json` therefore never captures
  a diagnostic, and an empty standard output means no document.
- **Arguments.** Options and file arguments may be mixed in any order.
  An option that takes a value takes the next argument (`--input
  name=doc.json`; the `--input=…` form is not accepted); `--input` and
  `--output` repeat. Files are read as UTF-8 by the path given, relative
  to the working directory; nothing is read from standard input.
- **Version.** `decl --version` prints `decl 0.3.0` — the package's
  version, the same string on every channel; its major and minor are the
  specification revision the tool implements.
- **Exit codes** are three (§5): 0 clean, 1 something reported, 2 the
  command line itself was wrong.

## 2. Universes, inputs, outputs

Every command that loads a module opens the **universe** of an entry
module: the module, the modules it imports (transitively), and — when a
`decl.toml` governs the entry's directory — the package resolver and the
lock, whose drift is a diagnostic (§8.6–8.7: **E3015** missing entry,
**E3016** version differs from the manifest, **E3017** content-hash
mismatch). Diagnostics name the entry module by the path given on the
command line and any other module by its absolute path. An entry that
does not exist is **E3004** `module not found`, and one that does not
parse is **E2001** with the parse-error count — both load diagnostics,
reported the same way by `check`, `evaluate`, and `validate`.

`--input name=doc.json` binds the JSON document in `doc.json` to the
input root `name` (§10.2); it is repeatable, one document per input. A
spec without `=` or an input the universe does not declare is a usage
error (exit 2, no report); a document that cannot be read, or is not
well-formed JSON, or has trailing characters, is one diagnostic **E6004**
against the entry (exit 1). An input left unbound takes its fallback,
or is reported as §10.2 says.

`--output name[=file]` says what `evaluate` emits, and where: `name` is a
root — an output, or an input bound by `--input` or demanded through its
fallback — `=file` writes its document to that file, and without `=file`
the document goes bare to standard output (at most one). With no
`--output`, the entry module's **exported** outputs are emitted as one
object keyed by name (§5.5); a module that exports none emits `{}` and
says so on standard error. An unknown root is `no root named X` (exit 1),
and a file that cannot be written is reported by name (exit 1).

## 3. Commands

### `decl check`

Loads each entry file's universe and statically checks every module in
it (§8 for loading, §6 and §9.7 for what the checker decides). Prints
every diagnostic of every entry; when nothing was reported,
`ok: N entry file(s) check clean` on standard error. Exit 1 iff an
error was reported: a warning (an unknown annotation, W0001) is
printed and leaves the exit code 0.

### `decl evaluate`

Loads the universe, checks it (a static diagnostic stops here, exit 1),
binds the `--input` documents, evaluates, and emits as `--output` says.
Diagnostics of every severity go to standard error; an error-severity
diagnostic means no document is emitted and exit 1. Documents are
canonical JSON (§10.4): compact, full-precision integers, shortest
round-trip floats, quantities as `{ "value", "unit" }`, references as
canonical paths — document-relative (`$.…`) within the emitted root —
unless the root's `@render` annotation or the options say otherwise
([05. Renderer](05_render.md)): `--format json|yaml` and `--indent n`
(`--pretty` = `--indent 2`) lay a document out, an output's `@render({
format, indent, file, template, each })` declares its own form, and
`--output name` alone writes to the declared `file` when there is one
(`name=-` forces standard output). A `--input name=doc.yaml` document
is read as YAML by its extension (05_render.md §2). Under `--json` the
report's `value` is the document in canonical JSON whatever the
layout.
Evaluation is total: every root demanded is evaluated whole, every
assertion visited — the verdict a document gets here is the document's
verdict, not a partial one (§9.8).

### `decl validate <file>`

The same pipeline as `evaluate`, module-aware, but emitting no document:
load the universe, check every module, bind the `--input` documents
(none is fine — fallbacks apply), evaluate every root, and print every
diagnostic. Exit 1 iff an error-severity diagnostic was reported. With
`--expect-errors E1,E2` the set of error codes reported must equal the
set named (warnings and information do not count): `ok: expected errors
reported (E1, E2)` and exit 0, or `expected error(s) not reported: …` /
`unexpected error(s): …` and exit 1 — the form CI scripts use to pin a
document's known failures.

### `decl validate <dir>`

Judges a fixture corpus: every `.decl` file under the directory, `valid/`
fixtures required to parse, check, and evaluate clean, `invalid/` fixtures
required to fail in the phase and with the code their `@expect-phase` /
`@expect-error` comments name, and — when `@expect-message` is given —
with a message containing the text named (`tests/validation/README.md`).
Prints one `FAIL file reason` line per miss and `N ok, M failed`; exit 1
iff any failed. The corpus judge reads fixtures as single modules; a
fixture is self-contained by construction.

### `decl fmt`

Rewrites each file in its canonical form (`reformatted <file>`), or with
`--check` reports `would reformat <file>` and touches nothing; a file
already canonical is passed over silently. The formatter keeps the
author's line structure (§2.9) and re-derives indentation and spacing;
it is idempotent and AST-preserving, and formatting is the same
function the language server's formatting request runs. A file that
cannot be read is `<file>: cannot be read`; one that does not parse is
`<file>: cannot format: file has parse errors`; both are skipped. Exit 1
iff a file could not be read or formatted or, under `--check`, would
change.

## 4. Diagnostics and `--json`

Diagnostics are reported in the specification's order (§6.7, §12.3):
compile-time diagnostics first, by source position, then evaluation-
and validation-time diagnostics by `(path, id)`, the path in canonical
path order — not in the order evaluation happened to reach them. On
standard error a diagnostic is one line:

```
<file>: <severity> [<code>] <id> at <path>: <message>
```

with the bracketed code, the id (a constraint's `Type.assert` id), and
the path present when the diagnostic has them, in the order of §6.7.
Codes are the specification's registry (§12.4), by family: E1xxx
lexical, E2xxx syntax, E3xxx names, modules, and packages (E3004 module
not found, E3015–E3017 lock drift), E4xxx types, E5xxx evaluation, E6xxx
validation and binding (E6004 a document that cannot be read or is not
JSON); every `assert` reports under its own id.

`--json` (for `check`, `validate`, and `evaluate`) collects diagnostics
into objects in the report's field order — `file, code, id, severity,
message, path`, absent fields omitted — and prints, on standard output:

- `check` / `validate`: the JSON array of diagnostics (`[]` when clean);
- `evaluate`: `{"ok": <bool>, "value": <document or null>, "diagnostics": [...]}`,
  where `value` is what would have gone to standard output, and
  `--output name=file` files are still written.

Usage errors (exit 2) print their one line, or the usage text, and no
report in either mode. `fmt` has no `--json`: its outcome is the exit
code and the per-file lines.

## 5. Exit codes

| Exit | Meaning |
|---|---|
| 0 | nothing of error severity reported; `fmt --check` found every file canonical; `--version` |
| 1 | an error-severity diagnostic (a parse or load diagnostic included), a missing root, a file that could not be read, written, or formatted, a corpus miss, an `--expect-errors` mismatch |
| 2 | a usage error: unknown subcommand, missing argument, a bad `--input` / `--output` spec, an unknown input, two documents for standard output |

Warnings and information never change the exit code; a script that must
fail on them reads the `--json` report.

## 6. Verification

The parity harness (`tests/parity/differential.py`) runs the Rust and
Python `decl` against the reference on the same command lines and diffs
exit code, standard output, and standard error byte for byte: `check`
and `evaluate` (with and without `--json`) over every fixture, example,
and module entry, `validate --input` and `evaluate --input` over bound
documents, `--output` to files and to standard output, the goldens
(`tests/golden`), `fmt` over every parseable module and `fmt --check`
over a corpus, package resolution and lock drift, and the command
line's whole surface: usage (no arguments, `--help`, an unknown command,
a missing operand), `--version`, several entry files, `--expect-errors`
(matching, mismatching, with `--json`, against a document that cannot be
read, a malformed document, a file that does not parse, without a
value), `validate <dir>`, and every error path (a missing file, an
unreadable or ill-formed document, an unknown root or input, a bad
`--input` or `--output` spec, an unwritable file, two documents for
standard output). The command line's cases with their recorded
outcomes are `tests/cli/` (each suite replays them; the harness runs
them three-way), the formatter's canonical-form cases `tests/fmt/`. A
behavior this document describes that no case or row exercises is a
gap in the corpus.

## 7. The language server and the REPL

`decl repl`, the sixth subcommand, is the interactive session over a
universe — [02. REPL](02_repl.md) — with its own argument syntax (the
usage line above) and the same core again: its `:check`, `:evaluate`,
`:validate`, and `:fmt` mean what the subcommands above mean, and its
scripted sessions (`tests/repl/`) are in the parity harness like every
other command line. `decl-lsp`, shipped beside `decl` by every
implementation, is the same core behind the Language Server Protocol;
it is described in [03. Language server](03_lsp.md).
