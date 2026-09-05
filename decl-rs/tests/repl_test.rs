//! `decl repl`: the session corpus (tests/repl/<case>/), each case replayed
//! in a fresh copy of its directory against its transcript and the files it
//! leaves (tests/repl/README.md), and again under DECL_FULL_RECOMPUTE=1 —
//! the incremental step is observationally identical to a full
//! recomputation (docs/tooling/02_repl.md §6).
mod common;
use common::*;
use std::path::{Path, PathBuf};
use std::process::Command;

/// the two normalizations of a transcript: milliseconds are the clock's, not the
/// session's; the count of recomputed slots is what only the incremental step reports
struct Norms {
    ms: regex::Regex,
    count: regex::Regex,
}
impl Norms {
    fn new() -> Norms {
        Norms {
            ms: regex::Regex::new(r"\d+\.\d ms").unwrap(),
            count: regex::Regex::new(r", recomputed \d+ of \d+ slots").unwrap(),
        }
    }
}

fn run(dir: &Path, full: bool, norms: &Norms) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_decl"));
    cmd.arg("repl");
    if dir.join("main.decl").exists() {
        cmd.arg("main.decl");
    }
    cmd.args(["--script", "session.txt"]).current_dir(dir);
    if full {
        cmd.env("DECL_FULL_RECOMPUTE", "1");
    } else {
        cmd.env_remove("DECL_FULL_RECOMPUTE");
    }
    let out = cmd.output().unwrap();
    (
        out.status.code(),
        norms
            .ms
            .replace_all(&String::from_utf8_lossy(&out.stdout), "<ms> ms")
            .to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn repl_corpus() {
    let root = root();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(root.join("tests/repl"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("session.txt").exists())
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    let norms = Norms::new();
    let mut failures: Vec<String> = vec![];
    for case in cases {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let want = std::fs::read_to_string(case.join("transcript.txt")).unwrap();
        let want_code = Some(if want.lines().any(|l| l.starts_with("error: ")) {
            1
        } else {
            0
        });
        let dir = temp_dir("repl");
        copy_dir(&case, &dir);
        let (code, out, err) = run(&dir, false, &norms);
        if out != want {
            failures.push(format!(
                "{name}: the transcript\n      {}",
                first_diff(&want, &out)
            ));
        }
        if code != want_code {
            failures.push(format!(
                "{name}: exit {code:?}, expected {want_code:?} — {}",
                &err[..err.len().min(200)]
            ));
        }
        // the files the session leaves: every file under expected/, byte for byte
        let expected = case.join("expected");
        if expected.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&expected)
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .collect();
            files.sort();
            for f in files {
                let n = f.file_name().unwrap().to_string_lossy().to_string();
                let text = std::fs::read_to_string(&f).unwrap();
                let actual = std::fs::read_to_string(dir.join(&n)).ok();
                if actual.as_deref() != Some(text.as_str()) {
                    failures.push(format!("{name}: {n} afterwards\n      expected {text:?}\n      got      {actual:?}"));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        // the incremental step against a full recomputation
        let dir2 = temp_dir("repl");
        copy_dir(&case, &dir2);
        let (code_full, full, _) = run(&dir2, true, &norms);
        let sans_count = norms.count.replace_all(&out, "").to_string();
        if sans_count != full || code != code_full {
            failures.push(format!(
                "{name}: incremental != full recomputation\n      {}",
                first_diff(&full, &sans_count)
            ));
        }
        let _ = std::fs::remove_dir_all(&dir2);
        println!("repl {name}: ok");
    }
    assert!(
        failures.is_empty(),
        "repl corpus failures:\n  {}",
        failures.join("\n  ")
    );
}
