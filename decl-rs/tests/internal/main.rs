//! The internal checks (tests/internal/checks.json) on the Rust
//! implementation: one module per source module, each check a test named
//! as checks.json names it; tests/internal/coverage.py holds the three
//! implementations to the same list.
#[path = "../common/mod.rs"]
mod common;

mod checker_test;
mod conformance_test;
mod engine_test;
mod fmt_test;
mod infer_test;
mod module_test;
mod package_test;
mod parse_test;
mod pipeline_test;
mod render_test;
mod semantics_test;
mod session_test;
mod yaml_test;
