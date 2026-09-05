//! The API corpus (tests/api/): the driver's answers (examples/api_corpus.rs)
//! against the reviewed expected answers, documents compared by value.
mod common;
use common::*;

/// The API corpus (tests/api/): the driver's answers against the reviewed
/// expected answers, documents compared by value.
#[test]
fn api_corpus_matches_expected() {
    let got = parse(&api_corpus::run());
    let want = parse(
        &std::fs::read_to_string(api_corpus::repo_root().join("tests/api/expected.json")).unwrap(),
    );
    let (Value::JArr(got), Value::JArr(want)) = (&got, &want) else {
        panic!("lists")
    };
    assert_eq!(got.len(), want.len(), "every case answered");
    let mut failures = vec![];
    for (g, w) in got.iter().zip(want.iter()) {
        if !json_eq(g, w) {
            failures.push(format!(
                "{}\n      expected {}\n      got      {}",
                text(get(w, "name").unwrap()),
                &json_of(w)[..json_of(w).len().min(300)],
                &json_of(g)[..json_of(g).len().min(300)]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "api corpus failures:\n  {}",
        failures.join("\n  ")
    );
}
