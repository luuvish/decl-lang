# REPL sessions

Scripted sessions for `decl repl --script` (docs/tooling/02_repl.md §9).
Each case is a directory holding an entry module (`main.decl`, with any
modules and documents it needs), the session (`session.txt`), and the
transcript the reference implementation prints (`transcript.txt`). The
session is run from the repository root, so its file arguments are
root-relative; the exit status is 1 iff the transcript has an `error:`
line. The TypeScript tests replay every case against its transcript
(`decl-ts/test/repl.ts`); the parity harness requires the Rust
and Python REPLs to print the same bytes.

Commands whose output is not deterministic (`:time`) or that write files
(`:save`, `:write`, `:history file`) are exercised by the unit tests,
not by the corpus.
