//! The high-level API of the crate: the operations the `decl` command line
//! offers, in the same vocabulary — `evaluate` binds inputs and returns
//! outputs, `check`, `validate`, `evaluate_source`, `format_source` — for
//! programs that would otherwise assemble parser, checker, and engine by
//! hand (those modules are public too). The npm package and the Python
//! package offer the same functions with the same semantics.
use indexmap::IndexMap;
use std::fmt;
use std::path::PathBuf;

use crate::checker::check_module;
use crate::cli::{check_files, file_tag, open_universe};
use crate::module::{run_universe, Bind, Module};
use crate::parse::parse_source;
use crate::pipeline::run_pipeline;
pub use crate::pipeline::{evaluate_source, Report};
use crate::render::{absolute, declared_form, emit_root, resolve_in, Emission, Emitted, Form};
use crate::semantics::{read_json, Diag};
use crate::yaml::{is_yaml_path, read_yaml, to_json as json_text, to_yaml as yaml_text};
use std::rc::Rc;

/// One diagnostic, in the report's field order (§12.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// the file the diagnostic is reported against: the entry module by the path given, another module by its absolute path
    pub file: String,
    /// the diagnostic code (§12), when the finding carries one
    pub code: Option<String>,
    /// the constraint's stable id (`Type.name`), for an assertion
    pub id: Option<String>,
    /// `error`, `warn`, or `info`
    pub severity: String,
    /// the message, rendered
    pub message: String,
    /// the canonical path of the value the finding concerns (§7.2); empty for a module-level finding
    pub path: String,
}

/// An operation failed; `diagnostics` carries the report (empty for a usage
/// error such as an unknown input or root).
#[derive(Clone, Debug)]
pub struct DeclError {
    /// the first diagnostic's message, or the usage error
    pub message: String,
    /// the report (§12.2), in canonical order; empty for a usage error
    pub diagnostics: Vec<Diagnostic>,
}
impl fmt::Display for DeclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for DeclError {}

/// A document to bind to an input: the path of a JSON file, or its JSON text.
#[derive(Clone, Debug)]
pub enum Document {
    /// a JSON or YAML file, by path (YAML by its extension)
    File(PathBuf),
    /// JSON text
    Json(String),
}

/// What `evaluate` binds and returns.
#[derive(Clone, Debug, Default)]
pub struct EvaluateOptions {
    /// documents to bind, by input name
    pub inputs: Vec<(String, Document)>,
    /// the roots to return — outputs, or inputs bound here or demanded
    /// through their fallback; empty: the entry module's exported outputs (§5.5)
    pub outputs: Vec<String>,
}

fn tagged(file: &str, d: &Diag) -> Diagnostic {
    Diagnostic {
        file: file.to_string(),
        code: d.code.clone(),
        id: d.id.clone(),
        severity: d.severity.clone(),
        message: d.message.clone(),
        path: d.path.clone(),
    }
}
fn fail<T>(fallback: &str, diagnostics: Vec<Diagnostic>) -> Result<T, DeclError> {
    let message = diagnostics
        .first()
        .map(|d| d.message.clone())
        .unwrap_or_else(|| fallback.to_string());
    Err(DeclError {
        message,
        diagnostics,
    })
}

// the documents to bind, each to the module that declares its input (§10)
fn bind_inputs(
    modules: &[Rc<Module>],
    file: &str,
    inputs: &[(String, Document)],
) -> Result<Vec<Bind>, DeclError> {
    let mut binds = vec![];
    for (name, doc) in inputs {
        let Some(module) = modules
            .iter()
            .find(|m| m.env.inputs.borrow().contains_key(name))
        else {
            return Err(DeclError {
                message: format!("no input named {name}"),
                diagnostics: vec![],
            });
        };
        let e6004 = |message: String| Diagnostic {
            file: file.to_string(),
            code: Some("E6004".into()),
            id: None,
            severity: "error".into(),
            message,
            path: name.clone(),
        };
        let (text, place) = match doc {
            Document::File(p) => match std::fs::read_to_string(p) {
                Ok(t) => (t, p.display().to_string()),
                Err(_) => {
                    return fail(
                        "",
                        vec![e6004(format!(
                            "bound document cannot be read: {}",
                            p.display()
                        ))],
                    )
                }
            },
            Document::Json(t) => (t.clone(), name.clone()),
        };
        // a file is YAML by its extension (docs/tooling/05_render.md §2)
        let yaml = matches!(doc, Document::File(p) if is_yaml_path(&p.display().to_string()));
        let raw = if yaml {
            match read_yaml(&text) {
                Ok(v) => v,
                Err(e) => {
                    return fail(
                        "",
                        vec![e6004(format!(
                            "bound document is not well-formed YAML: {place}: {e}"
                        ))],
                    )
                }
            }
        } else {
            match read_json(&text) {
                Ok(v) => v,
                Err(_) => {
                    return fail(
                        "",
                        vec![e6004(format!(
                            "bound document is not well-formed JSON: {place}"
                        ))],
                    )
                }
            }
        };
        binds.push(Bind {
            module: Some(module.clone()),
            input: name.clone(),
            raw,
        });
    }
    Ok(binds)
}

/// Evaluate a module: bind the input documents, run the pipeline, and return
/// the requested roots' documents (canonical JSON text) by name. Fails with
/// the diagnostics on any error-severity outcome.
pub fn evaluate(path: &str, opts: &EvaluateOptions) -> Result<IndexMap<String, String>, DeclError> {
    let r = open_universe(path);
    let Some(entry) = r.entry.clone() else {
        return fail(
            &format!("{path}: cannot be loaded"),
            r.diags.iter().map(|d| tagged(path, d)).collect(),
        );
    };
    if !r.diags.is_empty() {
        return fail("", r.diags.iter().map(|d| tagged(path, d)).collect());
    }
    let checks: Vec<Diagnostic> = r
        .modules
        .iter()
        .flat_map(|m| {
            let tag = file_tag(path, Some(entry.path.as_path()), &m.path);
            check_module(&m.decls, Some(m.env.clone()), None)
                .iter()
                .map(|d| tagged(&tag, d))
                .collect::<Vec<_>>()
        })
        .collect();
    if checks.iter().any(|d| d.severity == "error") {
        return fail("", checks);
    }
    let binds = bind_inputs(&r.modules, path, &opts.inputs)?;
    let (eng, diags) = run_universe(&r.modules, &entry, binds);
    let report: Vec<Diagnostic> = diags.iter().map(|d| tagged(path, d)).collect();
    if report.iter().any(|d| d.severity == "error") {
        return fail("", report);
    }
    let names: Vec<String> = if opts.outputs.is_empty() {
        entry
            .decls
            .iter()
            .filter(|d| d.exported)
            .filter_map(|d| match &d.body {
                crate::ast::DeclBody::Output { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    } else {
        opts.outputs.clone()
    };
    let mut out = IndexMap::new();
    for n in &names {
        let Some(v) = entry.env.root(n) else {
            return Err(DeclError {
                message: format!("no root named {n}"),
                diagnostics: report,
            });
        };
        out.insert(n.clone(), eng.serialize(&v, n, false));
    }
    Ok(out)
}

/// a template for `render`: the path of a template file, or its text
#[derive(Debug, Clone)]
pub enum TemplateSource {
    /// a template file, by path
    File(PathBuf),
    /// the template's text
    Text(String),
}
/// the options of `render` (docs/tooling/05_render.md §7)
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// documents to bind, by input name
    pub inputs: Vec<(String, Document)>,
    /// the roots to return; empty: the entry module's exported outputs
    pub outputs: Vec<String>,
    /// `Some(true)` for YAML, `Some(false)` for JSON: overrides every root's declared format
    pub yaml: Option<bool>,
    /// the layout: overrides every root's declared indent
    pub indent: Option<usize>,
    /// templates by root name (`"*"` for every root without one of its own)
    pub templates: Vec<(String, TemplateSource)>,
}
/// a rendered root: one text, or the files of a fan-out root by path
#[derive(Debug, Clone, PartialEq)]
pub enum Rendered {
    /// one text
    Text(String),
    /// the files of a fan-out root, by path, in element order
    Files(IndexMap<String, String>),
}

/// Render a module's roots in the forms their `@render` annotations declare
/// (docs/tooling/05_render.md), with the options as overrides: bind the
/// input documents, evaluate, and return each root's text — or, for a
/// fan-out root, its files by path. Nothing is written to disk. Fails with
/// the diagnostics on any error-severity outcome.
pub fn render(path: &str, opts: &RenderOptions) -> Result<IndexMap<String, Rendered>, DeclError> {
    let r = open_universe(path);
    let Some(entry) = r.entry.clone() else {
        return fail(
            &format!("{path}: cannot be loaded"),
            r.diags.iter().map(|d| tagged(path, d)).collect(),
        );
    };
    if !r.diags.is_empty() {
        return fail("", r.diags.iter().map(|d| tagged(path, d)).collect());
    }
    let checks: Vec<Diagnostic> = r
        .modules
        .iter()
        .flat_map(|m| {
            let tag = file_tag(path, Some(entry.path.as_path()), &m.path);
            check_module(&m.decls, Some(m.env.clone()), None)
                .iter()
                .map(|d| tagged(&tag, d))
                .collect::<Vec<_>>()
        })
        .collect();
    if checks.iter().any(|d| d.severity == "error") {
        return fail("", checks);
    }
    let binds = bind_inputs(&r.modules, path, &opts.inputs)?;
    let (eng, diags) = run_universe(&r.modules, &entry, binds);
    let report: Vec<Diagnostic> = diags.iter().map(|d| tagged(path, d)).collect();
    if report.iter().any(|d| d.severity == "error") {
        return fail("", report);
    }
    let names: Vec<String> = if opts.outputs.is_empty() {
        entry
            .decls
            .iter()
            .filter(|d| d.exported)
            .filter_map(|d| match &d.body {
                crate::ast::DeclBody::Output { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    } else {
        opts.outputs.clone()
    };
    let texts: std::cell::RefCell<std::collections::HashMap<String, Option<String>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    let read_tpl = |abs: &str| -> Option<String> {
        if !texts.borrow().contains_key(abs) {
            let t = std::fs::read_to_string(abs).ok();
            texts.borrow_mut().insert(abs.to_string(), t);
        }
        texts.borrow().get(abs).cloned().flatten()
    };
    let e7003 = |file: &str, n: &str| -> DeclError {
        DeclError {
            message: "template cannot be read".into(),
            diagnostics: vec![Diagnostic {
                file: file.to_string(),
                code: Some("E7003".into()),
                id: None,
                severity: "error".into(),
                message: "template cannot be read".into(),
                path: n.to_string(),
            }],
        }
    };
    let mut out: IndexMap<String, Rendered> = IndexMap::new();
    for n in &names {
        let Some(v) = entry.env.root(n) else {
            return Err(DeclError {
                message: format!("no root named {n}"),
                diagnostics: report,
            });
        };
        let found = r.modules.iter().find_map(|m| {
            m.decls
                .iter()
                .find(|d| matches!(&d.body, crate::ast::DeclBody::Output { name, .. } if name == n))
                .map(|d| (d, m))
        });
        let form = match found {
            Some((d, _)) => match declared_form(d) {
                Ok(f) => f,
                Err(m) => {
                    return fail(
                        "",
                        vec![Diagnostic {
                            file: path.to_string(),
                            code: Some("E7004".into()),
                            id: None,
                            severity: "error".into(),
                            message: m,
                            path: n.clone(),
                        }],
                    )
                }
            },
            None => Form::default(),
        };
        let src = opts
            .templates
            .iter()
            .find(|(k, _)| k == n)
            .or_else(|| opts.templates.iter().find(|(k, _)| k == "*"))
            .map(|(_, t)| t);
        let template: Option<(String, String, String)> = match src {
            Some(TemplateSource::File(p)) => {
                let given = p.to_string_lossy().to_string();
                let abs = absolute(&given);
                let Some(text) = read_tpl(&abs) else {
                    return Err(e7003(&given, n));
                };
                Some((given, text, parent_of(&abs)))
            }
            Some(TemplateSource::Text(t)) => {
                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".into());
                Some(("<text>".into(), t.clone(), cwd))
            }
            None => match &form.template {
                Some(t) => {
                    let dir = found
                        .and_then(|(_, m)| m.path.parent().map(|p| p.to_string_lossy().to_string()))
                        .unwrap_or_else(|| ".".into());
                    let abs = resolve_in(&dir, t);
                    let Some(text) = read_tpl(&abs) else {
                        return Err(e7003(t, n));
                    };
                    Some((t.clone(), text, parent_of(&abs)))
                }
                None => None,
            },
        };
        match emit_root(&Emission {
            eng: eng.clone(),
            menv: entry.env.clone(),
            root_name: n.clone(),
            value: v,
            form,
            yaml: opts.yaml,
            indent: opts.indent,
            template,
            read_template: &read_tpl,
        }) {
            Ok(Emitted::One(text)) => {
                out.insert(n.clone(), Rendered::Text(text));
            }
            Ok(Emitted::Many(files)) => {
                out.insert(n.clone(), Rendered::Files(files.into_iter().collect()));
            }
            Err(e) => {
                return fail(
                    "",
                    vec![tagged(e.file.as_deref().unwrap_or(path), &e.diag())],
                )
            }
        }
    }
    Ok(out)
}
fn parent_of(p: &str) -> String {
    std::path::Path::new(p)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".into())
}

/// Parse and statically check entry files (module-aware); empty means clean.
pub fn check(paths: &[&str]) -> Vec<Diagnostic> {
    let owned: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
    check_files(&owned)
        .iter()
        .map(|(file, d)| tagged(file, d))
        .collect()
}

/// Validate a file: static checks, then evaluation with the input documents
/// bound; returns every diagnostic (all severities). Fails when the file
/// does not parse.
pub fn validate(path: &str, inputs: &[(String, Document)]) -> Result<Vec<Diagnostic>, DeclError> {
    let src = std::fs::read_to_string(path).map_err(|_| DeclError {
        message: format!("{path}: cannot be read"),
        diagnostics: vec![],
    })?;
    let parsed = parse_source(&src);
    if !parsed.errors.is_empty() {
        return Err(DeclError {
            message: format!("{path}: {} parse error(s)", parsed.errors.len()),
            diagnostics: vec![],
        });
    }
    let checks: Vec<Diagnostic> = check_module(&parsed.decls, None, None)
        .iter()
        .map(|d| tagged(path, d))
        .collect();
    if checks.iter().any(|d| d.severity == "error") {
        return Ok(checks);
    }
    // a warning of the checks (W0001) is returned beside the run's diagnostics
    let mut all = checks;
    if !inputs.is_empty() {
        let r = open_universe(path);
        let Some(entry) = r.entry.clone() else {
            return fail(
                &format!("{path}: cannot be loaded"),
                r.diags.iter().map(|d| tagged(path, d)).collect(),
            );
        };
        let binds = bind_inputs(&r.modules, path, inputs)?;
        let (_, diags) = run_universe(&r.modules, &entry, binds);
        all.extend(diags.iter().map(|d| tagged(path, d)));
        return Ok(all);
    }
    all.extend(
        run_pipeline(&parsed.decls)
            .diags
            .iter()
            .map(|d| tagged(path, d)),
    );
    Ok(all)
}

/// The canonical formatting of a source text; fails when it does not parse.
pub fn format_source(text: &str) -> Result<String, DeclError> {
    crate::fmt::format(text).map_err(|message| DeclError {
        message,
        diagnostics: vec![],
    })
}

// a document's text (canonical JSON) as the reader's shape; the text is the
// library's document form, so its number texts pass through exactly
fn raw_of(text: &str) -> Result<crate::semantics::Value, DeclError> {
    read_json(text).map_err(|_| DeclError {
        message: "not well-formed JSON".into(),
        diagnostics: vec![],
    })
}

/// The JSON text of a document given as JSON text: canonical for indent 0,
/// laid out with `indent` spaces per level otherwise (docs/tooling/05_render.md §4.1).
pub fn to_json(text: &str, indent: usize) -> Result<String, DeclError> {
    Ok(json_text(&raw_of(text)?, indent))
}
/// The YAML text of a document given as JSON text (docs/tooling/05_render.md §4.2), no trailing newline.
pub fn to_yaml(text: &str, indent: usize) -> Result<String, DeclError> {
    Ok(yaml_text(&raw_of(text)?, indent))
}
