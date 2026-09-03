//! `decl check` / `decl evaluate` / `decl validate` / `decl fmt` (cli.ts). Output is byte-identical to
//! the reference implementation's CLI so the three implementations can
//! be diffed (tests/parity/differential.py).
use crate::checker::check_module;
use crate::conformance::judge_corpus;
use crate::fmt::format;
use crate::module::{load_modules, run_universe, Bind, LoadResult, Module};
use crate::pipeline::run_pipeline;
use crate::package::{open_package_universe, verify_lock};
use crate::parse::parse_source;
use crate::semantics::{json_str, read_json, Diag};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn usage() -> i32 {
    eprintln!("usage:\n  decl check <file>... [--json]\n  decl evaluate <file> [--input name=doc.json]... [--root <name>] [--json]\n  decl validate <dir>\n  decl validate <file> [--input name=doc.json]... [--expect-errors E1,E2] [--json]\n  decl fmt <file>... [--check]");
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

/// the documents named by `--input`, each bound to the module that
/// declares its input (§10): `name=doc.json`; Err carries the exit code
/// of a usage error (already printed)
/// the documents named by --input, each bound to the module that declares
/// its input (§10): `name=doc.json`. A usage error (bad spec, unknown input)
/// is printed and returned as exit 2; a document that cannot be read or is
/// not well-formed JSON is returned as one E6004 diagnostic (exit 1)
pub fn input_binds(modules: &[Rc<Module>], specs: &[String]) -> Result<Vec<Bind>, (i32, Option<Diag>)> {
    let doc_error = |name: &str, message: String| -> (i32, Option<Diag>) {
        (1, Some(Diag { severity: "error".into(), id: None, message, path: name.to_string(), code: Some("E6004".into()) }))
    };
    let mut binds = vec![];
    for spec in specs {
        let Some((name, file)) = spec.split_once('=') else {
            eprintln!("--input expects name=doc.json, got {spec}");
            return Err((2, None));
        };
        let Some(module) = modules.iter().find(|m| m.env.inputs.borrow().contains_key(name)) else {
            eprintln!("no input named {name}");
            return Err((2, None));
        };
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => return Err(doc_error(name, format!("bound document cannot be read: {file}"))),
        };
        let raw = match read_json(&text) {
            Ok(v) => v,
            Err(_) => {
                return Err(doc_error(name, format!("bound document is not well-formed JSON: {file}")));
            }
        };
        binds.push(Bind { module: Some(module.clone()), input: name.to_string(), raw });
    }
    Ok(binds)
}

/// (exit code, canonical JSON text, diagnostics): bind the `--input`
/// documents, evaluate, and emit every output — or the one root `--root`
/// names (an output, or an input bound by --input / demanded through
/// its fallback)
/// `decl evaluate`: (exit code, the serialized value, diagnostics tagged
/// with the file each is reported against)
pub fn evaluate(file: &str, root: Option<&str>, inputs: &[String]) -> (i32, Option<String>, Vec<(String, Diag)>) {
    let tag = |ds: Vec<Diag>| -> Vec<(String, Diag)> { ds.into_iter().map(|d| (file.to_string(), d)).collect() };
    let r = open_universe(file);
    let Some(entry) = r.entry else { return (1, None, tag(r.diags)) };
    if !r.diags.is_empty() {
        return (1, None, tag(r.diags));
    }
    let checks: Vec<(String, Diag)> = r.modules.iter().flat_map(|m| {
        let path = file_tag(file, Some(entry.path.as_path()), &m.path);
        check_module(&m.decls, Some(m.env.clone())).into_iter().map(move |d| (path.clone(), d)).collect::<Vec<_>>()
    }).collect();
    if !checks.is_empty() {
        return (1, None, checks);
    }
    let binds = match input_binds(&r.modules, inputs) {
        Ok(b) => b,
        Err((code, diag)) => return (code, None, diag.into_iter().map(|d| (file.to_string(), d)).collect()),
    };
    let (eng, diags) = run_universe(&r.modules, &entry, binds);
    if diags.iter().any(|d| d.severity == "error") {
        return (1, None, tag(diags));
    }
    let names: Vec<String> = match root {
        Some(n) => vec![n.to_string()],
        None => r.modules.iter().flat_map(|m| m.env.outputs.borrow().iter().map(|(n, _, _)| n.clone()).collect::<Vec<_>>()).collect(),
    };
    let mut pieces = vec![];
    let mut missing = 0;
    for n in &names {
        let Some(v) = entry.env.root(n) else {
            eprintln!("no root named {n}");
            missing += 1;
            continue;
        };
        pieces.push(format!("{}:{}", json_str(n), eng.serialize(&v, n)));
    }
    if missing > 0 {
        return (1, None, tag(diags));
    }
    if let (Some(n), 1) = (root, names.len()) {
        return (0, Some(eng.serialize(&entry.env.root(n).unwrap(), n)), tag(diags));
    }
    (0, Some(format!("{{{}}}", pieces.join(","))), tag(diags))
}

/// static checks, then evaluation (binding the `--input` documents);
/// Err carries the parse-error count, or a usage exit code as a negative
pub fn validate_file(file: &str, inputs: &[String]) -> Result<Vec<Diag>, i64> {
    let src = std::fs::read_to_string(file).unwrap_or_default();
    let parsed = parse_source(&src);
    if !parsed.errors.is_empty() {
        return Err(parsed.errors.len() as i64);
    }
    let checks = check_module(&parsed.decls, None);
    let mut diags = checks.clone();
    if checks.is_empty() {
        if !inputs.is_empty() {
            let r = open_universe(file);
            if let Some(entry) = r.entry {
                let binds = match input_binds(&r.modules, inputs) {
                    Ok(b) => b,
                    Err((_, Some(d))) => return Ok(vec![d]),
                    Err((code, None)) => return Err(-(code as i64)),
                };
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
            let path = file_tag(f, r.entry.as_ref().map(|e| e.path.as_path()), &m.path);
            out.extend(check_module(&m.decls, Some(m.env.clone())).into_iter().map(|d| (path.clone(), d)));
        }
    }
    out
}

/// the file a diagnostic is reported against: the entry module by the path
/// given on the command line, any other module by its absolute path
fn file_tag(given: &str, entry: Option<&Path>, module: &Path) -> String {
    if entry == Some(module) { given.to_string() } else { module.display().to_string() }
}


/// the command line: returns the process exit code
pub fn main(args: Vec<String>) -> i32 {
    let Some(cmd) = args.first().cloned() else { return usage() };
    let mut flags: HashMap<String, String> = HashMap::new();
    let mut input_flags: Vec<String> = vec![]; // --input name=doc.json, repeatable
    let mut pos: Vec<String> = vec![];
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if let Some(name) = a.strip_prefix("--") {
            if ["root", "input", "expect-errors"].contains(&name) && i + 1 < args.len() && !args[i + 1].starts_with("--") {
                if name == "input" {
                    input_flags.push(args[i + 1].clone());
                } else {
                    flags.insert(name.to_string(), args[i + 1].clone());
                }
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
            let (code, text, diags) = evaluate(f, flags.get("root").map(|s| s.as_str()), &input_flags);
            for (file, d) in &diags {
                print_diag(file, d, json, &mut collected);
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
                let diags = match validate_file(target, &input_flags) {
                    Ok(d) => d,
                    Err(n) if n < 0 => return (-n) as i32,
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
