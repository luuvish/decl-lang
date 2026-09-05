//! The command-line corpus (tests/cli/cases.json) through `decl` and
//! `decl-lsp`: every case's exit status, standard output, standard error,
//! and the files it leaves, against the recorded expectations
//! (tests/cli/README.md).
mod common;
use common::*;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn cli_corpus() {
    let root = root();
    let text = std::fs::read_to_string(root.join("tests/cli/cases.json")).unwrap();
    let Ok(Value::JArr(cases)) = read_json(&text) else {
        panic!("cases.json is a list")
    };
    let version = env!("CARGO_PKG_VERSION");
    let mut failures: Vec<String> = vec![];
    for c in cases.iter() {
        let name = common::text(get(c, "name").unwrap());
        let dir = temp_dir("cli");
        if let Some(Value::JObj(files)) = get(c, "files") {
            for (f, t) in files.iter() {
                std::fs::write(dir.join(f), common::text(t)).unwrap();
            }
        }
        let program = if get(c, "program").map(common::text) == Some("decl-lsp") {
            env!("CARGO_BIN_EXE_decl-lsp")
        } else {
            env!("CARGO_BIN_EXE_decl")
        };
        let dir_s = dir.to_string_lossy().to_string();
        let args: Vec<String> = match get(c, "args") {
            Some(Value::JArr(xs)) => xs
                .iter()
                .map(|a| common::text(a).replace("<dir>", &dir_s))
                .collect(),
            _ => vec![],
        };
        let mut child = Command::new(program)
            .args(&args)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            let mut stdin = child.stdin.take().unwrap();
            if let Some(s) = get(c, "stdin") {
                stdin.write_all(common::text(s).as_bytes()).unwrap();
            }
        }
        let out = child.wait_with_output().unwrap();
        let norm = |s: &[u8]| {
            String::from_utf8_lossy(s)
                .replace(&dir_s, "<dir>")
                .replace(version, "<version>")
        };
        let got = (out.status.code(), norm(&out.stdout), norm(&out.stderr));
        let want = (
            Some(int_of(get(c, "exit").unwrap()) as i32),
            common::text(get(c, "stdout").unwrap()).to_string(),
            common::text(get(c, "stderr").unwrap()).to_string(),
        );
        if got != want {
            failures.push(format!(
                "{name}\n      expected {want:?}\n      got      {got:?}"
            ));
        }
        if let Some(Value::JObj(after)) = get(c, "after") {
            for (f, t) in after.iter() {
                let actual = std::fs::read_to_string(dir.join(f)).ok();
                let expected = match t {
                    Value::Null => None,
                    other => Some(common::text(other).to_string()),
                };
                if actual != expected {
                    failures.push(format!("{name}: {f} afterwards\n      expected {expected:?}\n      got      {actual:?}"));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    println!("cli corpus: {} cases", cases.len());
    assert!(
        failures.is_empty(),
        "cli corpus failures:\n  {}",
        failures.join("\n  ")
    );
}
