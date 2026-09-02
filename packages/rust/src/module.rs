//! Module loading, linking, and universe evaluation (module.ts) —
//! relative imports; package specifiers report E3010 for now.
use crate::ast::*;
use crate::engine::Engine;
use crate::parse::parse_source;
use crate::semantics::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct Module {
    pub path: PathBuf,
    pub decls: Vec<Decl>,
    pub env: Rc<Env>,
    pub exports: Rc<RefCell<HashMap<String, Export>>>,
}

pub struct LoadResult {
    pub modules: Vec<Rc<Module>>,
    pub entry: Option<Rc<Module>>,
    pub diags: Vec<Diag>,
}

struct Loader {
    modules: HashMap<PathBuf, Rc<Module>>,
    order: Vec<Rc<Module>>,
    visiting: Vec<PathBuf>,
    diags: Vec<Diag>,
}

impl Loader {
    fn report(&mut self, code: &str, message: String) {
        self.diags.push(Diag::error(message, String::new(), Some(code)));
    }
    fn resolve_spec(&mut self, spec: &str, from_dir: &Path) -> Option<PathBuf> {
        if spec.starts_with("./") || spec.starts_with("../") {
            return Some(normalize(&from_dir.join(spec)));
        }
        self.report("E3010", format!("package import \"{spec}\" is not supported by this runtime (relative imports only)"));
        None
    }
    fn load(&mut self, path: &Path) -> Option<Rc<Module>> {
        let abs = normalize(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        if let Some(m) = self.modules.get(&abs) {
            return Some(m.clone());
        }
        if let Some(ci) = self.visiting.iter().position(|p| *p == abs) {
            let cycle: Vec<String> = self.visiting[ci..].iter().chain(std::iter::once(&abs)).map(|p| p.display().to_string()).collect();
            self.report("E3007", format!("module import cycle: {}", cycle.join(" -> ")));
            return None;
        }
        let src = match std::fs::read_to_string(&abs) {
            Ok(s) => s,
            Err(_) => {
                self.report("E3004", format!("module not found: {}", abs.display()));
                return None;
            }
        };
        let parsed = parse_source(&src);
        if !parsed.errors.is_empty() {
            self.report("E2001", format!("{}: {} parse error(s)", abs.display(), parsed.errors.len()));
            return None;
        }
        let env = Env::new();
        env.load(&parsed.decls);
        for n in env.duplicates.borrow().iter() {
            self.diags.push(Diag::error(format!("duplicate name {n} in {}", abs.display()), String::new(), Some("E3001")));
        }
        let m = Rc::new(Module { path: abs.clone(), decls: parsed.decls, env, exports: Rc::new(RefCell::new(HashMap::new())) });
        self.visiting.push(abs.clone());
        let mut targets: HashMap<String, Rc<Module>> = HashMap::new();
        let from_dir = abs.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        for d in &m.decls {
            let from = match &d.body {
                DeclBody::Import { from, .. } | DeclBody::ReExport { from, .. } => from.clone(),
                _ => continue,
            };
            if let Some(t) = self.resolve_spec(&from, &from_dir) {
                if let Some(tm) = self.load(&t) {
                    targets.insert(from, tm);
                }
            }
        }
        self.visiting.pop();
        self.modules.insert(abs.clone(), m.clone());

        let taken = |n: &str| {
            let e = &m.env;
            e.type_asts.borrow().contains_key(n)
                || e.consts.borrow().contains_key(n)
                || e.funcs.borrow().contains_key(n)
                || e.diags.borrow().contains_key(n)
                || e.inputs.borrow().contains_key(n)
                || e.outputs.borrow().iter().any(|(o, _, _)| o == n)
                || e.imports.borrow().contains_key(n)
                || e.namespaces.borrow().contains_key(n)
        };
        for d in &m.decls {
            match &d.body {
                DeclBody::Import { from, names, ns } => {
                    let Some(tm) = targets.get(from) else { continue };
                    if let Some(ns) = ns {
                        if taken(ns) {
                            self.report("E3006", format!("import {ns} collides with an existing binding in {}", abs.display()));
                            continue;
                        }
                        m.env.namespaces.borrow_mut().insert(ns.clone(), (tm.env.clone(), tm.exports.clone()));
                        continue;
                    }
                    for it in names.iter().flatten() {
                        let local = it.alias.clone().unwrap_or_else(|| it.name.clone());
                        let ex = tm.exports.borrow().get(&it.name).cloned();
                        let Some(ex) = ex else {
                            self.report("E3005", format!("{} does not export {}", tm.path.display(), it.name));
                            continue;
                        };
                        if taken(&local) {
                            self.report("E3006", format!("import {local} collides with an existing binding in {}", abs.display()));
                            continue;
                        }
                        m.env.imports.borrow_mut().insert(local, ex);
                    }
                }
                DeclBody::ReExport { from, names } => {
                    let Some(tm) = targets.get(from) else { continue };
                    for it in names {
                        let ex = tm.exports.borrow().get(&it.name).cloned();
                        match ex {
                            Some(ex) => {
                                m.exports.borrow_mut().insert(it.alias.clone().unwrap_or_else(|| it.name.clone()), ex);
                            }
                            None => self.report("E3005", format!("{} does not export {}", tm.path.display(), it.name)),
                        }
                    }
                }
                _ => {}
            }
        }
        for d in &m.decls {
            if !d.exported {
                continue;
            }
            if matches!(d.body, DeclBody::Unit { .. } | DeclBody::Dimension { .. } | DeclBody::Import { .. } | DeclBody::ReExport { .. }) {
                continue;
            }
            if let Some(n) = d.name() {
                m.exports.borrow_mut().insert(n.to_string(), Export { env: m.env.clone(), name: n.to_string() });
            }
        }
        self.order.push(m.clone());
        Some(m)
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub fn load_modules(entry: &Path) -> LoadResult {
    let mut ld = Loader { modules: HashMap::new(), order: vec![], visiting: vec![], diags: vec![] };
    let entry_m = ld.load(entry);
    if let Some(e) = &entry_m {
        link_universe(&ld.order, e, &mut ld.diags);
    }
    LoadResult { modules: ld.order, entry: entry_m, diags: ld.diags }
}

fn link_universe(mods: &[Rc<Module>], entry: &Rc<Module>, diags: &mut Vec<Diag>) {
    let mut owners: HashMap<String, PathBuf> = HashMap::new();
    for m in mods {
        for d in &m.decls {
            if let DeclBody::Output { name, .. } | DeclBody::Input { name, .. } = &d.body {
                if let Some(prev) = owners.get(name) {
                    if *prev != m.path {
                        diags.push(Diag::error(format!("root {name} declared in both {} and {}", prev.display(), m.path.display()), String::new(), Some("E3018")));
                    }
                }
                owners.insert(name.clone(), m.path.clone());
            }
        }
    }
    for m in mods {
        for d in &m.decls {
            if !d.exported {
                continue;
            }
            match &d.body {
                DeclBody::Dimension { name, terms } => {
                    for m2 in mods {
                        if Rc::ptr_eq(m2, m) {
                            continue;
                        }
                        let own = m2.decls.iter().any(|x| matches!(&x.body, DeclBody::Dimension { name: n, .. } if n == name));
                        let has = m2.env.dim_decls.borrow().contains_key(name);
                        if has && !own {
                            continue;
                        }
                        if has {
                            diags.push(Diag::error(format!("dimension {name} redeclared across modules"), String::new(), Some("E3001")));
                        } else {
                            m2.env.dim_decls.borrow_mut().insert(name.clone(), terms.clone());
                        }
                    }
                }
                DeclBody::Unit { name, dim, factor, base } => {
                    for m2 in mods {
                        if Rc::ptr_eq(m2, m) {
                            continue;
                        }
                        let own = m2.decls.iter().any(|x| matches!(&x.body, DeclBody::Unit { name: n, .. } if n == name));
                        let has = m2.env.unit_decls.borrow().contains_key(name);
                        if has && !own {
                            continue;
                        }
                        if has {
                            diags.push(Diag::error(format!("unit {name} redeclared across modules"), String::new(), Some("E4073")));
                        } else {
                            m2.env.unit_decls.borrow_mut().insert(name.clone(), UnitDecl { dim: dim.clone(), factor: factor.clone(), base: base.clone() });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for m in mods {
        if Rc::ptr_eq(m, entry) {
            continue;
        }
        *m.env.registry.borrow_mut() = entry.env.registry.borrow().clone();
        *m.env.roots.borrow_mut() = entry.env.roots.borrow().clone();
        *m.env.diagnostics.borrow_mut() = entry.env.diagnostics.borrow().clone();
    }
}

pub struct Bind {
    pub input: String,
    pub raw: Value,
}

pub fn run_universe(mods: &[Rc<Module>], entry: &Rc<Module>, binds: Vec<Bind>) -> (Rc<Engine>, Vec<Diag>) {
    let eng = Engine::new(entry.env.clone());
    for m in mods {
        eng.install_hooks(&m.env, true);
    }
    for m in mods {
        let outs = m.env.outputs.borrow().clone();
        for (name, ty_ast, expr) in outs {
            let sc = Scope::new(&name, Some(m.env.clone()));
            let bound = (|| -> R<Value> {
                let v = eng.ev(&expr, &sc)?;
                let rt = m.env.resolve(&ty_ast, None).or_else(|e| err(e))?;
                eng.bind(v, &rt, &[Seg::Name(name.clone())], None, &sc)
            })();
            if let Ok(v) = bound {
                entry.env.set_root(&name, v);
            }
        }
    }
    for b in binds {
        let decl = entry.env.inputs.borrow().get(&b.input).cloned();
        let Some((ty_ast, _)) = decl else { continue };
        let sc = Scope::new(&b.input, Some(entry.env.clone()));
        if let Ok(rt) = entry.env.resolve(&ty_ast, None) {
            if let Ok(v) = eng.bind(b.raw, &rt, &[Seg::Name(b.input.clone())], None, &sc) {
                entry.env.set_root(&b.input, v);
            }
        }
    }
    eng.drive(&entry.env);
    let diags = entry.env.diagnostics_vec();
    (eng, diags)
}

/// single-module evaluation of a fixture (no import resolution)
pub fn run_pipeline(decls: &[Decl]) -> (Rc<Env>, Rc<Engine>) {
    let env = Env::new();
    env.load(decls);
    let eng = Engine::new(env.clone());
    let outs = env.outputs.borrow().clone();
    for (name, ty_ast, expr) in outs {
        let sc = Scope::new(&name, None);
        let bound = (|| -> R<Value> {
            let v = eng.ev(&expr, &sc)?;
            let rt = env.resolve(&ty_ast, None).or_else(|e| err(e))?;
            eng.bind(v, &rt, &[Seg::Name(name.clone())], None, &sc)
        })();
        if let Ok(v) = bound {
            env.set_root(&name, v);
        }
    }
    eng.drive(&env);
    (env, eng)
}
