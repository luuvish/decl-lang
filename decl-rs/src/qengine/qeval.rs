//! Query entry points using shared programs and the value layer's slot graph.
use crate::ast::{Expr, TypeAst};
use crate::engine::{Engine, RootSrc};
use crate::semantics::*;
use std::rc::Rc;

/// Compatibility with callers that handle a query capability fallback.
#[derive(Debug)]
pub struct Unsupported(pub String);

/// The settled values and diagnostics of a query evaluation.
pub struct QReport {
    /// no error diagnostic was produced
    pub ok: bool,
    /// each output, serialized as canonical JSON text
    pub outputs: Vec<(String, String)>,
    /// evaluation and validation diagnostics
    pub diagnostics: Vec<Diag>,
}

type RootSpec = (String, TypeAst, Rc<Expr>, Rc<Env>);

/// Evaluate a single module using shared programs and retained reference rounds.
pub fn qevaluate(env: Rc<Env>) -> Result<QReport, Unsupported> {
    let roots: Vec<RootSpec> = env
        .outputs
        .borrow()
        .iter()
        .map(|(n, t, e)| (n.clone(), t.clone(), e.clone(), env.clone()))
        .collect();
    let (report, _eng) = eval_rounds(&env, &roots, &[], &[], true)?;
    Ok(report)
}

/// A bound input document for the universe: its name, raw value, and the
/// module scope that declares it.
pub struct BoundSpec {
    /// the input's name
    pub name: String,
    /// the document, as the JSON reader gives it
    pub raw: Value,
    /// the scope that declares the input
    pub menv: Rc<Env>,
}

/// Evaluate a whole multi-module universe (§8.8): every module's outputs are
/// roots, each bound in its own module scope into the entry module's universe;
/// bound input documents are roots too. Returns the report and the settled
/// round's value layer (its roots populated and forced) so the caller can
/// serialize through it exactly as `run_universe` does.
pub fn qevaluate_universe(
    mods: &[Rc<Env>],
    entry: &Rc<Env>,
    binds: &[BoundSpec],
) -> Result<(QReport, Rc<Engine>), Unsupported> {
    run_universe(mods, entry, binds, true)
}

/// Evaluate module values and diagnostics, optionally serializing the outputs.
/// Module consumers render selected roots themselves; building every root's
/// JSON here would allocate a second output only to discard it.
pub(crate) fn run_universe(
    mods: &[Rc<Env>],
    entry: &Rc<Env>,
    binds: &[BoundSpec],
    serialize_outputs: bool,
) -> Result<(QReport, Rc<Engine>), Unsupported> {
    let roots: Vec<RootSpec> = mods
        .iter()
        .flat_map(|m| {
            m.outputs
                .borrow()
                .iter()
                .map(|(n, t, e)| (n.clone(), t.clone(), e.clone(), m.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    eval_rounds(entry, &roots, binds, mods, serialize_outputs)
}

/// The rounds driver shared by the single-module and universe entries: bind the
/// bound inputs and roots through shared programs, forcing through the value
/// layer's rounds machinery until `$referrers` settles. `hook_mods` are
/// the module scopes whose elaboration hooks (a const in a type position, a
/// unit factor) point at each round's value layer — every module for a
/// universe, none for a single module (as `run_universe` and `run_pipeline` do).
fn eval_rounds(
    entry: &Rc<Env>,
    roots: &[RootSpec],
    binds: &[BoundSpec],
    hook_mods: &[Rc<Env>],
    serialize_outputs: bool,
) -> Result<(QReport, Rc<Engine>), Unsupported> {
    let bind = |eng: &Rc<Engine>| {
        for m in hook_mods {
            eng.install_hooks(m, true);
        }
        // a bound input document is a root of the universe, available before the
        // outputs that read it (§9.2); it binds through the value layer
        for b in binds {
            let decl = b.menv.inputs.borrow().get(&b.name).cloned();
            let Some((ty_ast, _)) = decl else { continue };
            let sc = Scope::new(&b.name, Some(b.menv.clone()));
            match b.menv.resolve(&ty_ast, None) {
                Ok(rt) => eng.bind_root(&b.name, RootSrc::Doc(b.raw.clone()), &rt, &sc),
                Err(e) => entry.report(Diag::error(e, b.name.clone(), None)),
            }
        }
        for (name, ty_ast, expr, menv) in roots {
            if !eng.should_bind_root(name) {
                continue;
            }
            let Ok(rt) = menv.resolve(ty_ast, None) else {
                continue;
            };
            eng.bind_root(
                name,
                RootSrc::Expr(expr),
                &rt,
                &Scope::new(name, Some(menv.clone())),
            );
        }
    };
    let eng = Engine::evaluate_query(entry, &bind);
    eng.validate_all("");
    let diags = sort_diags(entry.diagnostics_vec()); // §6.7
    entry.diag_set(diags.clone());
    let ok = !diags.iter().any(|d| d.severity == "error");
    let outputs = if ok && serialize_outputs {
        roots
            .iter()
            .filter_map(|(n, _, _, _)| {
                entry
                    .root(n)
                    .map(|v| (n.clone(), eng.serialize(&v, n, false)))
            })
            .collect()
    } else {
        vec![]
    };
    Ok((
        QReport {
            ok,
            outputs,
            diagnostics: diags,
        },
        eng,
    ))
}
