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
use crate::semantics::{read_json, Diag};
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
    /// a JSON file, by path
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
        let raw = match read_json(&text) {
            Ok(v) => v,
            Err(_) => {
                return fail(
                    "",
                    vec![e6004(format!(
                        "bound document is not well-formed JSON: {place}"
                    ))],
                )
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
