//! Expression-level static analysis — a port of the reference
//! implementation's infer.ts: type inference, assignability (§3.18,
//! strict S ⊑ T), the absence discipline (§4.10) with its two narrowing
//! rules, and the `match` static checks (§4.7). Inference is
//! conservative: a form whose type cannot be determined yields `unknown`
//! (rt None) and suppresses downstream judgments rather than guessing.
use crate::ast::*;
use crate::semantics::*;
use crate::subsume::subsumes;
use num_bigint::BigInt;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Clone)]
pub struct Ty {
    pub rt: Option<RT>,
    pub abs: bool,
}
pub fn unk() -> Ty {
    Ty { rt: None, abs: false }
}
pub fn tyv(rt: Option<RT>) -> Ty {
    Ty { rt, abs: false }
}
pub fn prim(name: &str) -> RT {
    ty(RTk::Prim(name.to_string()))
}
fn bool_ty() -> Ty {
    tyv(Some(prim("bool")))
}

pub type Report = Rc<dyn Fn(&str, String)>;

#[derive(Clone)]
pub struct Ctx {
    pub env: Rc<Env>,
    pub report: Report,
    pub vars: HashMap<String, Ty>,
    pub present: HashSet<String>,
    pub nonnull: HashSet<String>,
    pub const_memo: Rc<RefCell<HashMap<String, Ty>>>,
}
impl Ctx {
    pub fn report(&self, code: &str, msg: String) {
        (self.report)(code, msg)
    }
    pub fn child(&self) -> Ctx {
        self.clone()
    }
    pub fn with_env(&self, env: Rc<Env>) -> Ctx {
        let mut c = self.clone();
        c.env = env;
        c
    }
}
pub fn make_ctx(env: Rc<Env>, report: Report) -> Ctx {
    Ctx { env, report, vars: HashMap::new(), present: HashSet::new(), nonnull: HashSet::new(), const_memo: Rc::new(RefCell::new(HashMap::new())) }
}

// ---------------- JS-faithful helpers ----------------
pub fn js_typeof(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "boolean",
        Value::Int(_) => "bigint",
        Value::Float(_) => "number",
        Value::Str(_) => "string",
        _ => "object",
    }
}
pub fn js_str(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => if *b { "true".into() } else { "false".into() },
        Value::Int(i) => i.to_string(),
        Value::Float(f) => js_num_str(*f),
        Value::Str(s) => s.clone(),
        other => format!("{other:?}"),
    }
}
pub fn tag(rt: &RT) -> &'static str {
    match &rt.k {
        RTk::Prim(_) => "prim",
        RTk::Lit(_) => "lit",
        RTk::Range { .. } => "range",
        RTk::Pattern { .. } => "pattern",
        RTk::Arr { .. } => "arr",
        RTk::Map { .. } => "map",
        RTk::Union(_) => "union",
        RTk::IsectN(_) => "isectN",
        RTk::Rec(_) => "rec",
        RTk::Pred { .. } => "pred",
        RTk::Ref(_) => "ref",
        RTk::Quantity(_) => "quantity",
        RTk::Func { .. } => "func",
        RTk::Any => "any",
    }
}
/// a function's unknown result is carried as `any`
fn ret_of(rt: &RT) -> Option<RT> {
    if matches!(rt.k, RTk::Any) { None } else { Some(rt.clone()) }
}
pub fn member_ty(m: &Member) -> Option<RT> {
    match &m.conj {
        Some(c) => Some(ty(RTk::IsectN(c.clone()))),
        None => m.ty.clone(),
    }
}
fn find_member<'a>(members: &'a [Member], name: &str) -> Option<&'a Member> {
    members.iter().find(|m| m.name == name)
}

// ---------------- type utilities ----------------
fn is_null_lit(t: &RT) -> bool {
    matches!(&t.k, RTk::Lit(Value::Null)) || matches!(&t.k, RTk::Prim(n) if n == "null")
}
pub fn has_null(rt: Option<&RT>) -> bool {
    match rt {
        None => false,
        Some(t) => is_null_lit(t) || matches!(&t.k, RTk::Union(arms) if arms.iter().any(|a| has_null(Some(a)))),
    }
}
fn strip_null(rt: &RT) -> RT {
    if let RTk::Union(arms) = &rt.k {
        let kept: Vec<RT> = arms.iter().filter(|a| !is_null_lit(a)).cloned().collect();
        return if kept.len() == 1 { kept[0].clone() } else { ty(RTk::Union(kept)) };
    }
    rt.clone()
}
fn same_rt(a: &RT, b: &RT) -> bool {
    if Rc::ptr_eq(a, b) {
        return true;
    }
    match (&a.k, &b.k) {
        (RTk::Prim(x), RTk::Prim(y)) => x == y,
        (RTk::Lit(x), RTk::Lit(y)) => js_typeof(x) == js_typeof(y) && value_eq(x, y),
        _ => false,
    }
}
pub fn mk_union(arms: Vec<Option<RT>>) -> Option<RT> {
    if arms.iter().any(|a| a.is_none()) {
        return None;
    }
    let mut flat: Vec<RT> = vec![];
    for a in arms.into_iter().flatten() {
        match &a.k {
            RTk::Union(xs) => flat.extend(xs.iter().cloned()),
            _ => flat.push(a),
        }
    }
    let mut uniq: Vec<RT> = vec![];
    for a in flat {
        if !uniq.iter().any(|b| same_rt(&a, b)) {
            uniq.push(a);
        }
    }
    Some(if uniq.len() == 1 { uniq[0].clone() } else { ty(RTk::Union(uniq)) })
}
pub fn num_kind(rt: Option<&RT>) -> Option<String> {
    let rt = rt?;
    match &rt.k {
        RTk::Prim(n) => if ["int", "float", "string", "bool"].contains(&n.as_str()) { Some(n.clone()) } else { None },
        RTk::Lit(v) => match v {
            Value::Bool(_) => Some("bool".into()),
            Value::Int(_) => Some("int".into()),
            Value::Float(_) => Some("float".into()),
            Value::Str(_) => Some("string".into()),
            _ => None,
        },
        RTk::Range { base, .. } => Some(base.clone()),
        RTk::Pattern { .. } => Some("string".into()),
        RTk::Pred { base, .. } => num_kind(Some(base)),
        RTk::Quantity(_) => Some("quantity".into()),
        RTk::Union(arms) => {
            let ks: Vec<Option<String>> = arms.iter().map(|a| num_kind(Some(a))).collect();
            match ks.first() {
                Some(Some(k0)) if ks.iter().all(|k| k.as_ref() == Some(k0)) => Some(k0.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}
fn is_boolish(rt: Option<&RT>) -> bool {
    rt.is_none() || num_kind(rt).as_deref() == Some("bool")
}
/// structural view: unwrap ref/pred and select the arm of an intersection
/// that has the wanted shape (merged `&` members carry conj arms)
fn arm_of(rt: Option<&RT>, t: &str) -> Option<RT> {
    let rt = rt?;
    match &rt.k {
        RTk::Ref(target) if t != "ref" => return arm_of(Some(target), t),
        RTk::Pred { base, .. } => return arm_of(Some(base), t),
        _ => {}
    }
    if tag(rt) == t {
        return Some(rt.clone());
    }
    if let RTk::IsectN(arms) = &rt.k {
        for x in arms {
            if let Some(v) = arm_of(Some(x), t) {
                return Some(v);
            }
        }
    }
    if let RTk::Union(arms) = &rt.k {
        let sub: Vec<RT> = arms.iter().filter_map(|x| arm_of(Some(x), t)).collect();
        if sub.len() == arms.len() && !sub.is_empty() {
            if t == "arr" {
                let elems: Vec<RT> = sub.iter().filter_map(|a| if let RTk::Arr { elem, .. } = &a.k { Some(elem.clone()) } else { None }).collect();
                return Some(ty(RTk::Arr { elem: ty(RTk::Union(elems)), lo: None, hi: None }));
            }
            return Some(sub[0].clone());
        }
    }
    None
}

// ---------------- navigation paths & narrowing ----------------
pub fn path_key(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.clone()),
        Expr::Ctx(n) => Some(n.clone()),
        Expr::Paren(x) => path_key(x),
        Expr::Member { x, name, safe } => {
            if *safe {
                return None;
            }
            path_key(x).map(|b| format!("{b}.{name}"))
        }
        Expr::Index { x, i } => {
            let b = path_key(x)?;
            match &**i {
                Expr::Lit(v) => Some(format!("{b}[{}]", js_str(v))),
                Expr::Name(n) => Some(format!("{b}[{n}]")),
                _ => None,
            }
        }
        _ => None,
    }
}
#[derive(Default)]
pub struct Guards {
    pub present: Vec<String>,
    pub nonnull: Vec<String>,
}
fn merge(mut a: Guards, b: Guards) -> Guards {
    a.present.extend(b.present);
    a.nonnull.extend(b.nonnull);
    a
}
pub fn guards_of(e: &Expr, polarity: bool) -> Guards {
    match e {
        Expr::Paren(x) => guards_of(x, polarity),
        Expr::Un { op, x } => if op == "!" { guards_of(x, !polarity) } else { Guards::default() },
        Expr::Bin { op, l, r } => {
            if op == "&&" && polarity {
                return merge(guards_of(l, true), guards_of(r, true));
            }
            if op == "||" && !polarity {
                return merge(guards_of(l, false), guards_of(r, false));
            }
            if op == "in" && polarity {
                let Some(b) = path_key(r) else { return Guards::default() };
                return match &**l {
                    Expr::Lit(Value::Str(s)) => Guards { present: vec![format!("{b}.{s}"), format!("{b}[{s}]")], nonnull: vec![] },
                    Expr::Name(n) => Guards { present: vec![format!("{b}[{n}]")], nonnull: vec![] },
                    _ => Guards::default(),
                };
            }
            let null_side = if matches!(&**l, Expr::Lit(Value::Null)) { Some(r) } else if matches!(&**r, Expr::Lit(Value::Null)) { Some(l) } else { None };
            if let Some(side) = null_side {
                if let Some(p) = path_key(side) {
                    if (op == "!=" && polarity) || (op == "==" && !polarity) {
                        return Guards { present: vec![], nonnull: vec![p] };
                    }
                }
            }
            Guards::default()
        }
        _ => Guards::default(),
    }
}
/// is a name already taken here? (locals or the module namespace — the
/// no-shadowing rule E3019 spans both)
fn name_bound(cx: &Ctx, n: &str) -> bool {
    cx.vars.contains_key(n)
        || cx.env.consts.borrow().contains_key(n)
        || cx.env.funcs.borrow().contains_key(n)
        || cx.env.type_asts.borrow().contains_key(n)
        || cx.env.inputs.borrow().contains_key(n)
        || cx.env.outputs.borrow().iter().any(|(o, _, _)| o == n)
}
pub fn apply_guards(cx: &Ctx, g: Guards) -> Ctx {
    let mut c2 = cx.child();
    c2.present.extend(g.present);
    c2.nonnull.extend(g.nonnull);
    c2
}

// ---------------- stdlib signatures (arity + result) ----------------
fn std_sig(name: &str) -> Option<(usize, Option<RT>)> {
    let arr_str = || ty(RTk::Arr { elem: prim("string"), lo: None, hi: None });
    let pred_fn = || ty(RTk::Func { params: vec![prim("int")], ret: prim("bool") });
    Some(match name {
        "array.count" => (1, Some(prim("int"))),
        "array.all" => (2, Some(prim("bool"))),
        "array.any" => (2, Some(prim("bool"))),
        "array.filter" => (2, None),
        "array.all_distinct" => (1, Some(prim("bool"))),
        "array.sum" => (1, None),
        "array.fold" => (3, None),
        "map.keys" => (1, Some(arr_str())),
        "map.values" => (1, None),
        "string.length" => (1, Some(prim("int"))),
        "string.of" => (1, Some(prim("string"))),
        "string.join" => (2, Some(prim("string"))),
        "string.starts_with" => (2, Some(prim("bool"))),
        "string.ends_with" => (2, Some(prim("bool"))),
        "string.contains" => (2, Some(prim("bool"))),
        "string.split" => (2, Some(arr_str())),
        "map.entries" => (1, None),
        "ref.path" => (1, Some(prim("string"))),
        "math.abs" => (1, None),
        "math.min" => (2, None),
        "math.max" => (2, None),
        "math.clog2" => (1, Some(prim("int"))),
        "math.floor" => (1, Some(prim("int"))),
        "math.ceil" => (1, Some(prim("int"))),
        "math.round" => (1, Some(prim("int"))),
        "int.of" => (1, Some(prim("int"))),
        "int.at_least" => (1, Some(pred_fn())),
        "int.at_most" => (1, Some(pred_fn())),
        "float.of" => (1, Some(prim("float"))),
        "object.merge" => (2, None),
        _ => return None,
    })
}
fn std_path(e: &Expr) -> Option<String> {
    match e {
        Expr::Member { x, name, safe: false } => {
            let b = std_path(x)?;
            Some(if b.is_empty() { name.clone() } else { format!("{b}.{name}") })
        }
        Expr::Name(n) if n == "std" => Some(String::new()),
        _ => None,
    }
}

// ---------------- the judgment ----------------
pub fn try_resolve(env: &Rc<Env>, ast: Option<&TypeAst>) -> Option<RT> {
    env.resolve(ast?, None).ok()
}
pub fn named(name: &str) -> TypeAst {
    TypeAst::Named { name: name.to_string(), args: vec![], preds: None, ext: None }
}

pub fn require_val(cx: &Ctx, e: &Expr, ty: Ty, what: &str) -> Ty {
    if ty.abs {
        let k = path_key(e);
        if !k.map(|k| cx.present.contains(&k)).unwrap_or(false) {
            cx.report("E4050", format!("maybe-absent expression consumed {what} (use ?. / ?? or an `in` guard)"));
        }
    }
    ty
}

pub fn infer(cx: &Ctx, e: &Rc<Expr>) -> Ty {
    match &**e {
        Expr::Lit(v) => tyv(Some(ty(RTk::Lit(v.clone())))),
        Expr::Pattern(src) => match pattern_error(src) {
            Some(bad) => {
                cx.report("E4119", format!("malformed pattern /{src}/: {bad}"));
                unk()
            }
            None => match compile_pattern(src) {
                Ok(re) => tyv(Some(ty(RTk::Pattern { src: src.clone(), re }))),
                Err(_) => unk(),
            },
        },
        Expr::UnitLit { unit, .. } => match cx.env.unit_info(unit) {
            Ok((key, _)) => tyv(Some(ty(RTk::Quantity(key)))),
            Err(msg) => {
                cx.report("E4073", msg);
                unk()
            }
        },
        Expr::Template(parts) => {
            for p in parts {
                if let TPart::Expr(x) = p {
                    require_val(cx, x, infer(cx, x), "in a template");
                }
            }
            tyv(Some(prim("string")))
        }
        Expr::Name(name) => {
            if let Some(t) = cx.vars.get(name) {
                return t.clone();
            }
            let env = &cx.env;
            if env.consts.borrow().contains_key(name) {
                return const_ty(cx, name);
            }
            if env.funcs.borrow().contains_key(name) {
                return tyv(Some(func_rt(cx, name)));
            }
            if name == "std" {
                return unk();
            }
            let out = env.outputs.borrow().iter().find(|(o, _, _)| o == name).map(|(_, t, _)| t.clone());
            if let Some(t) = out {
                return tyv(try_resolve(env, Some(&t)));
            }
            let inp = env.inputs.borrow().get(name).map(|(t, _)| t.clone());
            if let Some(t) = inp {
                return tyv(try_resolve(env, Some(&t)));
            }
            let im = env.imports.borrow().get(name).cloned();
            if let Some(im) = im {
                return imported_ty(cx, &im);
            }
            if env.namespaces.borrow().contains_key(name) {
                cx.report("E3008", format!("namespace name {name} used as a value"));
                return unk();
            }
            if env.type_asts.borrow().contains_key(name) {
                cx.report("E3008", format!("type/namespace name {name} used as a value"));
                return unk();
            }
            cx.report("E3003", format!("unknown name {name}"));
            unk()
        }
        Expr::Ctx(n) => cx.vars.get(n).cloned().unwrap_or_else(unk),
        Expr::Referrers { ty: tn, .. } => {
            let rt = try_resolve(&cx.env, Some(&named(tn)));
            match &rt {
                None => cx.report("E4091", format!("$referrers: unknown record type {tn}")),
                Some(r) if !is_rec(r) => cx.report("E4091", format!("$referrers: {tn} is not a record type")),
                _ => {}
            }
            tyv(rt.filter(is_rec).map(|r| ty(RTk::Arr { elem: ty(RTk::Ref(r)), lo: None, hi: None })))
        }
        Expr::Obj(entries) => {
            for (_, v) in entries {
                require_val(cx, v, infer(cx, v), "as a construction member");
            }
            unk() // literals are typed by their checked position (§3.18)
        }
        Expr::Arr(items) => {
            let ts: Vec<Option<RT>> = items
                .iter()
                .map(|(spread, x)| {
                    let t = require_val(cx, x, infer(cx, x), "as an array element");
                    if *spread {
                        t.rt.and_then(|r| if let RTk::Arr { elem, .. } = &r.k { Some(elem.clone()) } else { None })
                    } else {
                        t.rt
                    }
                })
                .collect();
            tyv(mk_union(ts).map(|elem| ty(RTk::Arr { elem, lo: None, hi: None })))
        }
        Expr::Comp { head, clauses } => {
            let c2 = bind_clauses(cx, clauses);
            let h = require_val(&c2, head, infer(&c2, head), "as a comprehension element");
            tyv(h.rt.map(|elem| ty(RTk::Arr { elem, lo: None, hi: None })))
        }
        Expr::MapComp { key, val, clauses } => {
            let c2 = bind_clauses(cx, clauses);
            let k = require_val(&c2, key, infer(&c2, key), "as a map key");
            if k.rt.is_some() && num_kind(k.rt.as_ref()).as_deref() != Some("string") {
                cx.report("E4001", "map-comprehension key is not a string".into());
            }
            let v = require_val(&c2, val, infer(&c2, val), "as a map value");
            tyv(v.rt.map(|val| ty(RTk::Map { key: prim("string"), val })))
        }
        Expr::Bin { .. } => infer_bin(cx, e),
        Expr::Un { op, x } => {
            let t = require_val(cx, x, infer(cx, x), &format!("as `{op}` operand"));
            if op == "!" {
                if t.rt.is_some() && !is_boolish(t.rt.as_ref()) {
                    cx.report("E4071", "`!` on a non-bool operand".into());
                }
                return bool_ty();
            }
            if op == "~" {
                if t.rt.is_some() && num_kind(t.rt.as_ref()).as_deref() != Some("int") {
                    cx.report("E4071", "`~` on a non-int operand".into());
                }
                return tyv(Some(prim("int")));
            }
            let k = num_kind(t.rt.as_ref());
            if t.rt.is_some() && !matches!(k.as_deref(), Some("int") | Some("float") | Some("quantity")) {
                cx.report("E4071", "unary `-` on a non-numeric operand".into());
            }
            tyv(match k.as_deref() {
                Some("int") => Some(prim("int")),
                Some("float") => Some(prim("float")),
                _ => None,
            })
        }
        Expr::Paren(x) => infer(cx, x),
        Expr::If { c, t, f } => {
            let ct = require_val(cx, c, infer(cx, c), "as a condition");
            if ct.rt.is_some() && !is_boolish(ct.rt.as_ref()) {
                cx.report("E4001", "`if` condition is not bool".into());
            }
            let tt = infer(&apply_guards(cx, guards_of(c, true)), t);
            let ft = infer(&apply_guards(cx, guards_of(c, false)), f);
            Ty { rt: mk_union(vec![tt.rt, ft.rt]), abs: tt.abs || ft.abs }
        }
        Expr::Lambda { params, body } => {
            let mut c2 = cx.child();
            for p in params {
                if name_bound(&c2, p) {
                    cx.report("E3019", format!("lambda parameter {p} shadows an enclosing name"));
                }
                c2.vars.insert(p.clone(), unk());
            }
            infer(&c2, body);
            unk()
        }
        Expr::Call { .. } => infer_call(cx, e),
        Expr::Member { .. } => infer_member(cx, e),
        Expr::Index { x, .. } => {
            let b = require_val(cx, x, infer(cx, x), "for indexing");
            index_core(cx, b, e)
        }
        Expr::With { base, patch } => {
            let b = require_val(cx, base, infer(cx, base), "as `with` base");
            let brt = match &b.rt {
                Some(r) => match &r.k {
                    RTk::Ref(t) => Some(t.clone()),
                    _ => Some(r.clone()),
                },
                None => None,
            };
            if let Some(r) = &brt {
                if !is_rec(r) {
                    cx.report("E4080", "`with` on a non-record base".into());
                    return unk();
                }
            }
            if let (Expr::Obj(entries), Some(r)) = (&**patch, &brt) {
                let RTk::Rec(rec) = &r.k else { unreachable!() };
                let members = rec.members.borrow();
                for (k, _) in entries {
                    match find_member(&members, k) {
                        None if !rec.open.get() => cx.report("E4080", format!("`with` updates unknown member {k}")),
                        Some(m) if m.kind == MKind::Der => cx.report("E4080", format!("`with` updates derived member {k}")),
                        _ => {}
                    }
                }
            }
            if let Expr::Obj(entries) = &**patch {
                for (_, v) in entries {
                    require_val(cx, v, infer(cx, v), "as a `with` update");
                }
            } else {
                infer(cx, patch);
            }
            tyv(brt)
        }
        Expr::Match { .. } => infer_match(cx, e, None),
    }
}

fn bind_clauses(cx: &Ctx, clauses: &[ForClause]) -> Ctx {
    let mut c2 = cx.child();
    for cl in clauses {
        let vt = iter_var_ty(&c2, &cl.iter);
        if name_bound(&c2, &cl.v) {
            cx.report("E3019", format!("comprehension variable {} shadows an enclosing name", cl.v));
        }
        c2.vars.insert(cl.v.clone(), vt);
        for f in &cl.filters {
            require_val(&c2, f, infer(&c2, f), "as a filter");
            c2 = apply_guards(&c2, guards_of(f, true));
        }
    }
    c2
}

fn iter_var_ty(cx: &Ctx, it: &Rc<Expr>) -> Ty {
    let t = require_val(cx, it, infer(cx, it), "as an iterable");
    if let Expr::Bin { op, l, r } = &**it {
        if op == ".." || op == "..<" {
            let lo = if let Expr::Lit(v) = &**l { Some(v.clone()) } else { None };
            let hi = if let Expr::Lit(v) = &**r { Some(v.clone()) } else { None };
            if matches!(lo, Some(Value::Float(_))) || matches!(hi, Some(Value::Float(_))) {
                cx.report("E4115", "comprehension over a float range".into());
                return unk();
            }
            if let (Some(lo), Some(hi)) = (lo, hi) {
                return tyv(Some(ty(RTk::Range { lo, hi, excl: op == "..<", base: "int".into() })));
            }
            return tyv(Some(prim("int")));
        }
    }
    let Some(rt) = &t.rt else { return unk() };
    if let Some(a) = arm_of(Some(rt), "arr") {
        if let RTk::Arr { elem, .. } = &a.k {
            return tyv(Some(elem.clone()));
        }
    }
    let what = if arm_of(Some(rt), "map").is_some() { "map (use std.map.keys/values)" } else { "value" };
    cx.report("E4115", format!("comprehension over a non-iterable {what}"));
    unk()
}

fn q_dim(rt: Option<&RT>) -> Option<String> {
    match rt.map(|r| &r.k) {
        Some(RTk::Quantity(d)) => Some(d.clone()),
        Some(RTk::Pred { base, .. }) => match &base.k {
            RTk::Quantity(d) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn infer_bin(cx: &Ctx, e: &Rc<Expr>) -> Ty {
    let Expr::Bin { op, l, r } = &**e else { return unk() };
    let op = op.as_str();
    if op == "|>" {
        // first-argument insertion (§4.9)
        let call = match &**r {
            Expr::Call { fun, args } => {
                let mut a = vec![l.clone()];
                a.extend(args.iter().cloned());
                Expr::Call { fun: fun.clone(), args: a }
            }
            _ => Expr::Call { fun: r.clone(), args: vec![l.clone()] },
        };
        return infer_call(cx, &Rc::new(call));
    }
    if op == "??" {
        let lt = infer(cx, l); // absence/null on the left is the point
        let rt = require_val(cx, r, infer(cx, r), "as `??` fallback");
        return tyv(match (lt.rt, rt.rt) {
            (Some(a), Some(b)) => mk_union(vec![Some(strip_null(&a)), Some(b)]),
            _ => None,
        });
    }
    if op == "&&" || op == "||" {
        let lt = require_val(cx, l, infer(cx, l), &format!("as `{op}` operand"));
        if lt.rt.is_some() && !is_boolish(lt.rt.as_ref()) {
            cx.report("E4071", format!("`{op}` on a non-bool operand"));
        }
        let c2 = apply_guards(cx, guards_of(l, op == "&&"));
        let rt = require_val(&c2, r, infer(&c2, r), &format!("as `{op}` operand"));
        if rt.rt.is_some() && !is_boolish(rt.rt.as_ref()) {
            cx.report("E4071", format!("`{op}` on a non-bool operand"));
        }
        return bool_ty();
    }
    if op == "in" {
        require_val(cx, l, infer(cx, l), "as `in` key");
        let rt = require_val(cx, r, infer(cx, r), "as `in` container");
        let rrt = rt.rt.map(|x| match &x.k {
            RTk::Ref(t) => t.clone(),
            _ => x,
        });
        if let (Some(rr), Expr::Lit(Value::Str(key))) = (&rrt, &**l) {
            if let RTk::Rec(rec) = &rr.k {
                let members = rec.members.borrow();
                match find_member(&members, key) {
                    Some(m) if m.kind != MKind::Opt => cx.report("E4054", format!("`in` on member {key}, which is not optional")),
                    None if !rec.open.get() => cx.report("E4054", format!("`in` on undeclared member {key} of a closed record")),
                    _ => {}
                }
            }
        }
        return bool_ty();
    }
    if op == ".." || op == "..<" {
        require_val(cx, l, infer(cx, l), "as a range endpoint");
        require_val(cx, r, infer(cx, r), "as a range endpoint");
        return unk(); // a range value: iterable / membership container only
    }
    let lt = require_val(cx, l, infer(cx, l), &format!("as `{op}` operand"));
    let rt = require_val(cx, r, infer(cx, r), &format!("as `{op}` operand"));
    if op == "matches" {
        if lt.rt.is_some() && num_kind(lt.rt.as_ref()).as_deref() != Some("string") {
            cx.report("E4071", "`matches` needs a string left operand".into());
        }
        return bool_ty();
    }
    if op == "==" || op == "!=" {
        return bool_ty();
    }
    let lk = num_kind(lt.rt.as_ref());
    let rk = num_kind(rt.rt.as_ref());
    let cmp = ["<", "<=", ">", ">="].contains(&op);
    let (lks, rks) = (lk.as_deref(), rk.as_deref());
    if lks == Some("quantity") || rks == Some("quantity") {
        // §3.16: +/-/compare need equal dimensions; * and / compose them;
        // a bare int/float scales; a cancelled vector is a plain number
        if op == "+" || op == "-" || cmp {
            if lt.rt.is_some() && rt.rt.is_some() {
                if lks != Some("quantity") || rks != Some("quantity") {
                    let other = if lks == Some("quantity") { rks } else { lks };
                    cx.report("E4071", format!("`{op}` mixes quantity and {}", other.unwrap_or("null")));
                } else {
                    let (a, b) = (q_dim(lt.rt.as_ref()), q_dim(rt.rt.as_ref()));
                    if let (Some(a), Some(b)) = (&a, &b) {
                        if a != b {
                            let one = |s: &str| if s.is_empty() { "1".to_string() } else { s.to_string() };
                            cx.report("E4072", format!("`{op}` on quantities of different dimensions ({} vs {})", one(a), one(b)));
                        }
                    }
                }
            }
            return if cmp { bool_ty() } else { tyv(if lks == Some("quantity") { lt.rt.clone() } else { rt.rt.clone() }) };
        }
        if op == "*" || op == "/" {
            let (Some(_), Some(_)) = (&lt.rt, &rt.rt) else { return unk() };
            let (lv, rv) = (q_dim(lt.rt.as_ref()), q_dim(rt.rt.as_ref()));
            let numeric = |k: Option<&str>| matches!(k, Some("int") | Some("float"));
            if (lv.is_none() && !numeric(lks)) || (rv.is_none() && !numeric(rks)) {
                cx.report("E4071", format!("`{op}` on a non-numeric operand"));
                return unk();
            }
            let key = key_of_vec(&vec_combine(
                &lv.as_deref().map(vec_of_key).unwrap_or_default(),
                &rv.as_deref().map(vec_of_key).unwrap_or_default(),
                if op == "*" { 1 } else { -1 },
            ));
            return tyv(Some(if key.is_empty() { prim("float") } else { ty(RTk::Quantity(key)) }));
        }
        cx.report("E4071", format!("`{op}` on quantity operands"));
        return unk();
    }
    if let (Some(_), Some(_), Some(a), Some(b)) = (&lt.rt, &rt.rt, lks, rks) {
        if a != b {
            cx.report("E4071", format!("`{op}` mixes {a} and {b} operands"));
        }
    }
    if cmp {
        return bool_ty();
    }
    if ["&", "^", "<<", ">>"].contains(&op) {
        if (lt.rt.is_some() && lks != Some("int")) || (rt.rt.is_some() && rks != Some("int")) {
            cx.report("E4071", format!("`{op}` on non-int operands"));
        }
        return tyv(Some(prim("int")));
    }
    if op == "|" {
        // bitwise on ints (type-level | never reaches expressions)
        if (lt.rt.is_some() && lks != Some("int")) || (rt.rt.is_some() && rks != Some("int")) {
            cx.report("E4071", "`|` on non-int operands".into());
        }
        return tyv(Some(prim("int")));
    }
    // + - * / %
    if op == "+" && lks == Some("string") && rks == Some("string") {
        return tyv(Some(prim("string")));
    }
    if let (Some(lrt), Some(rrt), Some(a), Some(_)) = (&lt.rt, &rt.rt, lks, rks) {
        if !["int", "float", "quantity"].contains(&a) {
            cx.report("E4071", format!("`{op}` on {a} operands"));
        }
        if a == "int" && ["+", "-", "*"].contains(&op) {
            // interval arithmetic keeps range-typed operands range-typed, so
            // `9000 + i` with i: 0..<3 stays assignable where 1..65535 is expected
            if let (Some(x), Some(y)) = (as_ival(lrt), as_ival(rrt)) {
                let cands: Vec<BigInt> = match op {
                    "+" => vec![&x.0 + &y.0, &x.1 + &y.1],
                    "-" => vec![&x.0 - &y.1, &x.1 - &y.0],
                    _ => vec![&x.0 * &y.0, &x.0 * &y.1, &x.1 * &y.0, &x.1 * &y.1],
                };
                let lo = cands.iter().min().unwrap().clone();
                let hi = cands.iter().max().unwrap().clone();
                return tyv(Some(ty(RTk::Range { lo: Value::Int(lo), hi: Value::Int(hi), excl: false, base: "int".into() })));
            }
        }
        return tyv(if a == "int" || a == "float" { Some(prim(a)) } else { None });
    }
    unk()
}

fn as_ival(rt: &RT) -> Option<(BigInt, BigInt)> {
    match &rt.k {
        RTk::Lit(Value::Int(i)) => Some((i.clone(), i.clone())),
        RTk::Range { lo: Value::Int(lo), hi: Value::Int(hi), excl, base } if base == "int" => {
            Some((lo.clone(), if *excl { hi - 1 } else { hi.clone() }))
        }
        RTk::Union(arms) => {
            let ivs: Vec<Option<(BigInt, BigInt)>> = arms.iter().map(as_ival).collect();
            if !ivs.is_empty() && ivs.iter().all(|v| v.is_some()) {
                let ivs: Vec<(BigInt, BigInt)> = ivs.into_iter().flatten().collect();
                let lo = ivs.iter().map(|v| v.0.clone()).min().unwrap();
                let hi = ivs.iter().map(|v| v.1.clone()).max().unwrap();
                return Some((lo, hi));
            }
            None
        }
        RTk::Pred { base, .. } => as_ival(base),
        _ => None,
    }
}

fn index_core(cx: &Ctx, b: Ty, e: &Rc<Expr>) -> Ty {
    let Expr::Index { i, .. } = &**e else { return unk() };
    let it = require_val(cx, i, infer(cx, i), "as an index");
    let Some(brt) = &b.rt else { return unk() };
    if let Some(a) = arm_of(Some(brt), "arr") {
        if it.rt.is_some() && num_kind(it.rt.as_ref()).as_deref() != Some("int") {
            cx.report("E4071", "array index is not an int".into());
        }
        if let RTk::Arr { elem, .. } = &a.k {
            return tyv(Some(elem.clone()));
        }
    }
    if let Some(m) = arm_of(Some(brt), "map") {
        let k = path_key(e);
        if let RTk::Map { val, .. } = &m.k {
            return Ty { rt: Some(val.clone()), abs: !k.map(|k| cx.present.contains(&k)).unwrap_or(false) };
        }
    }
    if arm_of(Some(brt), "rec").is_some() {
        return unk(); // dynamic member access
    }
    cx.report("E4071", "indexing a non-collection".into());
    unk()
}

/// a name imported from another module, typed in that module's scope
fn imported_ty(cx: &Ctx, ex: &Export) -> Ty {
    let t = &ex.env;
    let name = &ex.name;
    let c = t.consts.borrow().get(name).cloned();
    if let Some(c) = c {
        return tyv(try_resolve(t, c.ty.as_ref()));
    }
    let f = t.funcs.borrow().get(name).cloned();
    if let Some(f) = f {
        return tyv(Some(func_rt_of(t, &f)));
    }
    let out = t.outputs.borrow().iter().find(|(o, _, _)| o == name).map(|(_, ty, _)| ty.clone());
    if let Some(o) = out {
        return tyv(try_resolve(t, Some(&o)));
    }
    let inp = t.inputs.borrow().get(name).map(|(ty, _)| ty.clone());
    if let Some(i) = inp {
        return tyv(try_resolve(t, Some(&i)));
    }
    if t.type_asts.borrow().contains_key(name) {
        cx.report("E3008", format!("type name {name} used as a value"));
        return unk();
    }
    unk()
}

fn infer_member(cx: &Ctx, e: &Rc<Expr>) -> Ty {
    let Expr::Member { x, name, safe } = &**e else { return unk() };
    if std_path(e).is_some() {
        return unk(); // std.* namespace path (typed at the call)
    }
    if let Expr::Name(xn) = &**x {
        if !cx.vars.contains_key(xn) {
            let ns = cx.env.namespaces.borrow().get(xn).map(|(_, ex)| ex.clone());
            if let Some(exports) = ns {
                let ex = exports.borrow().get(name).cloned();
                let Some(ex) = ex else {
                    cx.report("E3005", format!("namespace {xn} has no export {name}"));
                    return unk();
                };
                return imported_ty(cx, &ex);
            }
        }
    }
    let b = infer(cx, x);
    let key = path_key(x);
    if !*safe {
        let present = key.as_ref().map(|k| cx.present.contains(k)).unwrap_or(false);
        let nonnull = key.as_ref().map(|k| cx.nonnull.contains(k)).unwrap_or(false);
        if b.abs && !present {
            cx.report("E4050", "member access on a maybe-absent expression (use ?. or an `in` guard)".into());
        }
        if has_null(b.rt.as_ref()) && !nonnull {
            cx.report("E4051", format!("member .{name} on a possibly-null expression without ?."));
        }
    }
    member_core(cx, b, e)
}

fn member_core(cx: &Ctx, b: Ty, e: &Rc<Expr>) -> Ty {
    let Expr::Member { name, safe, .. } = &**e else { return unk() };
    let mut brt = b.rt.as_ref().map(strip_null);
    if let Some(RTk::Ref(t)) = brt.as_ref().map(|r| &r.k) {
        brt = Some(t.clone());
    }
    if let Some(RTk::Pred { base, .. }) = brt.as_ref().map(|r| &r.k) {
        brt = Some(base.clone());
    }
    if let Some(r) = &brt {
        if matches!(r.k, RTk::IsectN(_)) {
            brt = arm_of(Some(r), "rec").or_else(|| arm_of(Some(r), "map")).or_else(|| Some(r.clone()));
        }
    }
    let mk_abs = |t: Ty| if *safe { Ty { rt: t.rt, abs: true } } else { t };
    let Some(brt) = brt else { return mk_abs(unk()) };
    match &brt.k {
        RTk::Rec(rec) => {
            let members = rec.members.borrow();
            let Some(m) = find_member(&members, name) else {
                if !rec.open.get() {
                    cx.report("E4003", format!("member {name} is not declared on {}", brt.name.borrow().clone().unwrap_or_else(|| "this record".into())));
                }
                return mk_abs(unk());
            };
            let rt = member_ty(m);
            let present = path_key(e).map(|k| cx.present.contains(&k)).unwrap_or(false);
            Ty { rt, abs: *safe || (m.kind == MKind::Opt && !present) }
        }
        RTk::Map { val, .. } => {
            let present = path_key(e).map(|k| cx.present.contains(&k)).unwrap_or(false);
            Ty { rt: Some(val.clone()), abs: *safe || !present }
        }
        RTk::Union(arms) => {
            let parts: Vec<Option<RT>> = arms
                .iter()
                .map(|a| match &a.k {
                    RTk::Rec(rec) => find_member(&rec.members.borrow(), name).and_then(|m| m.ty.clone()),
                    _ => None,
                })
                .collect();
            mk_abs(tyv(mk_union(parts)))
        }
        RTk::Quantity(_) if name == "value" || name == "unit" => mk_abs(tyv(Some(prim(if name == "value" { "float" } else { "string" })))),
        _ => mk_abs(unk()),
    }
}

fn func_rt_of(env: &Rc<Env>, f: &FuncEntry) -> RT {
    ty(RTk::Func {
        params: f.params.iter().map(|p| try_resolve(env, p.ty.as_ref()).unwrap_or_else(|| ty(RTk::Any))).collect(),
        ret: try_resolve(env, f.ret.as_ref()).unwrap_or_else(|| ty(RTk::Any)),
    })
}
fn func_rt(cx: &Ctx, name: &str) -> RT {
    let f = cx.env.funcs.borrow().get(name).cloned().unwrap();
    func_rt_of(&cx.env, &f)
}
fn const_ty(cx: &Ctx, name: &str) -> Ty {
    if let Some(t) = cx.const_memo.borrow().get(name) {
        return t.clone();
    }
    cx.const_memo.borrow_mut().insert(name.to_string(), unk()); // cycle guard
    let c = cx.env.consts.borrow().get(name).cloned().unwrap();
    let anno = try_resolve(&cx.env, c.ty.as_ref());
    let t = match anno {
        Some(a) => tyv(Some(a)),
        None => infer(&make_ctx(cx.env.clone(), Rc::new(|_, _| {})), &c.expr), // silent module-scope inference
    };
    cx.const_memo.borrow_mut().insert(name.to_string(), t.clone());
    t
}

fn infer_call(cx: &Ctx, e: &Rc<Expr>) -> Ty {
    let Expr::Call { fun, args } = &**e else { return unk() };
    if let Some(sp) = std_path(fun) {
        let sig = std_sig(&sp);
        if sig.is_none() {
            cx.report("E3003", format!("std.{sp} does not exist (§13.1: names not listed do not exist)"));
        }
        if let Some((arity, _)) = &sig {
            if args.len() != *arity {
                cx.report("E4062", format!("std.{sp} expects {arity} argument(s), got {}", args.len()));
            }
        }
        for a in args {
            if matches!(&**a, Expr::Lambda { .. }) {
                infer(cx, a);
                continue;
            }
            require_val(cx, a, infer(cx, a), "as an argument");
        }
        return tyv(sig.and_then(|(_, r)| r));
    }
    let f = infer(cx, fun);
    let frt = f.rt.filter(|r| matches!(r.k, RTk::Func { .. }));
    let (params, ret): (Vec<RT>, Option<RT>) = match frt.as_ref().map(|r| &r.k) {
        Some(RTk::Func { params, ret }) => (params.clone(), ret_of(ret)),
        _ => (vec![], None),
    };
    if frt.is_some() && args.len() != params.len() {
        cx.report("E4062", format!("call expects {} argument(s), got {}", params.len(), args.len()));
    }
    for (i, a) in args.iter().enumerate() {
        let expected: Option<RT> = if frt.is_some() && i < params.len() && !matches!(params[i].k, RTk::Any) { Some(params[i].clone()) } else { None };
        if let (Expr::Lambda { .. }, Some(ex)) = (&**a, &expected) {
            if matches!(ex.k, RTk::Func { .. }) {
                check_lambda(cx, a, ex);
                continue;
            }
        }
        if matches!(&**a, Expr::Lambda { .. }) {
            infer(cx, a);
            continue;
        }
        let at = require_val(cx, a, infer(cx, a), "as an argument");
        if let (Some(art), Some(ex)) = (&at.rt, &expected) {
            if !subsumes(&cx.env, art, ex) && !deferrable(art, ex) {
                cx.report("E4001", format!("argument {} is not assignable to its parameter", i + 1));
            }
        }
    }
    tyv(if frt.is_some() { ret } else { None })
}

fn check_lambda(cx: &Ctx, e: &Rc<Expr>, expected: &RT) {
    let (Expr::Lambda { params, body }, RTk::Func { params: eps, ret }) = (&**e, &expected.k) else { return };
    if params.len() != eps.len() {
        cx.report("E4062", "lambda arity differs from expected function type".into());
        return;
    }
    let mut c2 = cx.child();
    for (i, p) in params.iter().enumerate() {
        if name_bound(&c2, p) {
            cx.report("E3019", format!("lambda parameter {p} shadows an enclosing name"));
        }
        c2.vars.insert(p.clone(), tyv(if matches!(eps[i].k, RTk::Any) { None } else { Some(eps[i].clone()) }));
    }
    let b = require_val(&c2, body, infer(&c2, body), "as a lambda result");
    if let (Some(brt), Some(r)) = (&b.rt, ret_of(ret)) {
        if !subsumes(&cx.env, brt, &r) && !deferrable(brt, &r) {
            cx.report("E4001", "lambda body is not assignable to the expected result type".into());
        }
    }
}

// ---------------- match (§4.7) ----------------
fn infer_match(cx: &Ctx, e: &Rc<Expr>, expected: Option<&RT>) -> Ty {
    let Expr::Match { subject, arms } = &**e else { return unk() };
    let s = require_val(cx, subject, infer(cx, subject), "as a match subject");
    let mut variants: Option<Vec<RT>> = None;
    if let Some(srt0) = &s.rt {
        let srt = strip_null(srt0);
        if let RTk::Union(vs) = &srt.k {
            let mut v = vs.clone();
            if has_null(Some(srt0)) {
                v.push(ty(RTk::Lit(Value::Null)));
            }
            variants = Some(v);
        } else {
            cx.report("E4103", "`match` subject is not a discriminable union".into());
        }
    }
    let mut covered: HashSet<usize> = HashSet::new();
    let mut catch_alls = 0;
    let mut results: Vec<Option<RT>> = vec![];
    for arm in arms {
        let mut c2 = cx.child();
        if name_bound(&c2, &arm.v) {
            cx.report("E3019", format!("match binding {} shadows an enclosing name", arm.v));
        }
        let mut arm_ty: Option<RT> = None;
        if let Some(t) = &arm.ty {
            arm_ty = try_resolve(&cx.env, Some(t));
            if arm_ty.is_none() {
                cx.report("E3003", "unknown type in match arm".into());
            }
            if let (Some(vs), Some(at)) = (&variants, &arm_ty) {
                for (i, v) in vs.iter().enumerate() {
                    if subsumes(&cx.env, v, at) {
                        if covered.contains(&i) {
                            cx.report("E4100", "match arms overlap on a variant".into());
                        }
                        covered.insert(i);
                    }
                }
            }
        } else {
            catch_alls += 1;
            if let Some(vs) = &variants {
                let rest: Vec<Option<RT>> = vs.iter().enumerate().filter(|(i, _)| !covered.contains(i)).map(|(_, v)| Some(v.clone())).collect();
                if rest.is_empty() {
                    cx.report("E4102", "match catch-all is dead (typed arms are exhaustive)".into());
                }
                arm_ty = mk_union(rest);
            }
        }
        c2.vars.insert(arm.v.clone(), tyv(arm_ty));
        let bt = match expected {
            Some(ex) => check_expr(&c2, &arm.body, Some(ex)),
            None => infer(&c2, &arm.body),
        };
        let b = require_val(&c2, &arm.body, bt, "as a match result");
        results.push(b.rt);
    }
    if catch_alls > 1 {
        cx.report("E4100", "more than one match catch-all arm".into());
    }
    if let Some(vs) = &variants {
        if catch_alls == 0 && covered.len() < vs.len() {
            cx.report("E4101", "`match` is not exhaustive over the subject union".into());
        }
    }
    tyv(mk_union(results))
}

// ---------------- bidirectional checking (§3.18) ----------------
/// a navigation expression in a ref<T> position denotes a place (§7.4):
/// the absence discipline does not apply along the spine — whether the
/// place holds a value is reference integrity (§7.5), checked at binding
fn place_ty(cx: &Ctx, e: &Rc<Expr>) -> Ty {
    match &**e {
        Expr::Paren(x) => place_ty(cx, x),
        Expr::Member { x, .. } => member_core(cx, place_ty(cx, x), e),
        Expr::Index { x, .. } => index_core(cx, place_ty(cx, x), e),
        Expr::If { c, t, f } => {
            // a conditional between places is a place: each branch is read in
            // the ref position, the condition as an ordinary value (§7.4)
            let ct = require_val(cx, c, infer(cx, c), "as a condition");
            if ct.rt.is_some() && !is_boolish(ct.rt.as_ref()) {
                cx.report("E4001", "`if` condition is not bool".into());
            }
            let tt = place_ty(&apply_guards(cx, guards_of(c, true)), t);
            let ft = place_ty(&apply_guards(cx, guards_of(c, false)), f);
            Ty { rt: mk_union(vec![tt.rt, ft.rt]), abs: tt.abs || ft.abs }
        }
        Expr::Name(n) if !cx.vars.contains_key(n) && cx.env.consts.borrow().contains_key(n) => {
            // the spine root must be a root-derived place, never a module const (§7.5, D32)
            cx.report("E4093", format!("`ref` position navigates module const {n} — not a root-derived place (§7.5)"));
            infer(cx, e)
        }
        _ => infer(cx, e),
    }
}

pub fn check_expr(cx: &Ctx, e: &Rc<Expr>, expected: Option<&RT>) -> Ty {
    let Some(expected) = expected else { return infer(cx, e) };
    if let RTk::Ref(_) = &expected.k {
        place_ty(cx, e);
        return tyv(Some(expected.clone())); // place, not value (§7.4)
    }
    if let RTk::Pred { base, .. } = &expected.k {
        return check_expr(cx, e, Some(base));
    }
    if let RTk::IsectN(arms) = &expected.k {
        if matches!(&**e, Expr::Obj(_) | Expr::Arr(_) | Expr::Comp { .. } | Expr::MapComp { .. }) {
            for arm in arms {
                check_expr(cx, e, Some(arm)); // a literal must satisfy every arm
            }
            return tyv(Some(expected.clone()));
        }
    }
    match (&**e, &expected.k) {
        (Expr::Comp { head, clauses }, RTk::Arr { elem, .. }) => {
            let c2 = bind_clauses(cx, clauses);
            check_expr(&c2, head, Some(elem));
            return tyv(Some(expected.clone()));
        }
        (Expr::MapComp { key, val, clauses }, RTk::Map { val: ev, .. }) => {
            let c2 = bind_clauses(cx, clauses);
            let k = require_val(&c2, key, infer(&c2, key), "as a map key");
            if k.rt.is_some() && num_kind(k.rt.as_ref()).as_deref() != Some("string") {
                cx.report("E4001", "map-comprehension key is not a string".into());
            }
            check_expr(&c2, val, Some(ev));
            return tyv(Some(expected.clone()));
        }
        (Expr::Paren(x), _) => return check_expr(cx, x, Some(expected)),
        (Expr::If { c, t, f }, _) => {
            let ct = require_val(cx, c, infer(cx, c), "as a condition");
            if ct.rt.is_some() && !is_boolish(ct.rt.as_ref()) {
                cx.report("E4001", "`if` condition is not bool".into());
            }
            check_expr(&apply_guards(cx, guards_of(c, true)), t, Some(expected));
            check_expr(&apply_guards(cx, guards_of(c, false)), f, Some(expected));
            return tyv(Some(expected.clone()));
        }
        (Expr::Match { .. }, _) => return infer_match(cx, e, Some(expected)),
        (Expr::Obj(entries), RTk::Rec(rec)) => {
            // entries see the record's members (siblings + inherited scope chain)
            let members = rec.members.borrow().clone();
            let mut cx_r = cx.child();
            for m in &members {
                cx_r.vars.insert(m.name.clone(), Ty { rt: member_ty(m), abs: m.kind == MKind::Opt });
            }
            for (k, v) in entries {
                let Some(m) = find_member(&members, k) else {
                    if !rec.open.get() {
                        cx.report("E4003", format!("member {k} is not declared on {}", expected.name.borrow().clone().unwrap_or_else(|| "the record".into())));
                    }
                    require_val(&cx_r, v, infer(&cx_r, v), "as a construction member");
                    continue;
                };
                let mt = member_ty(m);
                let t = check_expr(&cx_r, v, mt.as_ref());
                require_val(&cx_r, v, t, "as a construction member");
            }
            for m in &members {
                if m.kind == MKind::Req && !entries.iter().any(|(k, _)| *k == m.name) {
                    cx.report("E4002", format!("required member {} missing in the construction", m.name));
                }
            }
            return tyv(Some(expected.clone()));
        }
        (Expr::Obj(entries), RTk::Map { val, .. }) => {
            for (_, v) in entries {
                let t = check_expr(cx, v, Some(val));
                require_val(cx, v, t, "as a map value");
            }
            return tyv(Some(expected.clone()));
        }
        (Expr::Obj(_), RTk::Union(_)) => {
            infer(cx, e);
            return tyv(Some(expected.clone())); // discriminated at binding
        }
        (Expr::Obj(_), _) => {
            infer(cx, e);
            cx.report("E4001", format!("object literal where {} is expected", tag(expected)));
            return tyv(Some(expected.clone()));
        }
        (Expr::Arr(items), RTk::Arr { elem, .. }) => {
            for (spread, x) in items {
                if *spread {
                    require_val(cx, x, infer(cx, x), "as a spread");
                    continue;
                }
                let t = check_expr(cx, x, Some(elem));
                require_val(cx, x, t, "as an array element");
            }
            return tyv(Some(expected.clone()));
        }
        (Expr::Lambda { .. }, RTk::Func { .. }) => {
            check_lambda(cx, e, expected);
            return tyv(Some(expected.clone()));
        }
        _ => {}
    }
    let t = require_val(cx, e, infer(cx, e), "as a value");
    if let Some(rt) = &t.rt {
        if !subsumes(&cx.env, rt, expected) && !deferrable(rt, expected) {
            cx.report("E4001", "expression type does not satisfy the expected type".into());
        }
    }
    t
}

/// a same-kind refinement target (pattern, range, literal set) whose
/// membership the static type cannot prove is validated at binding, not
/// rejected here — the corpus (guide, benchmarks) relies on this split;
/// kind-level mismatches still fail statically
fn deferrable(s: &RT, t: &RT) -> bool {
    let Some(k) = num_kind(Some(s)) else { return false };
    match &t.k {
        RTk::Pattern { .. } => k == "string",
        RTk::Range { base, .. } => &k == base,
        RTk::Lit(_) => Some(k) == num_kind(Some(t)),
        RTk::Union(arms) => arms.iter().any(|a| deferrable(s, a)),
        RTk::Pred { base, .. } => deferrable(s, base),
        _ => false,
    }
}
