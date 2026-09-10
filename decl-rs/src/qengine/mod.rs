//! The query engine (qengine/DESIGN.md): a from-scratch incremental, memoized
//! evaluator, a faithful port of decl-ts/src/qengine. `db` is the generic
//! incremental core; `qeval` drives the shared programs and retained slot graph.
pub mod db;
pub mod edits;
pub mod programs;
pub mod qeval;

pub mod revisions;
pub mod rounds;
