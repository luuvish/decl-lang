# REPL sessions

Scripted sessions for `decl repl --script` (docs/tooling/02_repl.md §9).
Each case is a directory holding an entry module (`main.decl`, with any
modules and documents it needs), the session (`session.txt`), the
transcript the reference implementation prints (`transcript.txt`), and,
for a session that writes files, an `expected/` directory with the
files it must leave, byte for byte.

A case is replayed **in a fresh copy of its directory**: the driver
copies the case to a temporary directory, runs
`decl repl main.decl --script session.txt` there, and compares the
transcript, the exit status (1 iff the transcript has an `error:`
line), and every file under `expected/` with the file of the same name
the session left. Paths in a session are therefore relative to the
case (`:bind deployed=doc.json`). Milliseconds in a transcript (`:time`)
are normalized to `<ms>` by every driver: `\d+\.\d ms` → `<ms> ms`.

Every implementation replays every case (`decl-ts/tests/repl_test.ts`,
`decl-rs/tests/repl_test.rs`, `decl-py/tests/repl_test.py`), and again under
`DECL_FULL_RECOMPUTE=1` — the incremental step must be observationally
identical to a full recomputation (`:time`'s `recomputed N of M slots`,
which only the incremental step reports, is dropped from that
comparison); the parity harness requires the Rust
and Python REPLs to print the same bytes and leave the same files. The
command line's own REPL arguments (`--input`, `--script -`, `--compact`,
the usage errors) are cases of `tests/cli/`.
