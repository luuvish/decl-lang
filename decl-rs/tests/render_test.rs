//! The render corpus (tests/render) through the native command line: the
//! format goldens of formats.json (`--format yaml`, `--indent n`, and the
//! YAML read back to the golden document), every golden document bound
//! from its YAML twin under inputs/, and the documents under invalid/ that
//! the reader must refuse with their messages (tests/render/README.md).
mod common;
use common::*;
use decl_lang::yaml::{read_yaml, to_json};
use std::process::Command;

fn run(root: &std::path::Path, args: &[String]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_decl"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

// a manifest entry's command line, with its inputs replaced when asked
fn args_of(
    root: &std::path::Path,
    e: &Value,
    tmp: &std::path::Path,
    inputs: Option<Vec<String>>,
) -> Vec<String> {
    let rejected = matches!(get(e, "rejected"), Some(Value::Bool(true)));
    let module = match get(e, "module") {
        Some(m) => text(m).to_string(),
        None => {
            let blocks = regex::Regex::new(r"(?s)```decl\n(.*?)```").unwrap();
            let md = std::fs::read_to_string(root.join(text(get(e, "markdown").unwrap()))).unwrap();
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
    let specs: Vec<String> = match inputs {
        Some(v) => v,
        None => match get(e, "inputs") {
            Some(Value::JArr(items)) => items.iter().map(|s| text(s).to_string()).collect(),
            _ => vec![],
        },
    };
    for spec in specs {
        args.push("--input".into());
        args.push(spec);
    }
    if let Some(o) = get(e, "output") {
        args.push("--output".into());
        args.push(text(o).into());
    }
    args
}

fn manifest(root: &std::path::Path) -> Vec<Value> {
    let m = parse(&std::fs::read_to_string(root.join("tests/golden/manifest.json")).unwrap());
    let Value::JArr(entries) = m else {
        panic!("a list")
    };
    entries.iter().cloned().collect()
}

#[test]
fn formats() {
    let root = root();
    let tmp = temp_dir("render");
    let entries = manifest(&root);
    let formats = parse(&std::fs::read_to_string(root.join("tests/render/formats.json")).unwrap());
    let Value::JArr(formats) = formats else {
        panic!("a list")
    };
    let mut failures = vec![];
    for f in formats.iter() {
        let golden_path = text(get(f, "golden").unwrap());
        let e = entries
            .iter()
            .find(|e| text(get(e, "golden").unwrap()) == golden_path)
            .expect("a manifest entry");
        let golden = std::fs::read_to_string(root.join(golden_path)).unwrap();
        let yaml_path = text(get(f, "yaml").unwrap());
        let yaml = std::fs::read_to_string(root.join(yaml_path)).unwrap();
        let mut args = args_of(&root, e, &tmp, None);
        args.push("--format".into());
        args.push("yaml".into());
        let (code, out, _) = run(&root, &args);
        if code != 0 || out != yaml {
            failures.push(format!(
                "{yaml_path}: --format yaml: exit {code}, {}",
                first_diff(&yaml, &out)
            ));
        }
        let back = to_json(&read_yaml(&yaml).unwrap(), 0) + "\n";
        if back != golden {
            failures.push(format!(
                "{yaml_path}: read back: {}",
                first_diff(&golden, &back)
            ));
        }
        if let Some(Value::JObj(indents)) = get(f, "indent") {
            for (n, file) in indents.iter() {
                let want = std::fs::read_to_string(root.join(text(file))).unwrap();
                let mut args = args_of(&root, e, &tmp, None);
                args.push("--indent".into());
                args.push(n.clone());
                let (code, out, _) = run(&root, &args);
                if code != 0 || out != want {
                    failures.push(format!(
                        "{}: --indent {n}: exit {code}, {}",
                        text(file),
                        first_diff(&want, &out)
                    ));
                }
                let parsed = to_json(&read_json(&want).ok().expect("JSON"), 0) + "\n";
                if parsed != golden {
                    failures.push(format!(
                        "{}: parses back: {}",
                        text(file),
                        first_diff(&golden, &parsed)
                    ));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "format failures:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn yaml_twins() {
    let root = root();
    let tmp = temp_dir("render");
    let re = regex::Regex::new(r"=tests/golden/inputs/(.*)\.json$").unwrap();
    let mut failures = vec![];
    for e in manifest(&root) {
        let Some(Value::JArr(inputs)) = get(&e, "inputs") else {
            continue;
        };
        let twins: Vec<String> = inputs
            .iter()
            .map(|s| {
                re.replace(text(s), "=tests/render/inputs/$1.yaml")
                    .to_string()
            })
            .collect();
        if twins.iter().zip(inputs.iter()).all(|(t, s)| t == text(s)) {
            continue;
        }
        let rejected = matches!(get(&e, "rejected"), Some(Value::Bool(true)));
        let args = args_of(&root, &e, &tmp, Some(twins.clone()));
        let (code, out, err) = run(&root, &args);
        let expected =
            std::fs::read_to_string(root.join(text(get(&e, "golden").unwrap()))).unwrap();
        let got = if rejected { err } else { out };
        if code != if rejected { 1 } else { 0 } || got != expected {
            failures.push(format!(
                "{}: exit {code}, {}",
                text(get(&e, "golden").unwrap()),
                first_diff(&expected, &got)
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "twin failures:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn invalid_documents() {
    let root = root();
    let cases =
        parse(&std::fs::read_to_string(root.join("tests/render/invalid/cases.json")).unwrap());
    let Value::JArr(cases) = cases else {
        panic!("a list")
    };
    let mut failures = vec![];
    for c in cases.iter() {
        let file = format!("tests/render/invalid/{}", text(get(c, "file").unwrap()));
        let message = text(get(c, "message").unwrap());
        let reader = match read_yaml(&std::fs::read_to_string(root.join(&file)).unwrap()) {
            Ok(_) => String::new(),
            Err(e) => e.message(),
        };
        if reader != message {
            failures.push(format!(
                "{file}: the reader says {reader:?}, not {message:?}"
            ));
        }
        let args: Vec<String> = vec![
            "validate".into(),
            "tests/render/invalid/doc.decl".into(),
            "--input".into(),
            format!("doc={file}"),
        ];
        let (code, _, err) = run(&root, &args);
        let want = format!("tests/render/invalid/doc.decl: error [E6004] at doc: bound document is not well-formed YAML: {file}: {message}\n");
        if code != 1 || err != want {
            failures.push(format!("{file}: exit {code}, {}", first_diff(&want, &err)));
        }
    }
    assert!(
        failures.is_empty(),
        "invalid-document failures:\n  {}",
        failures.join("\n  ")
    );
}
