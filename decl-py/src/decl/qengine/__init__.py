"""The query engine (qengine/DESIGN.md): a from-scratch incremental, memoized
evaluator, a faithful port of decl-ts/src/qengine and decl-rs/src/qengine. `db`
is the generic incremental core; `qeval` drives shared programs and the retained
slot graph."""
