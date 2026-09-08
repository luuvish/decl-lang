//! The query engine (qengine/DESIGN.md): a from-scratch incremental, memoized
//! evaluator, a faithful port of decl-ts/src/qengine. `db` is the generic
//! incremental core; `qeval` (the decl layer) is built on it.
pub mod db;
pub mod qeval;
