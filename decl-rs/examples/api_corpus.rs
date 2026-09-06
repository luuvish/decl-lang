//! The API corpus (tests/api/cases.json) through the crate's high-level
//! API — `evaluate`, `check`, `validate`, `format_source` — every case run
//! from the repository root and the answers printed as one JSON array in
//! the form tests/api/README.md fixes: what the parity harness diffs across
//! the three drivers, and what the suite (tests/e2e.rs) compares with
//! tests/api/expected.json.
//!
//!     cargo run --release --example api_corpus
use decl_lang::semantics::{js_num_str, json_str, read_json, Value};
use decl_lang::{
    check, evaluate, format_source, render, validate, Diagnostic, Document, EvaluateOptions,
    RenderOptions, Rendered, TemplateSource,
};
use std::path::{Path, PathBuf};

/// the repository root: the crate's parent
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .unwrap()
}

/// a raw JSON value (`read_json`) as compact JSON text
pub fn json_of(v: &Value) -> String {
    match v {
        Value::Null | Value::Undef | Value::Absent => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => js_num_str(*f),
        Value::Str(s) => json_str(s),
        Value::JArr(items) => format!(
            "[{}]",
            items.iter().map(json_of).collect::<Vec<_>>().join(",")
        ),
        Value::JObj(es) => format!(
            "{{{}}}",
            es.iter()
                .map(|(k, x)| format!("{}:{}", json_str(k), json_of(x)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => json_str(&format!("{other:?}")),
    }
}

/// structural equality of two raw JSON values; a number compares by value
/// (6 and 6.0 are the same number)
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(i), Value::Float(f)) | (Value::Float(f), Value::Int(i)) => {
            i.to_string().parse::<f64>().ok() == Some(*f)
        }
        (Value::JArr(x), Value::JArr(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| json_eq(p, q))
        }
        (Value::JObj(x), Value::JObj(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k, p), (l, q))| k == l && json_eq(p, q))
        }
        _ => false,
    }
}

pub fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::JObj(es) => es.iter().find(|(k, _)| k == key).map(|(_, x)| x),
        _ => None,
    }
}

fn text(v: &Value) -> &str {
    match v {
        Value::Str(s) => s,
        _ => panic!("a string was expected"),
    }
}

/// a document to bind: a file named by its path, or the value itself
fn document(spec: &Value) -> Document {
    match get(spec, "file") {
        Some(f) => Document::File(PathBuf::from(text(f))),
        None => Document::Json(json_of(get(spec, "json").expect("file or json"))),
    }
}

fn inputs_of(case: &Value) -> Vec<(String, Document)> {
    match get(case, "inputs") {
        Some(Value::JObj(es)) => es.iter().map(|(k, v)| (k.clone(), document(v))).collect(),
        _ => vec![],
    }
}

fn diag_json(d: &Diagnostic) -> String {
    let mut fields = vec![format!("\"file\":{}", json_str(&d.file))];
    if let Some(c) = &d.code {
        fields.push(format!("\"code\":{}", json_str(c)));
    }
    if let Some(i) = &d.id {
        fields.push(format!("\"id\":{}", json_str(i)));
    }
    fields.push(format!("\"severity\":{}", json_str(&d.severity)));
    fields.push(format!("\"message\":{}", json_str(&d.message)));
    fields.push(format!("\"path\":{}", json_str(&d.path)));
    format!("{{{}}}", fields.join(","))
}

fn diags_json(ds: &[Diagnostic]) -> String {
    format!(
        "[{}]",
        ds.iter().map(diag_json).collect::<Vec<_>>().join(",")
    )
}

fn run_case(case: &Value) -> String {
    let name = json_str(text(get(case, "name").unwrap()));
    let answer: Result<String, decl_lang::DeclError> = if let Some(m) = get(case, "evaluate") {
        let outputs: Vec<String> = match get(case, "outputs") {
            Some(Value::JArr(xs)) => xs.iter().map(|x| text(x).to_string()).collect(),
            _ => vec![],
        };
        evaluate(
            text(m),
            &EvaluateOptions {
                inputs: inputs_of(case),
                outputs,
            },
        )
        .map(|roots| {
            format!(
                "{{{}}}",
                roots
                    .iter()
                    .map(|(k, v)| format!("{}:{v}", json_str(k)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
    } else if let Some(m) = get(case, "render") {
        let outputs: Vec<String> = match get(case, "outputs") {
            Some(Value::JArr(xs)) => xs.iter().map(|x| text(x).to_string()).collect(),
            _ => vec![],
        };
        let yaml = get(case, "format").map(|f| text(f) == "yaml");
        let indent = get(case, "indent").and_then(|i| match i {
            Value::Int(n) => n.to_string().parse::<usize>().ok(),
            _ => None,
        });
        let templates: Vec<(String, TemplateSource)> = match get(case, "templates") {
            Some(Value::JObj(es)) => es
                .iter()
                .map(|(k, v)| {
                    let src = match get(v, "file") {
                        Some(f) => TemplateSource::File(std::path::PathBuf::from(text(f))),
                        None => TemplateSource::Text(text(get(v, "text").unwrap()).to_string()),
                    };
                    (k.clone(), src)
                })
                .collect(),
            _ => vec![],
        };
        render(
            text(m),
            &RenderOptions {
                inputs: inputs_of(case),
                outputs,
                yaml,
                indent,
                templates,
            },
        )
        .map(|roots| {
            format!(
                "{{{}}}",
                roots
                    .iter()
                    .map(|(k, v)| {
                        let value = match v {
                            Rendered::Text(t) => json_str(t),
                            Rendered::Files(files) => format!(
                                "{{{}}}",
                                files
                                    .iter()
                                    .map(|(p, t)| format!("{}:{}", json_str(p), json_str(t)))
                                    .collect::<Vec<_>>()
                                    .join(",")
                            ),
                        };
                        format!("{}:{value}", json_str(k))
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
    } else if let Some(Value::JArr(files)) = get(case, "check") {
        let names: Vec<String> = files.iter().map(|f| text(f).to_string()).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        Ok(diags_json(&check(&refs)))
    } else if let Some(m) = get(case, "validate") {
        validate(text(m), &inputs_of(case)).map(|ds| diags_json(&ds))
    } else if let Some(t) = get(case, "format_source") {
        format_source(text(t)).map(|s| json_str(&s))
    } else {
        panic!("unknown call in {name}")
    };
    match answer {
        Ok(value) => format!("{{\"name\":{name},\"ok\":true,\"value\":{value}}}"),
        Err(e) => format!(
            "{{\"name\":{name},\"ok\":false,\"message\":{},\"diagnostics\":{}}}",
            json_str(&e.message),
            diags_json(&e.diagnostics)
        ),
    }
}

/// every case's answer, as one JSON array
pub fn run() -> String {
    let root = repo_root();
    std::env::set_current_dir(&root).unwrap();
    let text = std::fs::read_to_string(root.join("tests/api/cases.json")).unwrap();
    let Ok(Value::JArr(cases)) = read_json(&text) else {
        panic!("cases.json is a list")
    };
    format!(
        "[{}]",
        cases.iter().map(run_case).collect::<Vec<_>>().join(",\n")
    )
}

fn main() {
    println!("{}", run());
}
