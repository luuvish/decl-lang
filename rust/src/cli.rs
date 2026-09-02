//! `decl evaluate` / `decl validate` (cli.ts). Output is byte-identical to
//! the reference implementation's CLI so the three implementations can
//! be diffed (tests/parity/differential.py).
use crate::module::{load_modules, run_pipeline, run_universe, Bind};
use crate::parse::parse_source;
use crate::semantics::{json_str, read_json, Diag};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn usage() -> i32 {
    eprintln!("usage:\n  decl evaluate <file> [--root <name>] [--json]\n  decl validate <dir>\n  decl validate <file> [--input name=doc.json] [--json]");
    2
}

fn print_diag(file: &str, d: &Diag, json: bool, collected: &mut Vec<String>) {
    if json {
        collected.push(d.to_json(Some(file)));
        return;
    }
    eprintln!(
        "{file}: {}{}{}{}: {}",
        d.severity,
        d.code.as_ref().map(|c| format!(" [{c}]")).unwrap_or_default(),
        d.id.as_ref().map(|i| format!(" {i}")).unwrap_or_default(),
        if d.path.is_empty() { String::new() } else { format!(" at {}", d.path) },
        d.message
    );
}

/// (exit code, canonical JSON text, diagnostics)
pub fn evaluate(file: &str, root: Option<&str>) -> (i32, Option<String>, Vec<Diag>) {
    let r = load_modules(Path::new(file));
    let Some(entry) = r.entry else { return (1, None, r.diags) };
    if !r.diags.is_empty() {
        return (1, None, r.diags);
    }
    let (eng, diags) = run_universe(&r.modules, &entry, vec![]);
    if diags.iter().any(|d| d.severity == "error") {
        return (1, None, diags);
    }
    let names: Vec<String> = match root {
        Some(n) => vec![n.to_string()],
        None => r.modules.iter().flat_map(|m| m.env.outputs.borrow().iter().map(|(n, _, _)| n.clone()).collect::<Vec<_>>()).collect(),
    };
    let mut pieces = vec![];
    for n in &names {
        let Some(v) = entry.env.root(n) else {
            let mut d = diags.clone();
            d.push(Diag::error(format!("no output named {n}"), String::new(), None));
            return (1, None, d);
        };
        pieces.push(format!("{}:{}", json_str(n), eng.serialize(&v, n)));
    }
    if let (Some(n), 1) = (root, names.len()) {
        return (0, Some(eng.serialize(&entry.env.root(n).unwrap(), n)), diags);
    }
    (0, Some(format!("{{{}}}", pieces.join(","))), diags)
}

/// evaluate a module's outputs, optionally binding one input document
pub fn validate_file(file: &str, input: Option<&str>) -> Vec<Diag> {
    let r = load_modules(Path::new(file));
    let Some(entry) = r.entry else { return r.diags };
    if !r.diags.is_empty() {
        return r.diags;
    }
    let mut binds = vec![];
    if let Some(spec) = input {
        if let Some((name, path)) = spec.split_once('=') {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if let Ok(raw) = read_json(&text) {
                binds.push(Bind { input: name.to_string(), raw });
            }
        }
    }
    run_universe(&r.modules, &entry, binds).1
}

/// runtime-level judgment of a corpus fixture: valid fixtures evaluate
/// clean, parsing-phase fixtures fail to parse, binding-phase fixtures
/// report their code; checking-phase fixtures need the static checker
fn judge_fixture(file: &Path, is_valid: bool) -> (Option<bool>, String) {
    let src = std::fs::read_to_string(file).unwrap_or_default();
    let meta = |key: &str| -> Option<String> {
        src.lines().find_map(|l| l.strip_prefix(&format!("// @{key}:")).map(|v| v.trim().to_string()))
    };
    let phase = meta("expect-phase");
    let want = meta("expect-error").unwrap_or_default();
    let parsed = parse_source(&src);
    if is_valid {
        if !parsed.errors.is_empty() {
            return (Some(false), format!("{} parse errors", parsed.errors.len()));
        }
        let (env, _) = run_pipeline(&parsed.decls);
        let errs: Vec<Diag> = env.diagnostics_vec().into_iter().filter(|d| d.severity == "error").collect();
        return (Some(errs.is_empty()), errs.iter().take(2).map(|d| d.to_json(None)).collect::<Vec<_>>().join(" "));
    }
    match phase.as_deref() {
        Some("parsing") => (Some(!parsed.errors.is_empty()), "expected parse errors".into()),
        Some("binding") => {
            if !parsed.errors.is_empty() {
                return (Some(false), "parse errors".into());
            }
            let (env, _) = run_pipeline(&parsed.decls);
            let diags = env.diagnostics_vec();
            (Some(diags.iter().any(|d| d.code.as_deref() == Some(want.as_str()))), diags.iter().take(3).map(|d| d.to_json(None)).collect::<Vec<_>>().join(" "))
        }
        other => (None, format!("phase {other:?} needs the static checker")),
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().map(|x| x == "decl").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// the command line: returns the process exit code
pub fn main(args: Vec<String>) -> i32 {
    let Some(cmd) = args.first().cloned() else { return usage() };
    let mut flags: HashMap<String, String> = HashMap::new();
    let mut pos: Vec<String> = vec![];
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if let Some(name) = a.strip_prefix("--") {
            if ["root", "input", "expect-errors"].contains(&name) && i + 1 < args.len() && !args[i + 1].starts_with("--") {
                flags.insert(name.to_string(), args[i + 1].clone());
                i += 2;
                continue;
            }
            flags.insert(name.to_string(), "true".into());
        } else {
            pos.push(a.clone());
        }
        i += 1;
    }
    let json = flags.contains_key("json");
    let mut collected: Vec<String> = vec![];
    match cmd.as_str() {
        "evaluate" => {
            let Some(f) = pos.first() else { return usage() };
            let (code, text, diags) = evaluate(f, flags.get("root").map(|s| s.as_str()));
            for d in &diags {
                print_diag(f, d, json, &mut collected);
            }
            if json {
                println!("{{\"ok\":{},\"value\":{},\"diagnostics\":[{}]}}", code == 0, text.clone().unwrap_or_else(|| "null".into()), collected.join(","));
            } else if let Some(t) = text {
                println!("{t}");
            }
            code
        }
        "validate" => {
            let Some(target) = pos.first() else { return usage() };
            let tp = Path::new(target);
            if tp.is_dir() {
                let mut files = vec![];
                walk(tp, &mut files);
                let (mut ok, mut fail, mut skipped) = (0, 0, 0);
                for f in files {
                    let is_valid = f.to_string_lossy().contains("/valid/");
                    match judge_fixture(&f, is_valid) {
                        (None, _) => skipped += 1,
                        (Some(true), _) => ok += 1,
                        (Some(false), detail) => {
                            fail += 1;
                            eprintln!("FAIL {} {detail}", f.display());
                        }
                    }
                }
                eprintln!("{ok} ok, {fail} failed, {skipped} skipped (checking phase)");
                if fail > 0 { 1 } else { 0 }
            } else {
                let diags = validate_file(target, flags.get("input").map(|s| s.as_str()));
                for d in &diags {
                    print_diag(target, d, json, &mut collected);
                }
                if json {
                    println!("[{}]", collected.join(","));
                }
                if diags.iter().any(|d| d.severity == "error") { 1 } else { 0 }
            }
        }
        _ => usage(),
    }
}
