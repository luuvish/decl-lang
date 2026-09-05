//! conformance: the fixture judge and the corpus walk it rests on.
use crate::common::root;
use decl_lang::conformance::{judge_fixture, walk_decl};

#[test]
fn judge() {
    let mut files = vec![];
    walk_decl(&root().join("tests/modules"), &mut files);
    assert_eq!(files.len(), 11);
    assert!(files.windows(2).all(|w| w[0] < w[1]), "sorted");
    assert!(files
        .iter()
        .any(|f| f.ends_with("tests/modules/basic/main.decl")));
    let valid = judge_fixture(
        &root().join("tests/validation/types/valid/predicates.decl"),
        true,
    );
    let wrong = judge_fixture(
        &root().join("tests/validation/types/invalid/empty_range.decl"),
        true,
    );
    let right = judge_fixture(
        &root().join("tests/validation/types/invalid/empty_range.decl"),
        false,
    );
    assert!(valid.ok, "{}", valid.detail);
    assert!(
        !wrong.ok && wrong.detail.contains("E4011"),
        "{}",
        wrong.detail
    );
    assert!(right.ok, "{}", right.detail);
}
