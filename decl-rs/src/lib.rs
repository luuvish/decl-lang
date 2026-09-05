// The crate's page is the README (crates.io shows it; docs.rs renders it
// here, its Rust examples compiled as doctests); every public item beneath
// it is documented, and the compiler holds the crate to that.
#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
pub mod api;
pub mod ast;
pub mod checker;
pub mod cli;
pub mod conformance;
pub mod engine;
pub mod fmt;
pub mod infer;
pub mod lsp;
pub mod module;
pub mod package;
pub mod parse;
pub mod pipeline;
pub mod repl;
pub mod semantics;
pub mod session;
pub mod subsume;

// the high-level API, in the command line's vocabulary
pub use api::{
    check, evaluate, evaluate_source, format_source, validate, DeclError, Diagnostic, Document,
    EvaluateOptions, Report,
};
