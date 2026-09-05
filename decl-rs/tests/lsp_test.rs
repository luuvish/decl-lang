//! The language-server corpus (tests/lsp/<case>/): every session replayed
//! over the stdio server, its answers against the transcript.
mod common;
use common::*;
use std::path::PathBuf;

/// The language-server corpus (tests/lsp/<case>/): every session replayed
/// over the stdio server, its answers against the transcript.
#[test]
fn lsp_corpus() {
    let root = root();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(root.join("tests/lsp"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("session.json").exists())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "the corpus has sessions");
    let mut failures: Vec<String> = vec![];
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let want = parse(&std::fs::read_to_string(case.join("transcript.json")).unwrap());
        let Value::JArr(want) = want else {
            panic!("a transcript is a list")
        };
        let got = Session::new(case).run(case);
        if want.len() != got.len() {
            failures.push(format!(
                "{name}: {} answers, {} expected",
                got.len(),
                want.len()
            ));
        }
        for (pair, (label, answer)) in want.iter().zip(got.iter()) {
            let Value::JArr(pair) = pair else {
                panic!("a transcript entry is a pair")
            };
            if text(&pair[0]) != label || !json_eq(&pair[1], answer) {
                failures.push(format!(
                    "{name}: {label}\n      {}",
                    first_diff(&json_of(&pair[1]), &json_of(answer))
                ));
            }
        }
        println!("lsp {name}: {} answers", got.len());
    }
    assert!(
        failures.is_empty(),
        "lsp corpus failures:\n  {}",
        failures.join("\n  ")
    );
}
