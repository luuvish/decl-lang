//! `decl check` / `decl evaluate` / `decl validate` / `decl fmt` (cli.ts). Output is byte-identical to
//! the reference implementation's CLI so the three implementations can
//! be diffed (tests/parity/differential.py).
use crate::ast::DeclBody;
use crate::checker::check_module;
use crate::conformance::judge_corpus;
use crate::fmt::format;
use crate::module::{load_modules, run_universe, Bind, LoadResult, Module};
use crate::package::{open_package_universe, verify_lock};
use crate::render::{
    absolute, declared_form, emit_root, layout, resolve_in, Emission, Emitted, Form,
};
use crate::semantics::{json_str, read_json, Diag, Value};
use crate::yaml::{is_yaml_path, read_yaml, to_json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn usage() -> i32 {
    eprintln!("usage:\n  decl --version\n  decl check <files...>\n  decl evaluate <file> [--input name=doc.(json|yaml)]... [--output name[=file|dir|-]]...\n                       [--format json|yaml] [--indent n | --pretty] [--template [root=]path]...\n  decl validate <dir>\n  decl validate <file> [--input name=doc.(json|yaml)]... [--expect-errors E1,E2]\n  decl fmt <files...> [--check]\n  decl repl [file.decl] [--input name=doc.(json|yaml)]... [--script session.txt | --script -] [--compact]\n  (check / validate accept --json: diagnostics as a JSON array on stdout)");
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
    LoadResult {
        modules: r.modules,
        entry: r.entry,
        diags,
    }
}

fn print_diag(file: &str, d: &Diag, json: bool, collected: &mut Vec<String>) {
    if json {
        collected.push(d.to_json(Some(file)));
        return;
    }
    eprintln!(
        "{file}: {}{}{}{}: {}",
        d.severity,
        d.code
            .as_ref()
            .map(|c| format!(" [{c}]"))
            .unwrap_or_default(),
        d.id.as_ref().map(|i| format!(" {i}")).unwrap_or_default(),
        if d.path.is_empty() {
            String::new()
        } else {
            format!(" at {}", d.path)
        },
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
pub fn input_binds(
    modules: &[Rc<Module>],
    specs: &[String],
) -> Result<Vec<Bind>, (i32, Option<Diag>)> {
    let doc_error = |name: &str, message: String| -> (i32, Option<Diag>) {
        (
            1,
            Some(Diag {
                severity: "error".into(),
                id: None,
                message,
                path: name.to_string(),
                code: Some("E6004".into()),
                loc: None,
                by: None,
            }),
        )
    };
    let mut binds = vec![];
    for spec in specs {
        let Some((name, file)) = spec.split_once('=') else {
            eprintln!("--input expects name=doc.json, got {spec}");
            return Err((2, None));
        };
        let Some(module) = modules
            .iter()
            .find(|m| m.env.inputs.borrow().contains_key(name))
        else {
            eprintln!("no input named {name}");
            return Err((2, None));
        };
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => {
                return Err(doc_error(
                    name,
                    format!("bound document cannot be read: {file}"),
                ))
            }
        };
        // `name=doc.yaml` is read as YAML by its extension (docs/tooling/05_render.md §2)
        let raw = if is_yaml_path(file) {
            match read_yaml(&text) {
                Ok(v) => v,
                Err(e) => {
                    return Err(doc_error(
                        name,
                        format!("bound document is not well-formed YAML: {file}: {e}"),
                    ));
                }
            }
        } else {
            match read_json(&text) {
                Ok(v) => v,
                Err(_) => {
                    return Err(doc_error(
                        name,
                        format!("bound document is not well-formed JSON: {file}"),
                    ));
                }
            }
        };
        binds.push(Bind {
            module: Some(module.clone()),
            input: name.to_string(),
            raw,
        });
    }
    Ok(binds)
}

/// `decl evaluate`: (exit code, the document for stdout, diagnostics tagged
/// with the file each is reported against, bare stderr lines to print
/// after them). What to emit, and where (§5.5): each `--output name[=file]`
/// names a root — an output, or an input bound by --input or demanded
/// through its fallback — and the file its document goes to (stdout
/// without one); with no --output, the entry module's exported outputs, as
/// one object keyed by name, on stdout
/// the overrides of the declared forms: `--format`, `--indent` / `--pretty`
/// (docs/tooling/05_render.md §3.4); `json` is the --json report
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// `--format yaml`
    pub yaml: Option<bool>,
    /// `--indent n`, or 2 for `--pretty`
    pub indent: Option<usize>,
    /// `--json`: the report carries the document itself, whatever the layout
    pub json: bool,
}

/// `--format json|yaml`, `--indent n`, `--pretty`: the layout of every
/// document emitted (docs/tooling/05_render.md §3.4, §4); None for a usage error
pub fn form_overrides(flags: &HashMap<String, String>) -> Option<Overrides> {
    let json = flags.contains_key("json");
    let mut out = Overrides {
        json,
        ..Default::default()
    };
    if let Some(format) = flags.get("format") {
        if format != "json" && format != "yaml" {
            eprintln!(
                "--format expects json or yaml, got {}",
                if format == "true" { "nothing" } else { format }
            );
            return None;
        }
        if format == "yaml" && json {
            eprintln!("--json reports are JSON: it cannot be combined with --format yaml");
            return None;
        }
        out.yaml = Some(format == "yaml");
    }
    let indent = flags.get("indent");
    if indent.is_some() && flags.contains_key("pretty") {
        eprintln!("--indent and --pretty exclude each other");
        return None;
    }
    if let Some(indent) = indent {
        let ok = indent.len() <= 2
            && indent.bytes().all(|b| b.is_ascii_digit())
            && !(indent.len() == 2 && indent.starts_with('0'))
            && indent.parse::<usize>().is_ok_and(|n| n <= 16);
        if indent == "true" || !ok {
            eprintln!(
                "--indent expects an integer in 0..16, got {}",
                if indent == "true" { "nothing" } else { indent }
            );
            return None;
        }
        out.indent = Some(indent.parse().unwrap());
    } else if flags.contains_key("pretty") {
        out.indent = Some(2);
    }
    Some(out)
}

/// `decl evaluate`: (exit code, the text for stdout, the diagnostics tagged
/// with their file, the bare stderr lines that follow them); a usage error
/// is exit 2 with its line already printed
pub fn evaluate(
    file: &str,
    outputs: &[String],
    inputs: &[String],
    templates: &[String],
    over: &Overrides,
) -> (i32, Option<String>, Vec<(String, Diag)>, Vec<String>) {
    let tag = |ds: Vec<Diag>| -> Vec<(String, Diag)> {
        ds.into_iter().map(|d| (file.to_string(), d)).collect()
    };
    // what to emit, and where (§5.5, docs/tooling/05_render.md §3.2): each
    // `--output name[=file|dir|-]` names a root and where its document goes:
    // the file given, `-` for stdout, or, alone, the file the root's @render
    // declares, else stdout
    let mut targets: Vec<(String, Option<String>)> = vec![];
    for spec in outputs {
        let (name, dest) = match spec.split_once('=') {
            Some((n, f)) => (n.to_string(), Some(f.to_string())),
            None => (spec.clone(), None),
        };
        if name.is_empty() || dest.as_deref() == Some("") {
            eprintln!("--output expects name or name=file, got {spec}");
            return (2, None, vec![], vec![]);
        }
        targets.push((name, dest));
    }
    let r = open_universe(file);
    let Some(entry) = r.entry else {
        return (1, None, tag(r.diags), vec![]);
    };
    if !r.diags.is_empty() {
        return (1, None, tag(r.diags), vec![]);
    }
    let checks: Vec<(String, Diag)> = r
        .modules
        .iter()
        .flat_map(|m| {
            let path = file_tag(file, Some(entry.path.as_path()), &m.path);
            check_module(&m.decls, Some(m.env.clone()), None)
                .into_iter()
                .map(move |d| (path.clone(), d))
                .collect::<Vec<_>>()
        })
        .collect();
    if checks.iter().any(|(_, d)| d.severity == "error") {
        return (1, None, checks, vec![]);
    }
    // each target's declared form (§3): an invalid @render is E7004 at
    // emission and the root is not emitted; the others still are
    let decl_of = |n: &str| {
        r.modules
            .iter()
            .flat_map(|m| m.decls.iter())
            .find(|d| matches!(&d.body, DeclBody::Output { name, .. } if name == n))
    };
    let forms: Vec<Result<Form, String>> = targets
        .iter()
        .map(|(n, _)| decl_of(n).map(declared_form).unwrap_or(Ok(Form::default())))
        .collect();
    let module_dirs: Vec<String> = targets
        .iter()
        .map(|(n, _)| {
            let m = r
                .modules
                .iter()
                .find(|m| {
                    m.decls
                        .iter()
                        .any(|d| matches!(&d.body, DeclBody::Output { name, .. } if name == n))
                })
                .map(|m| m.path.clone())
                .unwrap_or_else(|| entry.path.clone());
            m.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".into())
        })
        .collect();
    // the templates named by --template (§3.4): `[root=]path`, `-` for
    // standard input; a root named must be emitted, and once
    let mut tpl_flags: HashMap<String, String> = HashMap::new(); // "" names every root
    for spec in templates {
        let (root, path) = match spec.split_once('=') {
            Some((r, p))
                if !r.is_empty()
                    && r.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                (r.to_string(), p.to_string())
            }
            _ => (String::new(), spec.clone()),
        };
        if path.is_empty() {
            eprintln!("--template expects [root=]path, got {spec}");
            return (2, None, vec![], vec![]);
        }
        if !root.is_empty() && !targets.iter().any(|(n, _)| *n == root) {
            eprintln!("--template {root}=: no --output {root}");
            return (2, None, vec![], vec![]);
        }
        if tpl_flags.contains_key(&root) {
            if root.is_empty() {
                eprintln!("--template given twice");
            } else {
                eprintln!("--template {root}= given twice");
            }
            return (2, None, vec![], vec![]);
        }
        tpl_flags.insert(root, path);
    }
    // destinations (§3.2, §6): one root at most to stdout; a fan-out root
    // goes to a directory, never to stdout
    let fan_out = |i: usize| -> bool { matches!(&forms[i], Ok(f) if f.each.is_some()) };
    let to_stdout = |i: usize| -> bool {
        match &targets[i].1 {
            Some(d) => d == "-",
            None => matches!(&forms[i], Ok(f) if f.file.is_none()),
        }
    };
    for (i, (n, dest)) in targets.iter().enumerate() {
        if !fan_out(i) {
            continue;
        }
        if dest.as_deref() == Some("-") {
            eprintln!("--output {n}=-: a fan-out root cannot go to stdout");
            return (2, None, vec![], vec![]);
        }
        if dest.is_none() && matches!(&forms[i], Ok(f) if f.file.is_none()) {
            eprintln!(
                "--output {n}: a fan-out root needs a directory ({n}=dir, or file in @render)"
            );
            return (2, None, vec![], vec![]);
        }
    }
    if (0..targets.len())
        .filter(|&i| !fan_out(i) && to_stdout(i))
        .count()
        > 1
    {
        eprintln!("--output: at most one document can go to stdout");
        return (2, None, vec![], vec![]);
    }
    let binds = match input_binds(&r.modules, inputs) {
        Ok(b) => b,
        Err((code, diag)) => {
            let mut all = checks;
            all.extend(diag.into_iter().map(|d| (file.to_string(), d)));
            return (code, None, all, vec![]);
        }
    };
    let (eng, diags) = run_universe(&r.modules, &entry, binds);
    let mut all = checks; // a warning of the checks (W0001) is reported with the run's
    all.extend(tag(diags.clone()));
    if diags.iter().any(|d| d.severity == "error") {
        return (1, None, all, vec![]);
    }
    let names: Vec<String> = if targets.is_empty() {
        entry
            .decls
            .iter()
            .filter(|d| d.exported)
            .filter_map(|d| match &d.body {
                DeclBody::Output { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    } else {
        targets.iter().map(|(n, _)| n.clone()).collect()
    };
    let mut notes = vec![];
    for n in &names {
        if entry.env.root(n).is_none() {
            notes.push(format!("no root named {n}"));
        }
    }
    if !notes.is_empty() {
        return (1, None, all.clone(), notes);
    }
    let doc = |n: &str| {
        read_json(&eng.serialize(&entry.env.root(n).unwrap(), n, false))
            .unwrap_or_else(|_| panic!("canonical JSON"))
    };
    let mut text = None;
    let mut bad = false;
    if targets.is_empty() {
        if names.is_empty() {
            notes.push(format!(
                "{file}: exports no output; --output <name> selects a root"
            ));
        }
        let all_docs = Value::JObj(Rc::new(names.iter().map(|n| (n.clone(), doc(n))).collect()));
        // a --json report carries the document itself, whatever the layout
        text = Some(if over.json {
            to_json(&all_docs, 0) + "\n"
        } else {
            layout(&all_docs, over.yaml.unwrap_or(false), over.indent)
        });
    } else {
        // templates are read once (§3.3): by absolute path, or standard input
        let texts: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
        let read_tpl = |abs: &str| -> Option<String> {
            if !texts.borrow().contains_key(abs) {
                let t = std::fs::read_to_string(abs).ok();
                texts.borrow_mut().insert(abs.to_string(), t);
            }
            texts.borrow().get(abs).cloned().flatten()
        };
        let mut stdin_text: Option<String> = None;
        for (i, (n, dest)) in targets.iter().enumerate() {
            let form = match &forms[i] {
                Ok(f) => f.clone(),
                Err(m) => {
                    all.push((
                        file.to_string(),
                        Diag::error(m.clone(), n.clone(), Some("E7004")),
                    ));
                    bad = true;
                    continue;
                }
            };
            // the template: the override, else the declared one, relative to the module's directory
            let given = tpl_flags.get(n).or_else(|| tpl_flags.get(""));
            let template: Option<(String, String, String)> = match given {
                Some(g) if g == "-" => {
                    if stdin_text.is_none() {
                        let mut t = String::new();
                        if std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).is_err() {
                            all.push((
                                "-".to_string(),
                                Diag::error("template cannot be read", n.clone(), Some("E7003")),
                            ));
                            bad = true;
                            continue;
                        }
                        stdin_text = Some(t);
                    }
                    let cwd = std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| ".".into());
                    Some(("-".to_string(), stdin_text.clone().unwrap(), cwd))
                }
                Some(g) => {
                    let abs = absolute(g);
                    match read_tpl(&abs) {
                        Some(t) => Some((g.clone(), t, parent_dir(&abs))),
                        None => {
                            all.push((
                                g.clone(),
                                Diag::error("template cannot be read", n.clone(), Some("E7003")),
                            ));
                            bad = true;
                            continue;
                        }
                    }
                }
                None => match &form.template {
                    Some(t) => {
                        let abs = resolve_in(&module_dirs[i], t);
                        match read_tpl(&abs) {
                            Some(text) => Some((t.clone(), text, parent_dir(&abs))),
                            None => {
                                all.push((
                                    t.clone(),
                                    Diag::error(
                                        "template cannot be read",
                                        n.clone(),
                                        Some("E7003"),
                                    ),
                                ));
                                bad = true;
                                continue;
                            }
                        }
                    }
                    None => None,
                },
            };
            let has_template = template.is_some();
            let em = match emit_root(&Emission {
                eng: eng.clone(),
                menv: entry.env.clone(),
                root_name: n.clone(),
                value: entry.env.root(n).unwrap(),
                form: form.clone(),
                yaml: over.yaml,
                indent: over.indent,
                template,
                read_template: &read_tpl,
            }) {
                Ok(e) => e,
                Err(e) => {
                    all.push((e.file.clone().unwrap_or_else(|| file.to_string()), e.diag()));
                    bad = true;
                    continue;
                }
            };
            let dest = match dest {
                Some(d) if d == "-" => None,
                Some(d) => Some(d.clone()),
                None => form.file.clone(),
            };
            match em {
                Emitted::Many(files) => {
                    // a fan-out's files, in element order, under the directory
                    let dir = dest.unwrap();
                    for (rel, body) in files {
                        let path = absolute(&format!("{dir}/{rel}"));
                        if let Some(d) = std::path::Path::new(&path).parent() {
                            let _ = std::fs::create_dir_all(d);
                        }
                        if std::fs::write(&path, body).is_err() {
                            notes.push(format!("cannot write {path}"));
                            return (1, None, all.clone(), notes);
                        }
                    }
                }
                Emitted::One(body) => match dest {
                    None => {
                        // the report's value: the document itself, or a template's text as a string
                        text = Some(if over.json {
                            if has_template {
                                json_str(&body) + "\n"
                            } else {
                                to_json(&doc(n), 0) + "\n"
                            }
                        } else {
                            body
                        })
                    }
                    Some(path) => {
                        if let Some(dir) = std::path::Path::new(&path).parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        if std::fs::write(&path, body).is_err() {
                            notes.push(format!("cannot write {path}"));
                            return (1, None, all.clone(), notes);
                        }
                    }
                },
            }
        }
    }
    (if bad { 1 } else { 0 }, text, all.clone(), notes)
}

fn parent_dir(p: &str) -> String {
    std::path::Path::new(p)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".into())
}

/// single-file validation, module-aware like `check` and `evaluate`: load
/// the universe, check every module, then evaluate with the `--input`
/// documents bound (none bound is fine: fallbacks apply). Diagnostics come
/// tagged with the file each is reported against; Err carries a usage exit
/// code as a negative
pub fn validate_file(file: &str, inputs: &[String]) -> Result<Vec<(String, Diag)>, i64> {
    let r = open_universe(file);
    let mut diags: Vec<(String, Diag)> = r
        .diags
        .iter()
        .map(|d| (file.to_string(), d.clone()))
        .collect();
    let Some(entry) = r.entry else {
        return Ok(diags);
    };
    if !diags.is_empty() {
        return Ok(diags);
    }
    let checks: Vec<(String, Diag)> = r
        .modules
        .iter()
        .flat_map(|m| {
            let path = file_tag(file, Some(entry.path.as_path()), &m.path);
            check_module(&m.decls, Some(m.env.clone()), None)
                .into_iter()
                .map(move |d| (path.clone(), d))
                .collect::<Vec<_>>()
        })
        .collect();
    if checks.iter().any(|(_, d)| d.severity == "error") {
        return Ok(checks);
    }
    diags.extend(checks); // a warning (W0001) is reported beside the run's diagnostics
    let binds = match input_binds(&r.modules, inputs) {
        Ok(b) => b,
        Err((_, Some(d))) => {
            diags.push((file.to_string(), d));
            return Ok(diags);
        }
        Err((code, None)) => return Err(-(code as i64)),
    };
    diags.extend(
        run_universe(&r.modules, &entry, binds)
            .1
            .into_iter()
            .map(|d| (file.to_string(), d)),
    );
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
            out.extend(
                check_module(&m.decls, Some(m.env.clone()), None)
                    .into_iter()
                    .map(|d| (path.clone(), d)),
            );
        }
    }
    out
}

/// the file a diagnostic is reported against: the entry module by the path
/// given on the command line, any other module by its absolute path
pub fn file_tag(given: &str, entry: Option<&Path>, module: &Path) -> String {
    if entry == Some(module) {
        given.to_string()
    } else {
        module.display().to_string()
    }
}

/// the command line: returns the process exit code
pub fn main(args: Vec<String>) -> i32 {
    let Some(cmd) = args.first().cloned() else {
        return usage();
    };
    // `decl --version`: the package's version, the same string on every registry
    if cmd == "--version" {
        println!("decl {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    // `decl repl`: its own argument syntax (docs/tooling/02_repl.md)
    if cmd == "repl" {
        return crate::repl::run_repl(args[1..].to_vec());
    }
    let mut flags: HashMap<String, String> = HashMap::new();
    let mut input_flags: Vec<String> = vec![]; // --input name=doc.json, repeatable
    let mut output_flags: Vec<String> = vec![]; // --output name[=file|dir|-], repeatable
    let mut template_flags: Vec<String> = vec![]; // --template [root=]path, repeatable
    let mut pos: Vec<String> = vec![];
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if let Some(name) = a.strip_prefix("--") {
            if [
                "output",
                "input",
                "expect-errors",
                "format",
                "indent",
                "template",
            ]
            .contains(&name)
                && i + 1 < args.len()
                && !args[i + 1].starts_with("--")
            {
                if name == "input" {
                    input_flags.push(args[i + 1].clone());
                } else if name == "output" {
                    output_flags.push(args[i + 1].clone());
                } else if name == "template" {
                    template_flags.push(args[i + 1].clone());
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
            if diags.iter().any(|(_, d)| d.severity == "error") {
                1 // a warning (W0001) is reported, not a failure
            } else {
                0
            }
        }
        "evaluate" => {
            let Some(f) = pos.first() else { return usage() };
            let Some(over) = form_overrides(&flags) else {
                return 2;
            };
            let (code, text, diags, notes) =
                evaluate(f, &output_flags, &input_flags, &template_flags, &over);
            if code == 2 {
                return 2; // a usage error: already printed, no report
            }
            for (file, d) in &diags {
                print_diag(file, d, json, &mut collected);
            }
            for n in &notes {
                eprintln!("{n}");
            }
            if json {
                println!(
                    "{{\"ok\":{},\"value\":{},\"diagnostics\":[{}]}}",
                    code == 0,
                    text.as_deref().map(str::trim_end).unwrap_or("null"),
                    collected.join(",")
                );
            } else if let Some(t) = text {
                print!("{t}");
            }
            code
        }
        "validate" => {
            let Some(target) = pos.first() else {
                return usage();
            };
            if flags.get("expect-errors").map(String::as_str) == Some("true") {
                eprintln!("--expect-errors expects a list of codes: E1,E2");
                return 2;
            }
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
                if fail > 0 {
                    1
                } else {
                    0
                }
            } else {
                let diags = match validate_file(target, &input_flags) {
                    Ok(d) => d,
                    Err(n) => return (-n) as i32,
                };
                for (file, d) in &diags {
                    print_diag(file, d, json, &mut collected);
                }
                if json {
                    println!("[{}]", collected.join(","));
                }
                let err_codes: Vec<String> = diags
                    .iter()
                    .filter(|(_, d)| d.severity == "error")
                    .map(|(_, d)| d.code.clone().unwrap_or_default())
                    .collect();
                if let Some(expect) = flags.get("expect-errors") {
                    let want: Vec<String> = expect
                        .split(',')
                        .map(|w| w.trim().to_string())
                        .filter(|w| !w.is_empty())
                        .collect();
                    let missing: Vec<&String> =
                        want.iter().filter(|w| !err_codes.contains(w)).collect();
                    let extra: Vec<&String> =
                        err_codes.iter().filter(|c| !want.contains(c)).collect();
                    if !missing.is_empty() || !extra.is_empty() {
                        if !missing.is_empty() {
                            eprintln!(
                                "expected error(s) not reported: {}",
                                missing
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                        if !extra.is_empty() {
                            eprintln!(
                                "unexpected error(s): {}",
                                extra
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                        return 1;
                    }
                    eprintln!(
                        "ok: expected errors reported ({})",
                        if want.is_empty() {
                            "none".to_string()
                        } else {
                            want.join(", ")
                        }
                    );
                    return 0;
                }
                if err_codes.is_empty() {
                    0
                } else {
                    1
                }
            }
        }
        "fmt" => {
            if pos.is_empty() {
                return usage();
            }
            let (mut changed, mut bad) = (0, 0);
            for f in &pos {
                let Ok(src) = std::fs::read_to_string(f) else {
                    eprintln!("{f}: cannot be read");
                    bad += 1;
                    continue;
                };
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
            if bad > 0 || (flags.contains_key("check") && changed > 0) {
                1
            } else {
                0
            }
        }
        _ => usage(),
    }
}
