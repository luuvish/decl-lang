//! `decl check` / `decl evaluate` / `decl validate` / `decl fmt` (cli.ts). Output is byte-identical to
//! the reference implementation's CLI so the three implementations can
//! be diffed (tests/parity/differential.py).
use crate::checker::check_module;
use crate::conformance::judge_corpus;
use crate::fmt::format;
use crate::module::{load_modules, run_universe, Bind, LoadResult};
use crate::pipeline::run_pipeline;
use crate::package::{open_package_universe, verify_lock};
use crate::parse::parse_source;
use crate::semantics::{json_str, read_json, Diag};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn usage() -> i32 {
    eprintln!("usage:\n  decl check <file>... [--json]\n  decl evaluate <file> [--root <name>] [--json]\n  decl validate <dir>\n  decl validate <file> [--input name=doc.json] [--expect-errors E1,E2] [--json]\n  decl fmt <file>... [--check]");
    2
}

/// the module graph of an entry file inside its package universe
/// (manifest and lock diagnostics first), as the reference CLI opens it
pub fn open_universe(file: &str) -> LoadResult {
    let abs = std::path::absolute(file).unwrap_or_else(|_| PathBuf::from(file));
    let pkg = open_package_universe(&abs);
    let mut diags: Vec<Diag> = vec![];
    if let Some(u) = &pkg {
        diags.extend(u.diags.clone());
        diags.extend(verify_lock(u));
    }
    let r = load_modules(&abs, pkg.as_ref().map(|u| &u.resolver), None);
    diags.extend(r.diags);
    LoadResult { modules: r.modules, entry: r.entry, diags }
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
    let r = open_universe(file);
    let Some(entry) = r.entry else { return (1, None, r.diags) };
    if !r.diags.is_empty() {
        return (1, None, r.diags);
    }
    let checks: Vec<(String, Diag)> = r.modules.iter().flat_map(|m| {
        let path = m.path.display().to_string();
        check_module(&m.decls, Some(m.env.clone())).into_iter().map(move |d| (path.clone(), d)).collect::<Vec<_>>()
    }).collect();
    if !checks.is_empty() {
        return (1, None, checks.into_iter().map(|(_, d)| d).collect());
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

/// static checks, then evaluation (optionally binding one input document);
/// Err carries the parse-error count
pub fn validate_file(file: &str, input: Option<&str>) -> Result<Vec<Diag>, usize> {
    let src = std::fs::read_to_string(file).unwrap_or_default();
    let parsed = parse_source(&src);
    if !parsed.errors.is_empty() {
        return Err(parsed.errors.len());
    }
    let checks = check_module(&parsed.decls, None);
    let mut diags = checks.clone();
    if checks.is_empty() {
        if let Some(spec) = input {
            let r = open_universe(file);
            if let (Some(entry), Some((name, path))) = (r.entry, spec.split_once('=')) {
                let text = std::fs::read_to_string(path).unwrap_or_default();
                let mut binds = vec![];
                if let Ok(raw) = read_json(&text) {
                    binds.push(Bind { input: name.to_string(), raw });
                }
                diags.extend(run_universe(&r.modules, &entry, binds).1);
            }
        } else {
            diags.extend(run_pipeline(&parsed.decls).diags);
        }
    }
    Ok(diags)
}

/// `decl check`: load each entry (following imports), report load
/// diagnostics and every module's static findings, tagged with their file
pub fn check_files(paths: &[String]) -> Vec<(String, Diag)> {
    let mut out = vec![];
    for f in paths {
        let r = open_universe(f);
        out.extend(r.diags.into_iter().map(|d| (f.clone(), d)));
        for m in &r.modules {
            let path = m.path.display().to_string();
            out.extend(check_module(&m.decls, Some(m.env.clone())).into_iter().map(|d| (path.clone(), d)));
        }
    }
    out
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
        "check" => {
            if pos.is_empty() {
                return usage();
            }
            let diags = check_files(&pos);
            for (file, d) in &diags {
                print_diag(file, d, json, &mut collected);
            }
            if diags.is_empty() {
                eprintln!("ok: {} entry file(s) check clean", pos.len());
            }
            if json {
                println!("[{}]", collected.join(","));
            }
            if diags.is_empty() { 0 } else { 1 }
        }
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
                let abs = std::path::absolute(tp).unwrap_or_else(|_| tp.to_path_buf());
                let (mut ok, mut fail) = (0, 0);
                for v in judge_corpus(&abs) {
                    if v.ok {
                        ok += 1;
                    } else {
                        fail += 1;
                        eprintln!("FAIL {} {}", v.file.display(), v.detail);
                    }
                }
                eprintln!("{ok} ok, {fail} failed");
                if fail > 0 { 1 } else { 0 }
            } else {
                let diags = match validate_file(target, flags.get("input").map(|s| s.as_str())) {
                    Ok(d) => d,
                    Err(n) => {
                        eprintln!("{target}: {n} parse error(s)");
                        return 1;
                    }
                };
                for d in &diags {
                    print_diag(target, d, json, &mut collected);
                }
                if json {
                    println!("[{}]", collected.join(","));
                }
                let err_codes: Vec<String> = diags.iter().filter(|d| d.severity == "error").map(|d| d.code.clone().unwrap_or_default()).collect();
                if let Some(expect) = flags.get("expect-errors") {
                    let want: Vec<String> = expect.split(',').map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect();
                    let missing: Vec<&String> = want.iter().filter(|w| !err_codes.contains(w)).collect();
                    let extra: Vec<&String> = err_codes.iter().filter(|c| !want.contains(c)).collect();
                    if !missing.is_empty() || !extra.is_empty() {
                        if !missing.is_empty() {
                            eprintln!("expected error(s) not reported: {}", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
                        }
                        if !extra.is_empty() {
                            eprintln!("unexpected error(s): {}", extra.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
                        }
                        return 1;
                    }
                    eprintln!("ok: expected errors reported ({})", if want.is_empty() { "none".to_string() } else { want.join(", ") });
                    return 0;
                }
                if err_codes.is_empty() { 0 } else { 1 }
            }
        }
        "fmt" => {
            if pos.is_empty() {
                return usage();
            }
            let (mut changed, mut bad) = (0, 0);
            for f in &pos {
                let src = std::fs::read_to_string(f).unwrap_or_default();
                let out = match format(&src) {
                    Ok(o) => o,
                    Err(e) => {
                        eprintln!("{f}: {e}");
                        bad += 1;
                        continue;
                    }
                };
                if out != src {
                    changed += 1;
                    if flags.contains_key("check") {
                        eprintln!("would reformat {f}");
                    } else {
                        let _ = std::fs::write(f, out);
                        eprintln!("reformatted {f}");
                    }
                }
            }
            if bad > 0 || (flags.contains_key("check") && changed > 0) { 1 } else { 0 }
        }
        _ => usage(),
    }
}
