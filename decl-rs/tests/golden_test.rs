//! The golden manifest (tests/golden/manifest.json) through the command
//! line: every entry's bytes, exactly (tests/golden/README.md).
mod common;
use common::*;
use std::process::Command;

/// The golden manifest (tests/golden/manifest.json) through the command
/// line: every entry's bytes, exactly (tests/golden/README.md).
#[test]
fn golden_manifest() {
    let root = root();
    let manifest =
        parse(&std::fs::read_to_string(root.join("tests/golden/manifest.json")).unwrap());
    let Value::JArr(entries) = manifest else {
        panic!("a list")
    };
    let tmp = std::env::temp_dir().join(format!("decl-golden-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let blocks = regex::Regex::new(r"(?s)```decl\n(.*?)```").unwrap();
    let mut failures = vec![];
    for e in entries.iter() {
        let rejected = matches!(get(e, "rejected"), Some(Value::Bool(true)));
        // a markdown entry's module: its ```decl blocks in order, in a temporary file
        let module = match get(e, "module") {
            Some(m) => text(m).to_string(),
            None => {
                let md =
                    std::fs::read_to_string(root.join(text(get(e, "markdown").unwrap()))).unwrap();
                let src: Vec<&str> = blocks
                    .captures_iter(&md)
                    .map(|c| c.get(1).unwrap().as_str())
                    .collect();
                let p = tmp.join("guide.decl");
                std::fs::write(&p, src.join("\n")).unwrap();
                p.to_string_lossy().to_string()
            }
        };
        let mut args: Vec<String> = vec![
            if rejected { "validate" } else { "evaluate" }.into(),
            module,
        ];
        if let Some(Value::JArr(inputs)) = get(e, "inputs") {
            for spec in inputs.iter() {
                args.push("--input".into());
                args.push(text(spec).into());
            }
        }
        if let Some(o) = get(e, "output") {
            args.push("--output".into());
            args.push(text(o).into());
        }
        let out = Command::new(env!("CARGO_BIN_EXE_decl"))
            .args(&args)
            .current_dir(&root)
            .output()
            .unwrap();
        let golden = text(get(e, "golden").unwrap());
        let expected = std::fs::read_to_string(root.join(golden)).unwrap();
        let (want_exit, got) = if rejected {
            (1, String::from_utf8_lossy(&out.stderr).to_string())
        } else {
            (0, String::from_utf8_lossy(&out.stdout).to_string())
        };
        if out.status.code() != Some(want_exit) || got != expected {
            failures.push(format!(
                "{golden}: exit {:?}, {}",
                out.status.code(),
                if got == expected {
                    "same bytes"
                } else {
                    "different bytes"
                }
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "golden failures:\n  {}",
        failures.join("\n  ")
    );
}
