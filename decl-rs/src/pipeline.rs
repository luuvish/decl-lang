//! The single-module pipeline — a port of the reference implementation's
//! pipeline.ts: bind and evaluate every output of one module's
//! declarations (the judgment the conformance runner applies), and the
//! source-level report that front-ends and embedders consume.
use crate::ast::{Decl, DeclBody};
use crate::checker::check_module;
use crate::engine::{Engine, RootSrc};
use crate::parse::parse_source;
use crate::qengine::qeval::qevaluate;
use crate::semantics::{sort_diags, Diag, Env, Scope};
use std::rc::Rc;

/// whether an env value reads as false: unset is the caller's default; a value
/// of `0`/`false`/`off`/`no` (or empty) is off, anything else is on
fn q_falsy(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "off" | "no"
    )
}
/// The incremental query engine (qengine/DESIGN.md) is the default evaluator, a
/// drop-in, byte-identical replacement for the tree walker; opt out of it with
/// `DECL_QENGINE=0` (or false/off/no) to run the tree walker.
pub fn use_qengine() -> bool {
    match std::env::var("DECL_QENGINE") {
        Err(_) => true, // unset: the query engine is the default
        Ok(v) => !q_falsy(&v),
    }
}
/// strict mode: an unhandled form is a hard error, not a silent fall back to
/// the tree walker — off unless `DECL_QENGINE_STRICT` is set, to prove coverage
pub fn strict_qengine() -> bool {
    std::env::var("DECL_QENGINE_STRICT")
        .map(|v| !q_falsy(&v))
        .unwrap_or(false)
}

/// one module evaluated: its environment, its engine, its diagnostics
pub struct Pipeline {
    /// the environment the declarations loaded into
    pub env: Rc<Env>,
    /// the engine that bound and evaluated the roots
    pub eng: Rc<Engine>,
    /// every diagnostic, in canonical order (§6.7)
    pub diags: Vec<Diag>,
}

/// Evaluate one module's declarations (§9.1): load, bind every output, force,
/// validate; the diagnostics sorted.
pub fn run_pipeline(decls: &[Decl]) -> Pipeline {
    let env = Env::new();
    env.load(decls);
    let outs = env.outputs.borrow().clone();
    let eng = Engine::evaluate(
        &env,
        &|eng| {
            for (name, ty_ast, expr) in &outs {
                let sc = Scope::new(name, None);
                match env.resolve(ty_ast, None) {
                    Ok(rt) => eng.bind_root(name, RootSrc::Expr(expr), &rt, &sc),
                    Err(e) => env.report(Diag::error(e, name.clone(), None)),
                }
            }
        },
        false,
    );
    eng.validate_all("");
    let diags = sort_diags(env.diagnostics_vec()); // §6.7
    env.diag_set(diags.clone());
    Pipeline { env, eng, diags }
}

/// the phase that decided a source-level report
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    /// a syntax error decided
    Parse,
    /// a static check decided
    Check,
    /// evaluation decided
    Evaluate,
}
/// the source-level report of [`evaluate_source`]: what the playground and the REPL show
pub struct Report {
    /// the phase that decided the report
    pub phase: Phase,
    /// no error-severity finding in that phase
    pub ok: bool,
    /// the syntax errors, as zero-based (row, column)
    pub parse_errors: Vec<(usize, usize)>,
    /// static-checker diagnostics
    pub checks: Vec<Diag>,
    /// binding / evaluation / assertion diagnostics
    pub diagnostics: Vec<Diag>,
    /// every `output`, serialized as canonical JSON text
    pub outputs: Vec<(String, String)>,
    /// input roots declared by the module (not bound here)
    pub inputs: Vec<String>,
}

/// parse, check, and evaluate one module given as source text
pub fn evaluate_source(source: &str) -> Report {
    let parsed = parse_source(source);
    let inputs: Vec<String> = parsed
        .decls
        .iter()
        .filter_map(|d| {
            if let DeclBody::Input { name, .. } = &d.body {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    if !parsed.errors.is_empty() {
        return Report {
            phase: Phase::Parse,
            ok: false,
            parse_errors: parsed.errors,
            checks: vec![],
            diagnostics: vec![],
            outputs: vec![],
            inputs,
        };
    }
    let checks = check_module(&parsed.decls, None, None);
    if checks.iter().any(|d| d.severity == "error") {
        return Report {
            phase: Phase::Check,
            ok: false,
            parse_errors: vec![],
            checks,
            diagnostics: vec![],
            outputs: vec![],
            inputs,
        };
    }
    // the query engine, when selected, evaluates in place of the tree walker;
    // an unhandled form falls back (unless strict mode makes it a hard error)
    if use_qengine() {
        let env = Env::new();
        env.load(&parsed.decls);
        match qevaluate(env) {
            Ok(qr) => {
                return Report {
                    phase: Phase::Evaluate,
                    ok: qr.ok,
                    parse_errors: vec![],
                    checks,
                    diagnostics: qr.diagnostics,
                    outputs: qr.outputs,
                    inputs,
                };
            }
            Err(u) => {
                if strict_qengine() {
                    panic!("DECL_QENGINE_STRICT: unhandled form: {}", u.0);
                }
                // else fall back to the tree walker
            }
        }
    }
    let Pipeline { env, eng, diags } = run_pipeline(&parsed.decls);
    let ok = !diags.iter().any(|d| d.severity == "error");
    let outputs = if ok {
        env.outputs
            .borrow()
            .iter()
            .filter_map(|(n, _, _)| {
                env.root(n)
                    .map(|v| (n.clone(), eng.serialize(&v, n, false)))
            })
            .collect()
    } else {
        vec![]
    };
    Report {
        phase: Phase::Evaluate,
        ok,
        parse_errors: vec![],
        checks,
        diagnostics: diags,
        outputs,
        inputs,
    }
}
