//! The fixture corpus (tests/validation, tests/validation/README.md) judged
//! by `decl validate <dir>` (src/conformance.rs): every fixture parses,
//! checks, and evaluates as its header says.
mod common;
use common::*;
use std::process::Command;

#[test]
fn validation_corpus() {
    let out = Command::new(env!("CARGO_BIN_EXE_decl"))
        .args(["validate", "tests/validation"])
        .current_dir(root())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "every fixture judged as its header says:\n{err}"
    );
    assert!(
        err.lines().any(|l| l.ends_with(" ok, 0 failed")),
        "the summary counts no failure: {err}"
    );
}
