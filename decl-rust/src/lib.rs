//! Decl, implemented natively in Rust: the parser binding, the static
//! checker, the evaluator, modules and packages, the canonical formatter,
//! and the language server — the same behavior as the TypeScript
//! reference implementation, verified by tests/parity in the repository.
pub mod ast;
pub mod checker;
pub mod cli;
pub mod engine;
pub mod fmt;
pub mod infer;
pub mod lsp;
pub mod module;
pub mod package;
pub mod parse;
pub mod semantics;
pub mod subsume;
