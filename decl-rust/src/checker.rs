//! Static checks over the AST + resolved types — a port of the reference
//! implementation's checker.ts (chapters 3–4). Implemented:
//!   E3001 duplicate module name         E3003 unknown type name
//!   E4010 mixed range endpoints         E4011 empty range / array size
//!   E4012 structurally empty intersection
//!   E4013 non-discriminable record union arms
//!   E4014 more than one non-record object arm in a union
//!   E4015 map key not string-shaped     E4030 inheritance widening
//!   E4032 illegal member-kind transition
//!   E4052 ?? mixed with &&/|| unparenthesized
//!   E4094 context variable without / with an invalid context declaration
//! plus the expression pass of infer.rs (inference, assignability, absence).
use crate::ast::*;
use crate::engine::Engine;
use crate::infer::*;
use crate::infer::Ty;
use crate::semantics::*;
use crate::subsume::{structurally_empty, subsumes};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

// ---------------- generic AST traversal (the reference walks object values) ----------------
/// every expression reachable from `e`, in the reference's object-value
/// order; `into_types` also descends into type ASTs embedded in
/// expressions (match arm types), as the untyped walks do
fn walk_expr_tree(e: &Rc<Expr>, into_types: bool, f: &mut dyn FnMut(&Rc<Expr>)) {
    f(e);
    let mut go = |x: &Rc<Expr>| walk_expr_tree(x, into_types, f);
    match &**e {
        Expr::Lit(_) | Expr::UnitLit { .. } | Expr::Name(_) | Expr::Ctx(_) | Expr::Referrers { .. } | Expr::Pattern(_) => {}
        Expr::Template(parts) => {
            for p in parts {
                if let TPart::Expr(x) = p {
                    go(x);
                }
            }
        }
        Expr::Obj(entries) => entries.iter().for_each(|(_, v)| go(v)),
        Expr::Arr(items) => items.iter().for_each(|(_, v)| go(v)),
        Expr::Comp { head, clauses } => {
            go(head);
            for c in clauses {
                go(&c.iter);
                c.filters.iter().for_each(&mut go);
            }
        }
        Expr::MapComp { key, val, clauses } => {
            go(key);
            go(val);
            for c in clauses {
                go(&c.iter);
                c.filters.iter().for_each(&mut go);
            }
        }
        Expr::Bin { l, r, .. } => {
            go(l);
            go(r);
        }
        Expr::Un { x, .. } | Expr::Paren(x) => go(x),
        Expr::If { c, t, f: ff } => {
            go(c);
            go(t);
            go(ff);
        }
        Expr::Lambda { body, .. } => go(body),
        Expr::Call { fun, args } => {
            go(fun);
            args.iter().for_each(&mut go);
        }
        Expr::Member { x, .. } => go(x),
        Expr::Index { x, i } => {
            go(x);
            go(i);
        }
        Expr::With { base, patch } => {
            go(base);
            go(patch);
        }
        Expr::Match { subject, arms } => {
            go(subject);
            for a in arms {
                if into_types {
                    if let Some(t) = &a.ty {
                        walk_type_exprs(t, into_types, f);
                    }
                }
                walk_expr_tree(&a.body, into_types, f);
            }
        }
    }
}
/// every expression embedded in a type AST (predicates, member defaults, asserts)
fn walk_type_exprs(t: &TypeAst, into_types: bool, f: &mut dyn FnMut(&Rc<Expr>)) {
    match t {
        TypeAst::Prim(_) | TypeAst::Lit(_) | TypeAst::Range { .. } | TypeAst::Pattern(_) => {}
        TypeAst::Record { members, .. } => members.iter().for_each(|m| walk_member_exprs(m, into_types, f)),
        TypeAst::Map { key, val } => {
            walk_type_exprs(key, into_types, f);
            walk_type_exprs(val, into_types, f);
        }
        TypeAst::Array { elem, .. } => walk_type_exprs(elem, into_types, f),
        TypeAst::Union(arms) | TypeAst::Isect(arms) => arms.iter().for_each(|a| walk_type_exprs(a, into_types, f)),
        TypeAst::Func { params, ret } => {
            params.iter().for_each(|p| walk_type_exprs(p, into_types, f));
            walk_type_exprs(ret, into_types, f);
        }
        TypeAst::Named { args, preds, ext, .. } => {
            args.iter().for_each(|a| walk_type_exprs(a, into_types, f));
            if let Some(ps) = preds {
                ps.iter().for_each(|p| walk_expr_tree(p, into_types, f));
            }
            if let Some(x) = ext {
                walk_type_exprs(x, into_types, f);
            }
        }
    }
}
fn walk_member_exprs(m: &MemberAst, into_types: bool, f: &mut dyn FnMut(&Rc<Expr>)) {
    match m {
        MemberAst::Value { ty, dflt, .. } => {
            walk_type_exprs(ty, into_types, f);
            if let Some(d) = dflt {
                walk_expr_tree(d, into_types, f);
            }
        }
        MemberAst::Derived { ty, expr, .. } => {
            if let Some(t) = ty {
                walk_type_exprs(t, into_types, f);
            }
            walk_expr_tree(expr, into_types, f);
        }
        MemberAst::Context { ty, .. } => walk_type_exprs(ty, into_types, f),
        MemberAst::Assert { cond, tail, .. } => {
            walk_expr_tree(cond, into_types, f);
            match tail {
                Some(Tail::Inline { template, .. }) => {
                    for p in template {
                        if let TPart::Expr(x) = p {
                            walk_expr_tree(x, into_types, f);
                        }
                    }
                }
                Some(Tail::Ref { args, .. }) => args.iter().for_each(|a| walk_expr_tree(a, into_types, f)),
                None => {}
            }
        }
        MemberAst::When { cond, body } => {
            walk_expr_tree(cond, into_types, f);
            body.iter().for_each(|b| walk_member_exprs(b, into_types, f));
        }
    }
}

fn str_shaped(k: &RT) -> bool {
    match &k.k {
        RTk::Prim(n) => n == "string",
        RTk::Pattern { .. } => true,
        RTk::Lit(Value::Str(_)) => true,
        RTk::Union(arms) => arms.iter().all(str_shaped),
        RTk::Pred { base, .. } => str_shaped(base),
        _ => false,
    }
}
fn kind_name(k: MKind) -> &'static str {
    match k {
        MKind::Req => "req",
        MKind::Opt => "opt",
        MKind::Dflt => "dflt",
        MKind::Der => "der",
    }
}

pub fn check_module(decls: &[Decl], linked: Option<Rc<Env>>) -> Vec<Diag> {
    let out: Rc<RefCell<Vec<Diag>>> = Rc::new(RefCell::new(vec![]));
    let report: Report = {
        let out = out.clone();
        Rc::new(move |code: &str, message: String| out.borrow_mut().push(Diag::error(message, String::new(), Some(code))))
    };
    let rep = |code: &str, message: String| report(code, message);

    let env = match &linked {
        Some(e) => e.clone(),
        None => {
            let e = Env::new();
            e.load(decls);
            e
        }
    };
    // installs env.const_eval / env.expr_eval (§4.13, §3.16); kept alive for the check
    let _eng = if env.const_eval.borrow().is_none() { Some(Engine::new(env.clone())) } else { None };
    *env.const_diag_sink.borrow_mut() = Some(out.clone()); // constant-evaluation errors surface here
    for n in env.duplicates.borrow().iter() {
        rep("E3001", format!("duplicate name {n} in module"));
    }
    out.borrow_mut().extend(env.finalize_unit_space()); // §3.16 unit/dimension spaces

    // ---------- §4.13: constant positions ----------
    let tparams: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    let is_input = |n: &str| env.inputs.borrow().contains_key(n);
    let is_output = |n: &str| env.outputs.borrow().iter().any(|(o, _, _)| o == n);
    let check_endpoint = |v: Option<&Value>, where_: &str| {
        let Some(Value::Str(v)) = v else { return };
        if tparams.borrow().contains(v) || v.contains('.') {
            return;
        }
        if is_input(v) || is_output(v) {
            rep("E4021", format!("non-constant {where_}: {v} is an input/output, not a module const"));
        } else if !env.consts.borrow().contains_key(v) {
            rep("E3003", format!("unknown name {v} in a {where_}"));
        }
    };
    let const_violation = |e: &Rc<Expr>| -> Option<String> {
        let mut found: Option<String> = None;
        walk_expr_tree(e, true, &mut |x| {
            if found.is_some() {
                return;
            }
            found = match &**x {
                Expr::Ctx(n) => Some(format!("context variable {n}")),
                Expr::Referrers { .. } => Some("$referrers".into()),
                Expr::Name(n) if is_input(n) => Some(format!("input {n}")),
                Expr::Name(n) if is_output(n) => Some(format!("output {n}")),
                _ => None,
            };
        });
        found
    };

    // ---------- AST-level walks ----------
    let is_bool_op = |e: &Expr| matches!(e, Expr::Bin { op, .. } if op == "&&" || op == "||");
    let walk_expr = |e: &Rc<Expr>| {
        walk_expr_tree(e, true, &mut |x| {
            if let Expr::Bin { op, l, r } = &**x {
                if op == "??" && (is_bool_op(l) || is_bool_op(r)) {
                    rep("E4052", "`??` mixed with `&&`/`||` without parentheses".into());
                }
            }
        });
    };

    // D30: context obligations
    fn ctx_uses(m: &MemberAst, used: &mut Vec<String>) {
        let mut scan = |e: &Rc<Expr>| {
            walk_expr_tree(e, false, &mut |x| {
                if let Expr::Ctx(n) = &**x {
                    if ["$parent", "$root", "$key"].contains(&n.as_str()) && !used.contains(n) {
                        used.push(n.clone());
                    }
                }
            });
        };
        match m {
            MemberAst::Value { dflt: Some(d), .. } => scan(d),
            MemberAst::Derived { expr, .. } => scan(expr),
            MemberAst::Assert { cond, .. } => scan(cond),
            MemberAst::When { cond, body } => {
                scan(cond);
                for b in body {
                    ctx_uses(b, used);
                }
            }
            _ => {}
        }
    }
    let check_record_ctx = |members: &[MemberAst], depth: i32, decl_name: Option<&str>| {
        let declared: Vec<(&String, &TypeAst)> = members
            .iter()
            .filter_map(|m| if let MemberAst::Context { variable, ty } = m { Some((variable, ty)) } else { None })
            .collect();
        for (v, t) in &declared {
            let is_ref = matches!(t, TypeAst::Named { name, .. } if name == "ref");
            if (v.as_str() == "$parent" || v.as_str() == "$root") && !is_ref {
                rep("E4094", format!("{v} declaration must be ref<...> ({})", decl_name.unwrap_or("anonymous")));
            }
            if v.as_str() == "$key" && is_ref {
                rep("E4094", "$key declares a plain value type, not ref<...>".into());
            }
        }
        if depth > 1 {
            return; // lexically nested: parent evident, no declaration required
        }
        let mut used: Vec<String> = vec![];
        for m in members {
            ctx_uses(m, &mut used);
        }
        for u in used {
            if !declared.iter().any(|(v, _)| **v == u) {
                rep("E4094", format!("{u} used without a context declaration in {}", decl_name.unwrap_or("anonymous type")));
            }
        }
    };

    // inheritance (extension)
    let check_extension = |name: &str, args: &[TypeAst], ext: &TypeAst, decl_name: Option<&str>| {
        let Ok(base) = env.resolve(&TypeAst::Named { name: name.to_string(), args: args.to_vec(), preds: None, ext: None }, None) else { return }; // unknown base reported by the resolution pass
        let RTk::Rec(brec) = &base.k else {
            rep("E4031", format!("extending non-record type {name}"));
            return;
        };
        let TypeAst::Record { members: ext_members, .. } = ext else { return };
        let bmembers = brec.members.borrow();
        for om in ext_members {
            let (oname, o_kind, o_type): (&str, &str, Option<&TypeAst>) = match om {
                MemberAst::Assert { .. } | MemberAst::When { .. } | MemberAst::Context { .. } => continue,
                MemberAst::Derived { name, ty, .. } => (name, "der", ty.as_ref()),
                MemberAst::Value { name, opt, ty, dflt } => (name, if dflt.is_some() { "dflt" } else if *opt { "opt" } else { "req" }, Some(ty)),
            };
            let Some(bm) = bmembers.iter().find(|x| x.name == oname) else { continue }; // addition
            let allowed: &[&str] = match bm.kind {
                MKind::Req => &["req", "dflt", "der"],
                MKind::Opt => &["req", "opt", "dflt", "der"],
                MKind::Dflt => &["req", "dflt", "der"],
                MKind::Der => &["der"],
            };
            if !allowed.contains(&o_kind) {
                rep("E4032", format!("illegal member-kind transition for {oname}: {} -> {o_kind} ({})", kind_name(bm.kind), decl_name.unwrap_or(name)));
                continue;
            }
            let ot = try_resolve(&env, o_type);
            if let (Some(ot), Some(bt)) = (ot, &bm.ty) {
                if !subsumes(&env, &ot, bt) {
                    rep("E4030", format!("override widens inherited member {oname} ({})", decl_name.unwrap_or(name)));
                }
            }
        }
    };

    struct Walk<'a> {
        rep: &'a dyn Fn(&str, String),
        check_endpoint: &'a dyn Fn(Option<&Value>, &str),
        const_violation: &'a dyn Fn(&Rc<Expr>) -> Option<String>,
        walk_expr: &'a dyn Fn(&Rc<Expr>),
        check_record_ctx: &'a dyn Fn(&[MemberAst], i32, Option<&str>),
        check_extension: &'a dyn Fn(&str, &[TypeAst], &TypeAst, Option<&str>),
    }
    impl<'a> Walk<'a> {
        fn walk_type(&self, t: Option<&TypeAst>, depth: i32, decl_name: Option<&str>) {
            let Some(t) = t else { return };
            match t {
                TypeAst::Range { lo, hi, .. } => {
                    let kinds = [js_typeof(lo), js_typeof(hi)];
                    if kinds[0] != kinds[1] && !kinds.contains(&"string") {
                        (self.rep)("E4010", format!("mixed range endpoints: {}..{}", js_str(lo), js_str(hi)));
                    }
                    (self.check_endpoint)(Some(lo), "range endpoint");
                    (self.check_endpoint)(Some(hi), "range endpoint");
                }
                TypeAst::Record { members, .. } => {
                    (self.check_record_ctx)(members, depth, decl_name);
                    for m in members {
                        self.walk_member(m, depth, decl_name);
                    }
                }
                TypeAst::Map { key, val } => {
                    self.walk_type(Some(key), depth, decl_name);
                    self.walk_type(Some(val), depth, decl_name);
                }
                TypeAst::Array { elem, lo, hi, .. } => {
                    (self.check_endpoint)(lo.as_ref(), "array size");
                    (self.check_endpoint)(hi.as_ref(), "array size");
                    self.walk_type(Some(elem), depth, decl_name);
                }
                TypeAst::Union(arms) | TypeAst::Isect(arms) => arms.iter().for_each(|a| self.walk_type(Some(a), depth, decl_name)),
                TypeAst::Func { params, ret } => {
                    params.iter().for_each(|p| self.walk_type(Some(p), depth, decl_name));
                    self.walk_type(Some(ret), depth, decl_name);
                }
                TypeAst::Named { name, args, preds, ext } => {
                    args.iter().for_each(|a| self.walk_type(Some(a), depth, decl_name));
                    if let Some(x) = ext {
                        (self.check_extension)(name, args, x, decl_name);
                        self.walk_type(Some(x), depth + 1, decl_name);
                    }
                    for p in preds.iter().flatten() {
                        if let Some(bad) = (self.const_violation)(p) {
                            (self.rep)("E4021", format!("non-constant predicate argument: {bad} (§4.13)"));
                        }
                        (self.walk_expr)(p);
                    }
                }
                TypeAst::Prim(_) | TypeAst::Lit(_) | TypeAst::Pattern(_) => {}
            }
        }
        fn walk_member(&self, m: &MemberAst, depth: i32, decl_name: Option<&str>) {
            match m {
                MemberAst::Value { ty, dflt, .. } => {
                    self.walk_type(Some(ty), depth + 1, decl_name);
                    if let Some(d) = dflt {
                        (self.walk_expr)(d);
                    }
                }
                MemberAst::Derived { ty, expr, .. } => {
                    self.walk_type(ty.as_ref(), depth + 1, decl_name);
                    (self.walk_expr)(expr);
                }
                MemberAst::Context { ty, .. } => self.walk_type(Some(ty), depth + 1, decl_name),
                MemberAst::Assert { cond, .. } => (self.walk_expr)(cond),
                MemberAst::When { cond, body } => {
                    (self.walk_expr)(cond);
                    body.iter().for_each(|x| self.walk_member(x, depth, decl_name));
                }
            }
        }
    }
    let walk = Walk {
        rep: &rep,
        check_endpoint: &check_endpoint,
        const_violation: &const_violation,
        walk_expr: &walk_expr,
        check_record_ctx: &check_record_ctx,
        check_extension: &check_extension,
    };

    // ---------- resolution-level checks ----------
    let resolve_reported: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    let pattern_unknown = regex::Regex::new(r"pattern interpolation of .*: unknown type").unwrap();
    let map_resolve_err = |msg: &str, where_: &str| {
        let key = format!("{msg}|{where_}");
        if !resolve_reported.borrow_mut().insert(key) {
            return; // one resolution failure, one report
        }
        if msg.contains("unknown dimension") || msg.contains("circular dimension") {
            rep("E3003", format!("{msg} (in {where_})"));
        } else if msg.contains("unknown unit") {
            rep("E4073", format!("{msg} (in {where_})"));
        } else if pattern_unknown.is_match(msg) {
            rep("E3003", format!("{msg} (in {where_})"));
        } else if msg.contains("unknown type") {
            rep("E3003", format!("{msg} (in {where_})"));
        } else if msg.contains("generic arity") {
            rep("E4022", format!("{msg} (in {where_})"));
        } else if msg.contains("outside parameter") {
            rep("E4023", format!("{msg} (in {where_})"));
        } else if msg.contains("non-constant value argument") {
            rep("E4021", format!("{msg} (in {where_})"));
        } else if msg.contains("pattern interpolation") {
            rep("E4117", format!("{msg} (in {where_})"));
        } else if msg.contains("malformed pattern") {
            rep("E1004", format!("{msg} (in {where_})"));
        } else {
            rep("E4001", format!("{msg} (in {where_})")); // never drop a resolution failure silently
        }
    };
    let resolve_or_report = |t: Option<&TypeAst>, where_: &str| -> Option<RT> {
        let t = t?;
        match env.resolve(t, None) {
            Ok(rt) => Some(rt),
            Err(msg) => {
                map_resolve_err(&msg, where_);
                None
            }
        }
    };

    fn check_resolved(env: &Rc<Env>, rep: &dyn Fn(&str, String), rt: &RT, name: &str, seen: &mut HashSet<usize>) {
        let id = Rc::as_ptr(rt) as usize;
        if seen.contains(&id) {
            return;
        }
        seen.insert(id);
        match &rt.k {
            RTk::Range { lo, hi, .. } => {
                let ks = [js_typeof(lo), js_typeof(hi)];
                if !ks.contains(&"string") && ks[0] != ks[1] {
                    rep("E4010", format!("mixed range endpoints after constant substitution in {name}"));
                }
                if structurally_empty(env, rt) {
                    rep("E4011", format!("empty range in {name}"));
                }
            }
            RTk::Arr { elem, .. } => {
                if structurally_empty(env, rt) {
                    rep("E4011", format!("empty array size in {name}"));
                }
                check_resolved(env, rep, elem, name, seen);
            }
            RTk::IsectN(arms) => {
                if structurally_empty(env, rt) {
                    rep("E4012", format!("structurally empty intersection in {name}"));
                }
                arms.iter().for_each(|a| check_resolved(env, rep, a, name, seen));
            }
            RTk::Map { key, val } => {
                if !str_shaped(key) {
                    rep("E4015", format!("map key type not string-shaped in {name}"));
                }
                check_resolved(env, rep, val, name, seen);
            }
            RTk::Union(arms) => {
                let recs: Vec<&RT> = arms.iter().filter(|a| is_rec(a)).collect();
                if recs.len() >= 2 {
                    let lit_of = |r: &RT, n: &str| -> Option<Value> {
                        rec_members(r).iter().find(|x| x.name == n).and_then(|x| x.ty.as_ref()).and_then(|t| if let RTk::Lit(v) = &t.k { Some(v.clone()) } else { None })
                    };
                    let disc: Vec<String> = rec_members(recs[0])
                        .iter()
                        .filter(|m| matches!(m.ty.as_ref().map(|t| &t.k), Some(RTk::Lit(_))) && recs.iter().all(|r| lit_of(r, &m.name).is_some()))
                        .map(|m| m.name.clone())
                        .collect();
                    let tuples: HashSet<String> = recs
                        .iter()
                        .map(|r| format!("[{}]", disc.iter().map(|d| json_str(&js_str(&lit_of(r, d).unwrap()))).collect::<Vec<_>>().join(",")))
                        .collect();
                    if disc.is_empty() || tuples.len() != recs.len() {
                        rep("E4013", format!("record union arms not discriminable in {name}"));
                    }
                }
                let non_rec_obj = arms.iter().filter(|a| matches!(a.k, RTk::Map { .. } | RTk::Quantity(_))).count();
                if non_rec_obj > 1 {
                    rep("E4014", format!("more than one non-record object arm in {name}"));
                }
                arms.iter().for_each(|a| check_resolved(env, rep, a, name, seen));
            }
            RTk::Rec(_) => {
                for m in rec_members(rt) {
                    if let Some(t) = &m.ty {
                        check_resolved(env, rep, t, name, seen);
                    }
                }
            }
            RTk::Pred { base, .. } => check_resolved(env, rep, base, name, seen),
            RTk::Ref(target) => check_resolved(env, rep, target, name, seen),
            _ => {}
        }
    }

    let type_order = env.type_order.borrow().clone();
    let type_entry = |n: &str| env.type_asts.borrow().get(n).cloned();
    for name in &type_order {
        let Some(decl) = type_entry(name) else { continue };
        if !decl.params.is_empty() {
            continue; // generic declarations check at instantiation (§3.15)
        }
        match env.resolve(&named(name), None) {
            Ok(rt) => check_resolved(&env, &rep, &rt, name, &mut HashSet::new()),
            Err(msg) => map_resolve_err(&msg, name),
        }
    }

    // AST walks over all declarations
    for d in decls {
        *tparams.borrow_mut() = match &d.body {
            DeclBody::Type { params, .. } => params.iter().map(|p| p.name.clone()).collect(),
            _ => HashSet::new(),
        };
        match &d.body {
            DeclBody::Type { name, ty, .. } => walk.walk_type(Some(ty), 1, Some(name)),
            DeclBody::Const { ty, expr, .. } => {
                walk.walk_type(ty.as_ref(), 0, None);
                walk_expr(expr);
            }
            DeclBody::Func { params, ret, body, .. } => {
                params.iter().for_each(|p| walk.walk_type(p.ty.as_ref(), 0, None));
                walk.walk_type(ret.as_ref(), 0, None);
                walk_expr(body);
            }
            DeclBody::Output { ty, expr, .. } => {
                walk.walk_type(Some(ty), 0, None);
                walk_expr(expr);
            }
            DeclBody::Input { ty, fallback, .. } => {
                walk.walk_type(Some(ty), 0, None);
                if let Some(f) = fallback {
                    walk_expr(f);
                }
            }
            _ => {}
        }
    }

    // ---------- expression pass: inference, assignability, absence (§3.18, §4.10) ----------
    let cx0 = make_ctx(env.clone(), report.clone());
    let is_bool_ty = |t: &Ty| match &t.rt {
        None => true,
        Some(r) => matches!(&r.k, RTk::Prim(n) if n == "bool") || matches!(&r.k, RTk::Lit(Value::Bool(_))),
    };
    let rec_ctx = |cx: &Ctx, rt: &RT, ast: Option<&TypeAst>| -> Ctx {
        let mut c = cx.child();
        for m in rec_members(rt) {
            c.vars.insert(m.name.clone(), Ty { rt: member_ty(&m), abs: m.kind == MKind::Opt });
        }
        c.vars.insert("$this".into(), tyv(Some(rt.clone())));
        c.vars.insert("$path".into(), tyv(Some(ty(RTk::Prim("string".into())))));
        if let Some(TypeAst::Record { members, .. }) = ast {
            for m in members {
                if let MemberAst::Context { variable, ty } = m {
                    c.vars.insert(variable.clone(), tyv(try_resolve(&env, Some(ty))));
                }
            }
        }
        c
    };
    fn check_member_ast(env: &Rc<Env>, rep: &dyn Fn(&str, String), is_bool_ty: &dyn Fn(&Ty) -> bool, cx: &Ctx, m: &MemberAst) {
        match m {
            MemberAst::Value { ty, dflt: Some(d), .. } => {
                check_expr(cx, d, try_resolve(env, Some(ty)).as_ref());
            }
            MemberAst::Derived { ty, expr, .. } => {
                check_expr(cx, expr, try_resolve(env, ty.as_ref()).as_ref());
            }
            MemberAst::Assert { cond, tail, .. } => {
                if !is_bool_ty(&require_val(cx, cond, infer(cx, cond), "as an assert condition")) {
                    rep("E4001", "assert condition is not bool".into());
                }
                match tail {
                    Some(Tail::Inline { template, .. }) => {
                        for p in template {
                            if let TPart::Expr(x) = p {
                                infer(cx, x);
                            }
                        }
                    }
                    Some(Tail::Ref { args, .. }) => {
                        for a in args {
                            require_val(cx, a, infer(cx, a), "as a diagnostic argument");
                        }
                    }
                    None => {}
                }
            }
            MemberAst::When { cond, body } => {
                if !is_bool_ty(&require_val(cx, cond, infer(cx, cond), "as a when condition")) {
                    rep("E4001", "when condition is not bool".into());
                }
                let c2 = apply_guards(cx, guards_of(cond, true));
                for b in body {
                    check_member_ast(env, rep, is_bool_ty, &c2, b);
                }
            }
            _ => {}
        }
    }

    let seen_recs: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    fn check_record_exprs(
        env: &Rc<Env>,
        rep: &dyn Fn(&str, String),
        is_bool_ty: &dyn Fn(&Ty) -> bool,
        rec_ctx: &dyn Fn(&Ctx, &RT, Option<&TypeAst>) -> Ctx,
        seen: &RefCell<HashSet<usize>>,
        rt: &RT,
        cx: &Ctx,
        ast: Option<&TypeAst>,
    ) {
        let RTk::Rec(rec) = &rt.k else { return };
        let id = Rc::as_ptr(rt) as usize;
        if seen.borrow().contains(&id) {
            return;
        }
        seen.borrow_mut().insert(id);
        let cx_r = rec_ctx(cx, rt, ast);
        // member expressions and asserts check in their declaring module's
        // scope (§8.3) — same rule the engine follows at evaluation
        let cx_for = |menv: &Option<Rc<Env>>| -> Ctx {
            match menv {
                Some(m) if !Rc::ptr_eq(m, &cx_r.env) => cx_r.with_env(m.clone()),
                _ => cx_r.clone(),
            }
        };
        // D30/E4090: an embedded type's declared bounds must hold at this
        // site — the container is the parent, the collection's key or index
        // type is what $key ranges over (none for a direct member)
        let check_embedding = |member_rt: &RT, member_name: &str, key_rt: Option<&RT>| {
            let RTk::Rec(mrec) = &member_rt.k else { return };
            let site = format!("{}.{member_name}", rt.name.borrow().clone().unwrap_or_else(|| "record".into()));
            let who = member_rt.name.borrow().clone().unwrap_or_else(|| "the member type".into());
            for (var, cty) in mrec.ctx_decls.borrow().iter() {
                if var == "$parent" {
                    let RTk::Ref(bound) = &cty.k else { continue };
                    if !subsumes(env, rt, bound) {
                        rep("E4090", format!("embedding site {site} fails {who}'s $parent bound (§7.3)"));
                    }
                } else if var == "$key" {
                    match key_rt {
                        None => rep("E4090", format!("embedding site {site} gives $key no meaning: {who} is a direct member, not a collection element (§7.3)")),
                        Some(k) => {
                            if !subsumes(env, k, cty) {
                                rep("E4090", format!("embedding site {site} fails {who}'s $key bound (§7.3)"));
                            }
                        }
                    }
                }
            }
        };
        let int_rt: RT = ty(RTk::Prim("int".into()));
        let members = rec.members.borrow().clone();
        for m in &members {
            if m.kind == MKind::Der {
                if let Some(x) = &m.expr {
                    check_expr(&cx_for(&m.menv), x, m.ty.as_ref());
                }
            }
            if m.kind == MKind::Dflt {
                if let Some(d) = &m.dflt {
                    check_expr(&cx_for(&m.menv), d, m.ty.as_ref());
                }
            }
            let Some(mt) = &m.ty else { continue };
            let nested: Option<(RT, Option<RT>)> = match &mt.k {
                RTk::Rec(_) => Some((mt.clone(), None)),
                RTk::Arr { elem, .. } if is_rec(elem) => Some((elem.clone(), Some(int_rt.clone()))),
                RTk::Map { key, val } if is_rec(val) => Some((val.clone(), Some(key.clone()))),
                _ => None,
            };
            if let Some((n, key_rt)) = nested {
                check_embedding(&n, &m.name, key_rt.as_ref());
                check_record_exprs(env, rep, is_bool_ty, rec_ctx, seen, &n, &cx_for(&m.menv), None);
            }
        }
        let asserts = rec.asserts.borrow().clone();
        for a in &asserts {
            let m = if a.when {
                MemberAst::When { cond: a.cond.clone(), body: a.body.clone() }
            } else {
                MemberAst::Assert { name: a.name.clone(), cond: a.cond.clone(), tail: a.tail.clone() }
            };
            check_member_ast(env, rep, is_bool_ty, &cx_for(&a.menv), &m);
        }
    }

    for name in &type_order {
        let Some(decl) = type_entry(name) else { continue };
        if !decl.params.is_empty() {
            continue;
        }
        let Ok(rt) = env.resolve(&named(name), None) else { continue };
        check_record_exprs(&env, &rep, &is_bool_ty, &rec_ctx, &seen_recs, &rt, &cx0, Some(&decl.ast));
    }
    // D30/E4090 for $root: every record type owned (transitively) by an
    // evaluation root must have its declared $root bound met by the root's
    // own type — checked once per root declaration
    fn walk_root_bounds(env: &Rc<Env>, rep: &dyn Fn(&str, String), root_name: &str, root_rt: &RT, t: &RT, seen: &mut HashSet<usize>) {
        let id = Rc::as_ptr(t) as usize;
        if !seen.insert(id) {
            return;
        }
        match &t.k {
            RTk::Rec(r) => {
                for (var, cty) in r.ctx_decls.borrow().iter() {
                    if var != "$root" {
                        continue;
                    }
                    let RTk::Ref(bound) = &cty.k else { continue };
                    if !subsumes(env, root_rt, bound) {
                        rep("E4090", format!("root {root_name} fails {}'s $root bound (§7.3)", t.name.borrow().clone().unwrap_or_else(|| "a member type".into())));
                    }
                }
                for m in rec_members(t) {
                    if let Some(mt) = &m.ty {
                        walk_root_bounds(env, rep, root_name, root_rt, mt, seen);
                    }
                }
            }
            RTk::Arr { elem, .. } => walk_root_bounds(env, rep, root_name, root_rt, elem, seen),
            RTk::Map { val, .. } => walk_root_bounds(env, rep, root_name, root_rt, val, seen),
            RTk::Union(arms) | RTk::IsectN(arms) => arms.iter().for_each(|a| walk_root_bounds(env, rep, root_name, root_rt, a, seen)),
            RTk::Pred { base, .. } => walk_root_bounds(env, rep, root_name, root_rt, base, seen),
            _ => {}
        }
    }
    for d in decls {
        let (name, ty_ast) = match &d.body {
            DeclBody::Output { name, ty, .. } | DeclBody::Input { name, ty, .. } => (name, ty),
            _ => continue,
        };
        if let Some(rt) = try_resolve(&env, Some(ty_ast)) {
            walk_root_bounds(&env, &rep, name, &rt, &rt, &mut HashSet::new());
        }
    }
    for d in decls {
        match &d.body {
            DeclBody::Const { name, ty, expr } => {
                let exp = ty.as_ref().and_then(|t| resolve_or_report(Some(t), &format!("const {name}")));
                check_expr(&cx0, expr, exp.as_ref());
            }
            DeclBody::Func { name, params, ret, body } => {
                let mut cx_f = cx0.child();
                for p in params {
                    cx_f.vars.insert(p.name.clone(), tyv(resolve_or_report(p.ty.as_ref(), &format!("func {name}"))));
                }
                let exp = ret.as_ref().and_then(|t| resolve_or_report(Some(t), &format!("func {name}")));
                check_expr(&cx_f, body, exp.as_ref());
            }
            DeclBody::Output { name, ty, expr } => {
                let exp = resolve_or_report(Some(ty), &format!("output {name}"));
                check_expr(&cx0, expr, exp.as_ref());
            }
            DeclBody::Input { name, ty, fallback: Some(f) } => {
                let exp = resolve_or_report(Some(ty), &format!("input {name}"));
                check_expr(&cx0, f, exp.as_ref());
            }
            DeclBody::Input { name, ty, fallback: None } => {
                resolve_or_report(Some(ty), &format!("input {name}"));
            }
            DeclBody::Diagnostic { params, template, .. } => {
                let mut cx_d = cx0.child();
                for p in params {
                    cx_d.vars.insert(p.name.clone(), tyv(try_resolve(&env, p.ty.as_ref())));
                }
                for p in template {
                    if let TPart::Expr(x) = p {
                        infer(&cx_d, x);
                    }
                }
            }
            DeclBody::Unit { name, factor: Some(f), .. } => {
                if let Some(bad) = const_violation(f) {
                    rep("E4021", format!("non-constant unit factor for {name}: {bad} (§3.16)"));
                }
            }
            _ => {}
        }
    }
    *env.const_diag_sink.borrow_mut() = None;
    let result = out.borrow().clone();
    result
}

#[allow(dead_code)]
fn _unused(_: &HashMap<String, String>) {}
