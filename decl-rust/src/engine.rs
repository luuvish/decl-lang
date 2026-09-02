//! Binding, evaluation, validation, and serialization — a port of the
//! reference implementation's engine.ts. Semantics are the spec's: lazy
//! slots with cycle detection, taint / root-cause diagnostics,
//! $referrers universe ordering, canonical JSON output.
use crate::ast::*;
use crate::semantics::*;
use crate::subsume::subsumes;
use num_bigint::BigInt;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};
use regex::Regex;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering::{self, Equal, Greater, Less};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

pub type Inst = Rc<RefCell<RecInst>>;

pub struct Engine {
    pub env: Rc<Env>,
    pub deferred_slots: RefCell<Vec<(Inst, String)>>,
    no_reg: Cell<u32>,
    phase: Cell<u8>,
}

fn num_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Int(x), Value::Float(y)) => x.to_f64()?.partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&y.to_f64()?),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn exact_float(i: &BigInt) -> Option<f64> {
    let f = i.to_f64()?;
    if !f.is_finite() {
        return None;
    }
    if BigInt::from_f64(f).as_ref() == Some(i) {
        Some(f)
    } else {
        None
    }
}

fn num_s(v: &Value) -> String {
    match v {
        Value::Float(f) => js_num_str(*f),
        Value::Int(i) => i.to_string(),
        Value::Str(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

fn lit_display(v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { "true".into() } else { "false".into() },
        Value::Null => "null".into(),
        other => num_s(other),
    }
}

fn to_index(v: &Value) -> R<i64> {
    match v {
        Value::Int(i) => i.to_i64().ok_or(()).or_else(|_| err("index out of range")),
        Value::Float(f) => Ok(*f as i64),
        _ => err("index must be an integer"),
    }
}

fn path_key_cmp(a: &[Seg], b: &[Seg]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = match (x, y) {
            (Seg::Idx(i), Seg::Idx(j)) => i.cmp(j),
            (Seg::Idx(_), Seg::Name(_)) => Less,
            (Seg::Name(_), Seg::Idx(_)) => Greater,
            (Seg::Name(m), Seg::Name(n)) => m.cmp(n),
        };
        if o != Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

fn arg<'a>(a: &'a [Value], i: usize, name: &str) -> R<&'a Value> {
    a.get(i).ok_or(()).or_else(|_| err(format!("std.{name}: missing argument {i}")))
}

fn add_num(a: &Value, b: &Value) -> R<Value> {
    Ok(match (a, b) {
        (Value::Int(x), Value::Int(y)) => Value::Int(x + y),
        (Value::Float(x), Value::Float(y)) => Value::Float(x + y),
        (Value::Int(x), Value::Float(y)) => Value::Float(x.to_f64().unwrap_or(f64::INFINITY) + y),
        (Value::Float(x), Value::Int(y)) => Value::Float(x + y.to_f64().unwrap_or(f64::INFINITY)),
        _ => return err("bad operands for +"),
    })
}

fn structural_of(v: &Value) -> RT {
    match v {
        Value::Bool(_) => ty(RTk::Prim("bool".into())),
        Value::Int(_) => ty(RTk::Prim("int".into())),
        Value::Float(_) => ty(RTk::Prim("float".into())),
        Value::Str(_) => ty(RTk::Prim("string".into())),
        Value::Null => ty(RTk::Prim("null".into())),
        Value::Ref(_) => ty(RTk::Ref(ty(RTk::Any))),
        Value::Arr(a) => {
            let elem = a.borrow().items.first().map(structural_of).unwrap_or_else(|| ty(RTk::Any));
            ty(RTk::Arr { elem, lo: None, hi: None })
        }
        Value::Q { dim, .. } => ty(RTk::Quantity(dim.clone())),
        _ => ty(RTk::Any),
    }
}

fn fmt_f(n: f64) -> String {
    let s = js_num_str(n);
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

impl Engine {
    /// an engine without registering itself as the environment's evaluator
    pub fn bare(env: Rc<Env>) -> Rc<Engine> {
        Rc::new(Engine { env, deferred_slots: RefCell::new(vec![]), no_reg: Cell::new(0), phase: Cell::new(1) })
    }
    pub fn new(env: Rc<Env>) -> Rc<Engine> {
        let eng = Self::bare(env.clone());
        eng.install_hooks(&env, false);
        eng
    }
    /// §4.13 / unit factors: let the environment evaluate constants through this engine
    pub fn install_hooks(self: &Rc<Self>, env: &Rc<Env>, with_menv: bool) {
        let w = Rc::downgrade(self);
        let we: Weak<Env> = Rc::downgrade(env);
        *env.const_eval.borrow_mut() = Some(Rc::new(move |n: &str| match (w.upgrade(), we.upgrade()) {
            (Some(eng), Some(e)) => eng.force_const_in(&e, n, ""),
            _ => Ok(Value::Null),
        }));
        let w2 = Rc::downgrade(self);
        let we2: Weak<Env> = Rc::downgrade(env);
        *env.expr_eval.borrow_mut() = Some(Rc::new(move |x: &Rc<Expr>| match w2.upgrade() {
            Some(eng) => eng.ev(x, &Scope::new("", if with_menv { we2.upgrade() } else { None })),
            None => Ok(Value::Null),
        }));
    }

    // ---------- expression evaluation ----------
    pub fn ev(&self, e: &Rc<Expr>, sc: &Scope) -> R<Value> {
        match &**e {
            Expr::Lit(v) => Ok(v.clone()),
            Expr::Pattern(s) => Ok(Value::Pat(s.clone())),
            Expr::UnitLit { num, unit } => {
                let (key, to_base) = self.env.unit_info(unit).or_else(err)?;
                Ok(Value::Q { dim: key, value: num * to_base })
            }
            Expr::Paren(x) => self.ev(x, sc),
            Expr::MapComp { key, val, clauses } => {
                let mut entries = vec![];
                self.map_comp(key, val, clauses, 0, (*sc.locals).clone(), sc, &mut entries)?;
                Ok(Value::PreObj(Rc::new(entries)))
            }
            Expr::Template(parts) => Ok(Value::Str(self.render(parts, sc)?)),
            Expr::Name(name) => {
                if let Some(v) = sc.locals.get(name) {
                    return Ok(v.clone());
                }
                if let Some(inst) = &sc.inst {
                    if let Some(v) = self.slot_lookup(inst, name)? {
                        return Ok(v);
                    }
                }
                let menv = sc.menv.clone().unwrap_or_else(|| self.env.clone());
                if let Some(v) = self.module_value(&menv, name, &sc.root_name)? {
                    return Ok(v);
                }
                if name == "std" {
                    return Ok(Value::Std(Rc::new(vec![])));
                }
                if let Some(v) = self.env.root(name) {
                    return Ok(v);
                }
                err(format!("unknown name {name}"))
            }
            Expr::Ctx(n) => match n.as_str() {
                "$this" => Ok(sc.inst.clone().map(Value::Rec).unwrap_or(Value::Null)),
                "$parent" => Ok(sc.inst.as_ref().and_then(|i| i.borrow().parent.clone()).map(Value::Rec).unwrap_or(Value::Null)),
                "$path" => match &sc.inst {
                    Some(i) => Ok(Value::Str(path_str(&i.borrow().path, None))),
                    None => err("$path outside a record"),
                },
                _ => err(format!("unsupported context var {n}")),
            },
            Expr::Referrers { ty, member } => self.referrers(ty, member, sc),
            Expr::Obj(entries) => Ok(Value::PreObj(Rc::new(
                entries.iter().map(|(k, v)| (k.clone(), Value::PreVal(Rc::new(PreValV { expr: v.clone(), scope: sc.clone() })))).collect(),
            ))),
            Expr::Arr(items) => Ok(Value::PreArr(Rc::new(
                items.iter().map(|(sp, v)| (*sp, Value::PreVal(Rc::new(PreValV { expr: v.clone(), scope: sc.clone() })))).collect(),
            ))),
            Expr::Comp { head, clauses } => {
                let mut items = vec![];
                self.comp(head, clauses, 0, (*sc.locals).clone(), sc, &mut items)?;
                Ok(Value::PreArr(Rc::new(items)))
            }
            Expr::If { c, t, f } => {
                if self.truthy(&self.ev(c, sc)?)? {
                    self.ev(t, sc)
                } else {
                    self.ev(f, sc)
                }
            }
            Expr::Match { subject, arms } => {
                let subj = self.deref(self.ev(subject, sc)?)?;
                let run = |arm: &MatchArm| {
                    let mut l2 = (*sc.locals).clone();
                    l2.insert(arm.v.clone(), subj.clone());
                    self.ev(&arm.body, &sc.with_locals(l2))
                };
                let mut catch_all = None;
                for arm in arms {
                    let Some(t) = &arm.ty else {
                        catch_all = Some(arm);
                        continue;
                    };
                    let menv = sc.menv.clone().unwrap_or_else(|| self.env.clone());
                    let rt = menv.resolve(t, None).or_else(err)?;
                    if self.member_of(&subj, &rt, sc) {
                        return run(arm);
                    }
                }
                if let Some(arm) = catch_all {
                    return run(arm);
                }
                err("match: no arm matched")
            }
            Expr::Lambda { params, body } => Ok(Value::Clo(Rc::new(Closure { params: params.clone(), body: body.clone(), scope: sc.clone() }))),
            Expr::Un { op, x } => {
                let x = self.ev(x, sc)?;
                match op.as_str() {
                    "!" => Ok(Value::Bool(!self.truthy(&x)?)),
                    "-" => match x {
                        Value::Absent => err("absent consumed"),
                        Value::Q { dim, value } => Ok(Value::Q { dim, value: -value }),
                        Value::Int(i) => Ok(Value::Int(-i)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        _ => err("bad operand for unary -"),
                    },
                    "~" => match x {
                        Value::Int(i) => Ok(Value::Int(-i - 1)),
                        _ => err("bad operand for ~"),
                    },
                    _ => err("un"),
                }
            }
            Expr::Bin { op, l, r } => {
                if op == "|>" {
                    let call = match &**r {
                        Expr::Call { fun, args } => {
                            let mut a = vec![l.clone()];
                            a.extend(args.iter().cloned());
                            Expr::Call { fun: fun.clone(), args: a }
                        }
                        _ => Expr::Call { fun: r.clone(), args: vec![l.clone()] },
                    };
                    return self.ev(&Rc::new(call), sc);
                }
                self.binop(op, l, r, sc)
            }
            Expr::Member { x, name, safe } => {
                let x0 = self.ev(x, sc)?;
                if let Value::NsRef(ns) = &x0 {
                    return self.ns_value(ns, name, sc);
                }
                if *safe && matches!(x0, Value::Null | Value::Absent) {
                    return Ok(Value::Absent);
                }
                let d = self.deref(x0)?;
                self.access(&d, name)
            }
            Expr::Index { x, i } => {
                let x = self.deref(self.ev(x, sc)?)?;
                let i = self.ev(i, sc)?;
                match &x {
                    Value::Arr(a) => {
                        let n = to_index(&i)?;
                        let a = a.borrow();
                        if n < 0 || n as usize >= a.items.len() {
                            return err_code(format!("index {n} out of bounds"), "E5005");
                        }
                        Ok(a.items[n as usize].clone())
                    }
                    Value::Map(m) => match &i {
                        Value::Str(k) => Ok(m.borrow().get(k).cloned().unwrap_or(Value::Absent)),
                        _ => Ok(Value::Absent),
                    },
                    Value::Rec(_) => match &i {
                        Value::Str(k) => self.access(&x, k),
                        _ => err("index on record needs a string"),
                    },
                    _ => err("index on non-collection"),
                }
            }
            Expr::Call { fun, args } => {
                let mut a = Vec::with_capacity(args.len());
                for x in args {
                    a.push(self.ev(x, sc)?);
                }
                let f = self.ev_callee(fun, sc)?;
                self.call(&f, a, sc)
            }
            Expr::With { base, patch } => self.with_expr(base, patch, sc),
        }
    }

    fn render(&self, parts: &[TPart], sc: &Scope) -> R<String> {
        let mut s = String::new();
        for p in parts {
            match p {
                TPart::Text(t) => s.push_str(t),
                TPart::Expr(x) => s.push_str(&self.to_str(&self.ev(x, sc)?)?),
            }
        }
        Ok(s)
    }

    fn comp(&self, head: &Rc<Expr>, clauses: &[ForClause], ci: usize, locals: HashMap<String, Value>, sc: &Scope, out: &mut Vec<(bool, Value)>) -> R<()> {
        if ci == clauses.len() {
            out.push((false, Value::PreVal(Rc::new(PreValV { expr: head.clone(), scope: sc.with_locals(locals) }))));
            return Ok(());
        }
        let cl = &clauses[ci];
        let it = self.ev(&cl.iter, &sc.with_locals(locals.clone()))?;
        for el in self.iterate(&it)? {
            let mut l2 = locals.clone();
            l2.insert(cl.v.clone(), el);
            let sc2 = sc.with_locals(l2.clone());
            let mut ok = true;
            for f in &cl.filters {
                if !self.truthy(&self.ev(f, &sc2)?)? {
                    ok = false;
                    break;
                }
            }
            if ok {
                self.comp(head, clauses, ci + 1, l2, sc, out)?;
            }
        }
        Ok(())
    }

    fn map_comp(&self, key: &Rc<Expr>, val: &Rc<Expr>, clauses: &[ForClause], ci: usize, locals: HashMap<String, Value>, sc: &Scope, out: &mut Vec<(String, Value)>) -> R<()> {
        if ci == clauses.len() {
            let sc2 = sc.with_locals(locals);
            let Value::Str(k) = self.ev(key, &sc2)? else { return err("map key must be string") };
            if out.iter().any(|(kk, _)| *kk == k) {
                return err_code(format!("duplicate key {k}"), "E5004");
            }
            let v = self.ev(val, &sc2)?;
            out.push((k, v));
            return Ok(());
        }
        let cl = &clauses[ci];
        let it = self.ev(&cl.iter, &sc.with_locals(locals.clone()))?;
        for el in self.iterate(&it)? {
            let mut l2 = locals.clone();
            l2.insert(cl.v.clone(), el);
            let sc2 = sc.with_locals(l2.clone());
            let mut ok = true;
            for f in &cl.filters {
                if !self.truthy(&self.ev(f, &sc2)?)? {
                    ok = false;
                    break;
                }
            }
            if ok {
                self.map_comp(key, val, clauses, ci + 1, l2, sc, out)?;
            }
        }
        Ok(())
    }

    pub fn member_of(&self, v: &Value, rt: &RT, sc: &Scope) -> bool {
        if let Value::Rec(r) = v {
            let vrt = r.borrow().rt.clone();
            return subsumes(&self.env, &vrt, rt);
        }
        let mark = self.env.diag_len();
        self.no_reg.set(self.no_reg.get() + 1);
        let ok = self.bind(v.clone(), rt, &[Seg::Name("<match>".into())], None, sc).is_ok();
        self.no_reg.set(self.no_reg.get() - 1);
        self.env.diag_truncate(mark);
        ok
    }

    fn ev_callee(&self, e: &Rc<Expr>, sc: &Scope) -> R<Value> {
        if let Expr::Member { x, name, .. } = &**e {
            let x = self.ev_callee(x, sc)?;
            return match &x {
                Value::Std(p) => {
                    let mut p2 = (**p).clone();
                    p2.push(name.clone());
                    Ok(Value::Std(Rc::new(p2)))
                }
                Value::NsRef(ns) => self.ns_value(ns, name, sc),
                _ => {
                    let d = self.deref(x)?;
                    self.access(&d, name)
                }
            };
        }
        self.ev(e, sc)
    }

    fn module_value(&self, menv: &Rc<Env>, name: &str, root_name: &str) -> R<Option<Value>> {
        if menv.consts.borrow().contains_key(name) {
            return Ok(Some(self.force_const_in(menv, name, root_name)?));
        }
        let f = menv.funcs.borrow().get(name).cloned();
        if let Some(f) = f {
            return Ok(Some(Value::Clo(Rc::new(Closure {
                params: f.params.iter().map(|p| p.name.clone()).collect(),
                body: f.body.clone(),
                scope: Scope::new(root_name, Some(menv.clone())),
            }))));
        }
        let im = menv.imports.borrow().get(name).cloned();
        if let Some(im) = im {
            if let Some(v) = self.module_value(&im.env, &im.name, root_name)? {
                return Ok(Some(v));
            }
            return Ok(self.env.root(&im.name));
        }
        let ns = menv.namespaces.borrow().get(name).cloned();
        if let Some((_, exports)) = ns {
            return Ok(Some(Value::NsRef(Rc::new(NsRefV { exports }))));
        }
        Ok(None)
    }

    fn ns_value(&self, ns: &NsRefV, name: &str, sc: &Scope) -> R<Value> {
        let ex = ns.exports.borrow().get(name).cloned();
        let Some(ex) = ex else { return err(format!("namespace has no export {name}")) };
        if let Some(v) = self.module_value(&ex.env, &ex.name, &sc.root_name)? {
            return Ok(v);
        }
        if let Some(v) = self.env.root(&ex.name) {
            return Ok(v);
        }
        err(format!("{name} is not a value"))
    }

    fn q_arith(&self, op: &str, l: &Value, r: &Value) -> R<Value> {
        let dim_or_1 = |d: &str| if d.is_empty() { "1".to_string() } else { d.to_string() };
        match op {
            "+" | "-" => {
                let (Value::Q { dim: ld, value: lv }, Value::Q { dim: rd, value: rv }) = (l, r) else {
                    return err(format!("`{op}` mixes quantity and plain number"));
                };
                if ld != rd {
                    return err(format!("quantity dimension mismatch: {} vs {}", dim_or_1(ld), dim_or_1(rd)));
                }
                Ok(Value::Q { dim: ld.clone(), value: if op == "+" { lv + rv } else { lv - rv } })
            }
            "<" | "<=" | ">" | ">=" => {
                let (Value::Q { dim: ld, value: lv }, Value::Q { dim: rd, value: rv }) = (l, r) else {
                    return err("quantity dimension mismatch in comparison");
                };
                if ld != rd {
                    return err("quantity dimension mismatch in comparison");
                }
                Ok(Value::Bool(match op {
                    "<" => lv < rv,
                    "<=" => lv <= rv,
                    ">" => lv > rv,
                    _ => lv >= rv,
                }))
            }
            _ => {
                let mag = |v: &Value| -> Option<f64> {
                    match v {
                        Value::Q { value, .. } => Some(*value),
                        Value::Int(i) => Some(i.to_f64().unwrap_or(f64::INFINITY)),
                        Value::Float(f) => Some(*f),
                        _ => None,
                    }
                };
                let (Some(lm), Some(rm)) = (mag(l), mag(r)) else { return err(format!("bad operands for {op}")) };
                if op == "/" && rm == 0.0 {
                    return err_code("division by zero", "E5001");
                }
                let dv = |v: &Value| match v {
                    Value::Q { dim, .. } => vec_of_key(dim),
                    _ => DimVec::new(),
                };
                let vec = vec_combine(&dv(l), &dv(r), if op == "*" { 1 } else { -1 });
                let value = if op == "*" { lm * rm } else { lm / rm };
                if !value.is_finite() {
                    return err_code("non-finite", "E5002");
                }
                let key = key_of_vec(&vec);
                Ok(if key.is_empty() { Value::Float(value) } else { Value::Q { dim: key, value } })
            }
        }
    }

    fn iterate(&self, v: &Value) -> R<Vec<Value>> {
        match v {
            Value::PreArr(_) | Value::PreObj(_) => self.mat_arr(v),
            Value::Arr(a) => Ok(a.borrow().items.clone()),
            Value::Range { lo, hi, excl } => {
                let (Value::Int(lo), Value::Int(hi)) = (&**lo, &**hi) else { return err("range bounds must be integers") };
                let hi = if *excl { hi.clone() } else { hi + 1 };
                let mut out = vec![];
                let mut i = lo.clone();
                while i < hi {
                    out.push(Value::Int(i.clone()));
                    i += 1;
                }
                Ok(out)
            }
            _ => err("not iterable"),
        }
    }

    fn truthy(&self, v: &Value) -> R<bool> {
        match v {
            Value::Bool(b) => Ok(*b),
            _ => err("non-bool condition"),
        }
    }

    fn binop(&self, op: &str, le: &Rc<Expr>, re: &Rc<Expr>, sc: &Scope) -> R<Value> {
        match op {
            "&&" => {
                return if self.truthy(&self.ev(le, sc)?)? { Ok(Value::Bool(self.truthy(&self.ev(re, sc)?)?)) } else { Ok(Value::Bool(false)) };
            }
            "||" => {
                return if self.truthy(&self.ev(le, sc)?)? { Ok(Value::Bool(true)) } else { Ok(Value::Bool(self.truthy(&self.ev(re, sc)?)?)) };
            }
            "??" => {
                let l = self.ev(le, sc)?;
                return if matches!(l, Value::Absent | Value::Null) { self.ev(re, sc) } else { Ok(l) };
            }
            _ => {}
        }
        let l = self.ev(le, sc)?;
        let r = self.ev(re, sc)?;
        match op {
            ".." | "..<" => return Ok(Value::Range { lo: Box::new(l), hi: Box::new(r), excl: op == "..<" }),
            "matches" => {
                let (Value::Str(s), Value::Pat(p)) = (&l, &r) else { return err("matches needs a string and a pattern") };
                let re = Regex::new(&format!("^(?:{p})$")).or_else(|e| err(e.to_string()))?;
                return Ok(Value::Bool(re.is_match(s)));
            }
            "==" => return Ok(Value::Bool(value_eq(&l, &r))),
            "!=" => return Ok(Value::Bool(!value_eq(&l, &r))),
            "in" => {
                return match &r {
                    Value::Range { lo, hi, excl } => {
                        let ge = matches!(num_cmp(&l, lo), Some(Greater | Equal));
                        let hi_ok = if *excl { num_cmp(&l, hi) == Some(Less) } else { matches!(num_cmp(&l, hi), Some(Less | Equal)) };
                        Ok(Value::Bool(ge && hi_ok))
                    }
                    Value::PreArr(_) | Value::PreObj(_) => Ok(Value::Bool(self.mat_arr(&r)?.iter().any(|x| value_eq(&l, x)))),
                    Value::Arr(a) => Ok(Value::Bool(a.borrow().items.iter().any(|x| value_eq(&l, x)))),
                    Value::Map(m) => {
                        let Value::Str(k) = &l else { return Ok(Value::Bool(false)) };
                        Ok(Value::Bool(m.borrow().has(k)))
                    }
                    Value::Rec(rec) => {
                        let Value::Str(k) = &l else { return Ok(Value::Bool(false)) };
                        let has = rec.borrow().has_slot(k);
                        Ok(Value::Bool(has && self.force_state(rec, k) != SlotState::Absent))
                    }
                    _ => err("in: bad container"),
                };
            }
            _ => {}
        }
        if l.is_absent() || r.is_absent() {
            return err("absent consumed");
        }
        if (matches!(l, Value::Q { .. }) || matches!(r, Value::Q { .. })) && ["+", "-", "*", "/", "<", "<=", ">", ">="].contains(&op) {
            return self.q_arith(op, &l, &r);
        }
        let both_i = matches!((&l, &r), (Value::Int(_), Value::Int(_)));
        let both_f = matches!((&l, &r), (Value::Float(_), Value::Float(_)));
        let both_s = matches!((&l, &r), (Value::Str(_), Value::Str(_)));
        match (op, &l, &r) {
            ("+", Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}"))),
            ("+", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
            ("+", Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            ("-", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
            ("-", Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            ("*", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
            ("*", Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            ("/", Value::Int(a), Value::Int(b)) => {
                if b.is_zero() {
                    return err_code("division by zero", "E5001");
                }
                Ok(Value::Int(a / b))
            }
            ("/", Value::Float(a), Value::Float(b)) => {
                if *b == 0.0 {
                    return err_code("division by zero", "E5001");
                }
                let q = a / b;
                if !q.is_finite() {
                    return err_code("non-finite", "E5002");
                }
                Ok(Value::Float(q))
            }
            ("%", Value::Int(a), Value::Int(b)) => {
                if b.is_zero() {
                    return err_code("mod zero", "E5001");
                }
                Ok(Value::Int(a % b))
            }
            ("<" | "<=" | ">" | ">=", _, _) if both_i || both_f || both_s => {
                let o = num_cmp(&l, &r);
                Ok(Value::Bool(matches!((op, o), ("<", Some(Less)) | ("<=", Some(Less | Equal)) | (">", Some(Greater)) | (">=", Some(Greater | Equal)))))
            }
            ("&", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.clone() & b.clone())),
            ("|", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.clone() | b.clone())),
            ("^", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.clone() ^ b.clone())),
            ("<<" | ">>", Value::Int(a), Value::Int(b)) => {
                if b.is_negative() {
                    return err_code("negative shift count", "E5003");
                }
                let n = b.to_usize().ok_or(()).or_else(|_| err("shift count too large"))?;
                Ok(Value::Int(if op == "<<" { a.clone() << n } else { a.clone() >> n }))
            }
            _ => err(format!("bad operands for {op}")),
        }
    }

    fn to_str(&self, v: &Value) -> R<String> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            Value::Bool(b) => Ok(if *b { "true".into() } else { "false".into() }),
            Value::Int(i) => Ok(i.to_string()),
            Value::Float(f) => Ok(js_num_str(*f)),
            _ => err("template: non-convertible"),
        }
    }

    fn deref(&self, v: Value) -> R<Value> {
        if let Value::Ref(p) = &v {
            let target = self.resolve_segs(p)?;
            if target.is_undef() {
                return err(format!("dangling reference {}", path_str(p, None)));
            }
            return Ok(target);
        }
        Ok(v)
    }

    pub fn resolve_segs(&self, segs: &[Seg]) -> R<Value> {
        let mut cur = match segs.first() {
            Some(Seg::Name(n)) => self.env.root(n).unwrap_or(Value::Undef),
            _ => Value::Undef,
        };
        for s in segs.iter().skip(1) {
            if cur.is_undef() {
                break;
            }
            cur = match (&cur, s) {
                (Value::Rec(r), Seg::Name(n)) => self.force_slot(r, n)?,
                (Value::Rec(_), Seg::Idx(i)) => return err(format!("no member {i}")),
                (Value::Arr(a), Seg::Idx(i)) => a.borrow().items.get(*i).cloned().unwrap_or(Value::Undef),
                (Value::Map(m), Seg::Name(n)) => m.borrow().get(n).cloned().unwrap_or(Value::Undef),
                _ => Value::Undef,
            };
            if let Value::Ref(_) = cur {
                cur = self.deref(cur)?;
            }
        }
        Ok(cur)
    }

    fn access(&self, x: &Value, name: &str) -> R<Value> {
        match x {
            Value::Rec(r) => {
                let (has, is_extra) = {
                    let b = r.borrow();
                    (b.has_slot(name), b.extra(name).is_some())
                };
                if has {
                    if self.force_state(r, name) == SlotState::Absent {
                        return Ok(Value::Absent);
                    }
                    return self.force_slot(r, name);
                }
                if is_extra {
                    return err(format!("opaque field {name} accessed"));
                }
                err(format!("no member {name}"))
            }
            Value::PreObj(es) => {
                for (k, v) in es.iter() {
                    if k == name {
                        return match v {
                            Value::PreVal(pv) => self.ev(&pv.expr, &pv.scope),
                            other => Ok(other.clone()),
                        };
                    }
                }
                Ok(Value::Absent)
            }
            Value::Null => err("member access on null"),
            Value::Absent => Ok(Value::Absent),
            _ => err(format!("member access on non-record ({name})")),
        }
    }

    fn slot_lookup(&self, inst: &Inst, name: &str) -> R<Option<Value>> {
        let mut cur = Some(inst.clone());
        while let Some(c) = cur {
            if c.borrow().has_slot(name) {
                if self.force_state(&c, name) == SlotState::Absent {
                    return Ok(Some(Value::Absent));
                }
                return Ok(Some(self.force_slot(&c, name)?));
            }
            cur = c.borrow().parent.clone();
        }
        Ok(None)
    }

    pub fn call(&self, f: &Value, args: Vec<Value>, sc: &Scope) -> R<Value> {
        match f {
            Value::Clo(c) => {
                let mut locals = (*c.scope.locals).clone();
                for (p, a) in c.params.iter().zip(args) {
                    locals.insert(p.clone(), a);
                }
                self.ev(&c.body, &c.scope.with_locals(locals))
            }
            Value::Nat(f) => f(&args),
            Value::Std(p) => self.std(&p.join("."), args, sc),
            _ => err("call of non-function"),
        }
    }

    fn std(&self, name: &str, a: Vec<Value>, sc: &Scope) -> R<Value> {
        let domain = |msg: String| -> Fail { Fail::Eval(EvalErr { msg: format!("std.{name}: {msg}"), code: Some("E5008".into()) }) };
        let arr = |path: Vec<Value>| Value::Arr(Rc::new(RefCell::new(ArrV { items: path, path: vec![] })));
        let s = |v: &Value| -> R<String> {
            match v {
                Value::Str(s) => Ok(s.clone()),
                _ => err(format!("std.{name}: expected string")),
            }
        };
        match name {
            "array.count" => Ok(Value::Int(BigInt::from(self.mat_arr(arg(&a, 0, name)?)?.len()))),
            "array.all" => {
                for x in self.mat_arr(arg(&a, 0, name)?)? {
                    if !self.truthy(&self.call(arg(&a, 1, name)?, vec![x], sc)?)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            "array.any" => {
                for x in self.mat_arr(arg(&a, 0, name)?)? {
                    if self.truthy(&self.call(arg(&a, 1, name)?, vec![x], sc)?)? {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            "array.filter" => {
                let mut out = vec![];
                for x in self.mat_arr(arg(&a, 0, name)?)? {
                    if self.truthy(&self.call(arg(&a, 1, name)?, vec![x.clone()], sc)?)? {
                        out.push(x);
                    }
                }
                Ok(arr(out))
            }
            "array.all_distinct" => {
                let items = self.mat_arr(arg(&a, 0, name)?)?;
                for i in 0..items.len() {
                    for j in i + 1..items.len() {
                        if value_eq(&items[i], &items[j]) {
                            return Ok(Value::Bool(false));
                        }
                    }
                }
                Ok(Value::Bool(true))
            }
            "array.sum" => {
                let items = self.mat_arr(arg(&a, 0, name)?)?;
                let Some(first) = items.first() else { return Ok(Value::Int(BigInt::zero())) };
                let mut acc = if matches!(first, Value::Float(_)) { Value::Float(0.0) } else { Value::Int(BigInt::zero()) };
                for x in &items {
                    acc = add_num(&acc, x)?;
                }
                Ok(acc)
            }
            "array.fold" => {
                let mut acc = arg(&a, 1, name)?.clone();
                for x in self.mat_arr(arg(&a, 0, name)?)? {
                    acc = self.call(arg(&a, 2, name)?, vec![acc, x], sc)?;
                }
                Ok(acc)
            }
            "map.keys" => Ok(arr(self.mat_map(arg(&a, 0, name)?)?.borrow().entries.iter().map(|(k, _)| Value::Str(k.clone())).collect())),
            "map.values" => Ok(arr(self.mat_map(arg(&a, 0, name)?)?.borrow().entries.iter().map(|(_, v)| v.clone()).collect())),
            "map.entries" => Ok(arr(
                self.mat_map(arg(&a, 0, name)?)?
                    .borrow()
                    .entries
                    .iter()
                    .map(|(k, v)| Value::PreObj(Rc::new(vec![("key".into(), Value::Str(k.clone())), ("value".into(), v.clone())])))
                    .collect(),
            )),
            "string.length" => Ok(Value::Int(BigInt::from(s(arg(&a, 0, name)?)?.chars().count()))),
            "string.of" => Ok(Value::Str(self.to_str(arg(&a, 0, name)?)?)),
            "string.join" => {
                let sep = s(arg(&a, 1, name)?)?;
                let parts: Vec<String> = self.mat_arr(arg(&a, 0, name)?)?.iter().map(s).collect::<R<_>>()?;
                Ok(Value::Str(parts.join(&sep)))
            }
            "string.starts_with" => Ok(Value::Bool(s(arg(&a, 0, name)?)?.starts_with(&s(arg(&a, 1, name)?)?))),
            "string.ends_with" => Ok(Value::Bool(s(arg(&a, 0, name)?)?.ends_with(&s(arg(&a, 1, name)?)?))),
            "string.contains" => Ok(Value::Bool(s(arg(&a, 0, name)?)?.contains(&s(arg(&a, 1, name)?)?))),
            "string.split" => {
                let sep = s(arg(&a, 1, name)?)?;
                if sep.is_empty() {
                    return Err(domain("separator must be non-empty".into()));
                }
                Ok(arr(s(arg(&a, 0, name)?)?.split(&sep).map(|p| Value::Str(p.to_string())).collect()))
            }
            "ref.path" => match arg(&a, 0, name)? {
                Value::Ref(p) => Ok(Value::Str(path_str(p, None))),
                _ => err("ref.path on non-reference"),
            },
            "math.abs" => match arg(&a, 0, name)? {
                Value::Int(i) => Ok(Value::Int(i.abs())),
                Value::Float(f) => Ok(Value::Float(f.abs())),
                _ => err("std.math.abs: bad operand"),
            },
            "math.min" | "math.max" => {
                let (x, y) = (arg(&a, 0, name)?, arg(&a, 1, name)?);
                let Some(o) = num_cmp(x, y) else { return err(format!("std.{name}: bad operands")) };
                let pick_x = if name == "math.min" { o == Less } else { o == Greater };
                Ok(if pick_x { x.clone() } else { y.clone() })
            }
            "math.clog2" => {
                let n = arg(&a, 0, name)?;
                let Value::Int(i) = n else { return Err(domain(format!("n >= 1 required, got {}", num_s(n)))) };
                if i < &BigInt::from(1) {
                    return Err(domain(format!("n >= 1 required, got {i}")));
                }
                Ok(Value::Int(BigInt::from((i - BigInt::from(1)).bits())))
            }
            "math.floor" | "math.ceil" => match arg(&a, 0, name)? {
                Value::Float(f) => {
                    let r = if name == "math.floor" { f.floor() } else { f.ceil() };
                    BigInt::from_f64(r).map(Value::Int).ok_or(()).or_else(|_| err(format!("std.{name}: non-finite")))
                }
                Value::Int(i) => Ok(Value::Int(i.clone())),
                _ => err(format!("std.{name}: bad operand")),
            },
            "math.round" => match arg(&a, 0, name)? {
                Value::Float(x) => {
                    let f = x.floor();
                    let frac = x - f;
                    let r = if frac > 0.5 {
                        f + 1.0
                    } else if frac < 0.5 {
                        f
                    } else if f % 2.0 == 0.0 {
                        f
                    } else {
                        f + 1.0
                    };
                    BigInt::from_f64(r).map(Value::Int).ok_or(()).or_else(|_| err("std.math.round: non-finite"))
                }
                Value::Int(i) => Ok(Value::Int(i.clone())),
                _ => err("std.math.round: bad operand"),
            },
            "int.of" => match arg(&a, 0, name)? {
                Value::Float(x) if x.fract() == 0.0 && x.is_finite() => Ok(Value::Int(BigInt::from_f64(*x).unwrap())),
                other => Err(domain(format!("no fractional part allowed, got {}", num_s(other)))),
            },
            "int.at_least" | "int.at_most" => {
                let n = arg(&a, 0, name)?.clone();
                let least = name == "int.at_least";
                let f: NatFn = Rc::new(move |args: &[Value]| {
                    let Some(x) = args.first() else { return err("missing argument") };
                    match num_cmp(x, &n) {
                        Some(o) => Ok(Value::Bool(if least { o != Less } else { o != Greater })),
                        None => err("bad operands"),
                    }
                });
                Ok(Value::Nat(f))
            }
            "float.of" => {
                let v = match arg(&a, 0, name)? {
                    Value::Int(i) => i.to_f64().unwrap_or(f64::INFINITY),
                    Value::Float(f) => *f,
                    Value::Str(s) => s.parse::<f64>().or_else(|_| err("std.float.of: bad operand"))?,
                    _ => return err("std.float.of: bad operand"),
                };
                if !v.is_finite() {
                    return Err(domain("magnitude outside binary64 range".into()));
                }
                Ok(Value::Float(v))
            }
            "object.merge" => self.deep_merge(arg(&a, 0, name)?, arg(&a, 1, name)?),
            _ => err(format!("std.{name} does not exist")),
        }
    }

    fn deep_merge(&self, base0: &Value, patch0: &Value) -> R<Value> {
        let base = self.mat_rec(base0)?;
        let patch = self.mat_rec(patch0)?;
        let val = |r: &Inst, n: &str| -> R<Option<Value>> {
            {
                let b = r.borrow();
                if let Some(v) = b.extra(n) {
                    return Ok(Some(v.clone()));
                }
                match b.slot(n) {
                    None => return Ok(None),
                    Some(s) if s.kind == MKind::Der => return Ok(None),
                    _ => {}
                }
            }
            if self.force_state(r, n) == SlotState::Absent {
                return Ok(None);
            }
            Ok(Some(self.force_slot(r, n)?))
        };
        let mut names: Vec<String> = vec![];
        let mut push = |n: &str| {
            if !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        };
        for r in [&base, &patch] {
            let b = r.borrow();
            for n in &b.entry_order {
                push(n);
            }
            for m in rec_members(&b.rt) {
                if m.kind == MKind::Dflt {
                    push(&m.name);
                }
            }
        }
        let mut entries = vec![];
        for n in &names {
            let is_der = |r: &Inst| r.borrow().slot(n).map(|s| s.kind == MKind::Der).unwrap_or(false);
            if is_der(&base) || is_der(&patch) {
                continue;
            }
            let bv = val(&base, n)?;
            let pv = val(&patch, n)?;
            match (bv, pv) {
                (Some(bv), Some(pv)) => {
                    let bd = self.deref(bv.clone())?;
                    let pd = self.deref(pv.clone())?;
                    if matches!(bd, Value::Rec(_)) && matches!(pd, Value::Rec(_)) {
                        entries.push((n.clone(), self.deep_merge(&bv, &pv)?));
                    } else {
                        entries.push((n.clone(), pv));
                    }
                }
                (_, Some(pv)) => entries.push((n.clone(), pv)),
                (Some(bv), None) => entries.push((n.clone(), bv)),
                (None, None) => {}
            }
        }
        Ok(Value::PreObj(Rc::new(entries)))
    }

    fn mat_rec(&self, v: &Value) -> R<Inst> {
        let mut d = self.deref(v.clone())?;
        if matches!(d, Value::PreObj(_) | Value::PreArr(_) | Value::JObj(_)) {
            d = self.materialize(d, &[])?;
        }
        match d {
            Value::Rec(r) => Ok(r),
            _ => err_code("std.object.merge: expected records", "E5008"),
        }
    }
    fn mat_arr(&self, v: &Value) -> R<Vec<Value>> {
        let mut d = self.deref(v.clone())?;
        if matches!(d, Value::PreObj(_) | Value::PreArr(_)) {
            d = self.materialize(d, &[])?;
        }
        match d {
            Value::Arr(a) => Ok(a.borrow().items.clone()),
            _ => err("expected array"),
        }
    }
    fn mat_map(&self, v: &Value) -> R<Rc<RefCell<MapV>>> {
        let mut d = self.deref(v.clone())?;
        if matches!(d, Value::PreObj(_) | Value::PreArr(_)) {
            d = self.materialize(d, &[])?;
        }
        match d {
            Value::Map(m) => Ok(m),
            _ => err("expected map"),
        }
    }

    // ---------- referrers ----------
    fn referrers(&self, type_name: &str, member: &str, sc: &Scope) -> R<Value> {
        if self.phase.get() < 2 {
            return Err(Fail::Defer);
        }
        let Some(self_inst) = &sc.inst else { return err("$referrers outside a record") };
        let target = self_inst.borrow().path.clone();
        let mut out: Vec<Inst> = vec![];
        for cand in self.env.registry_snapshot() {
            let (tn, has) = {
                let b = cand.borrow();
                (b.type_name.clone(), b.has_slot(member))
            };
            if tn.as_deref() != Some(type_name) || !has {
                continue;
            }
            let Ok(v) = self.force_slot(&cand, member) else { continue };
            if self.contains_ref_to(&v, &target) {
                out.push(cand);
            }
        }
        out.sort_by(|a, b| path_key_cmp(&a.borrow().path, &b.borrow().path));
        let items = out.iter().map(|c| Value::Ref(Rc::new(c.borrow().path.clone()))).collect();
        Ok(Value::Arr(Rc::new(RefCell::new(ArrV { items, path: vec![] }))))
    }

    fn contains_ref_to(&self, v: &Value, target: &[Seg]) -> bool {
        match v {
            Value::Ref(p) => cmp_path(p, target) == Equal,
            Value::Arr(a) => a.borrow().items.iter().any(|x| self.contains_ref_to(x, target)),
            Value::Map(m) => m.borrow().entries.iter().any(|(_, x)| self.contains_ref_to(x, target)),
            _ => false,
        }
    }

    // ---------- binding / checking ----------
    pub fn bind(&self, raw: Value, rt: &RT, path: &[Seg], parent: Option<&Inst>, sc: &Scope) -> R<Value> {
        if let Value::PreVal(pv) = &raw {
            let inst = parent.cloned().or_else(|| pv.scope.inst.clone());
            let sc2 = pv.scope.with_inst(inst);
            if let RTk::Ref(_) = rt.k {
                return match self.eval_place(&pv.expr, &sc2)? {
                    Some(p) => Ok(Value::Ref(Rc::new(p))),
                    None => err("not a place in ref position"),
                };
            }
            let v = self.ev(&pv.expr, &sc2)?;
            return self.bind(v, rt, path, parent, sc);
        }
        let fail = |msg: String, code: Option<&str>| -> Fail {
            let tail = rt.tail.borrow();
            match &*tail {
                Some(Tail::Inline { template, .. }) => {
                    let text: String = template.iter().filter_map(|p| if let TPart::Text(t) = p { Some(t.as_str()) } else { None }).collect();
                    self.env.report(Diag { severity: "error".into(), id: rt.name.borrow().clone(), message: text, path: path_str(path, None), code: Some("E4001".into()) });
                }
                _ => self.env.report(Diag::error(msg, path_str(path, None), Some(code.unwrap_or("E4001")))),
            }
            Fail::Taint
        };
        match &rt.k {
            RTk::Prim(n) => match (n.as_str(), &raw) {
                ("int", Value::Int(_)) | ("float", Value::Float(_)) | ("bool", Value::Bool(_)) | ("string", Value::Str(_)) | ("null", Value::Null) => Ok(raw),
                ("float", Value::Int(i)) => match exact_float(i) {
                    Some(f) => Ok(Value::Float(f)),
                    None => Err(fail(format!("expected {n}"), None)),
                },
                _ => Err(fail(format!("expected {n}"), None)),
            },
            RTk::Lit(v) => {
                if value_eq(&raw, v) {
                    Ok(raw)
                } else {
                    Err(fail(format!("expected {}", json_str(&lit_display(v))), None))
                }
            }
            RTk::Range { lo, hi, excl, base } => {
                let raw = match (&raw, base.as_str()) {
                    (Value::Int(i), "float") => match exact_float(i) {
                        Some(f) => Value::Float(f),
                        None => raw,
                    },
                    _ => raw,
                };
                let ok = if base == "int" { matches!(raw, Value::Int(_)) } else { matches!(raw, Value::Float(_)) };
                if !ok {
                    return Err(fail(format!("expected {base} in range"), None));
                }
                let hi_ok = if *excl { num_cmp(&raw, hi) == Some(Less) } else { matches!(num_cmp(&raw, hi), Some(Less | Equal)) };
                if matches!(num_cmp(&raw, lo), Some(Greater | Equal)) && hi_ok {
                    return Ok(raw);
                }
                Err(fail(format!("out of range {}..{}{}", num_s(lo), if *excl { "<" } else { "" }, num_s(hi)), None))
            }
            RTk::Pattern { src, re } => match &raw {
                Value::Str(s) if re.is_match(s) => Ok(raw),
                _ => Err(fail(format!("does not match /{src}/"), None)),
            },
            RTk::Quantity(dim) => {
                if let Value::Q { dim: d, .. } = &raw {
                    if d == dim {
                        return Ok(raw);
                    }
                }
                if let Value::JObj(es) = &raw {
                    if es.len() == 2 {
                        let v = es.iter().find(|(k, _)| k == "value").map(|(_, v)| v);
                        let u = es.iter().find(|(k, _)| k == "unit").map(|(_, v)| v);
                        if let (Some(v), Some(Value::Str(u))) = (v, u) {
                            let Ok((key, to_base)) = self.env.unit_info(u) else {
                                return Err(fail(format!("unknown unit {u}"), Some("E4073")));
                            };
                            if key != *dim {
                                return Err(fail("unit of wrong dimension".into(), Some("E4073")));
                            }
                            let f = match v {
                                Value::Int(i) => i.to_f64().unwrap_or(f64::INFINITY),
                                Value::Float(f) => *f,
                                _ => return Err(fail("expected quantity".into(), None)),
                            };
                            return Ok(Value::Q { dim: dim.clone(), value: f * to_base });
                        }
                    }
                }
                Err(fail("expected quantity".into(), None))
            }
            RTk::Ref(_) => match &raw {
                Value::Ref(_) => Ok(raw),
                Value::Str(s) => {
                    let segs = parse_path(s, &sc.root_name)?;
                    if self.resolve_segs(&segs)?.is_undef() {
                        return Err(fail(format!("dangling reference {s}"), Some("E6002")));
                    }
                    Ok(Value::Ref(Rc::new(segs)))
                }
                Value::Rec(_) | Value::Arr(_) | Value::Map(_) => Ok(Value::Ref(Rc::new(raw.place().unwrap()))),
                _ => Err(fail("expected reference path".into(), None)),
            },
            RTk::Arr { elem, lo, hi } => {
                let items: Vec<Value> = match &raw {
                    Value::PreArr(pa) => {
                        let mut items = vec![];
                        for (spread, v) in pa.iter() {
                            if *spread {
                                let s = match v {
                                    Value::PreVal(pv) => self.deref(self.ev(&pv.expr, &pv.scope)?)?,
                                    other => self.deref(other.clone())?,
                                };
                                items.extend(self.mat_arr(&s)?);
                            } else {
                                items.push(v.clone());
                            }
                        }
                        items
                    }
                    Value::JArr(a) => (**a).clone(),
                    Value::Arr(a) => a.borrow().items.clone(),
                    _ => return Err(fail("expected array".into(), None)),
                };
                if let Some(lo) = lo {
                    let n = items.len() as i64;
                    let h = hi.unwrap_or(i64::MAX);
                    if n < *lo || n > h {
                        return Err(fail(format!("array size {n} outside {lo}..{h}"), None));
                    }
                }
                let arr = Rc::new(RefCell::new(ArrV { items: vec![], path: path.to_vec() }));
                for (i, it) in items.into_iter().enumerate() {
                    let mut p = path.to_vec();
                    p.push(Seg::Idx(i));
                    match self.bind(it, elem, &p, parent, sc) {
                        Ok(v) => arr.borrow_mut().items.push(v),
                        Err(Fail::Taint) => arr.borrow_mut().items.push(Value::Absent),
                        Err(e) => return Err(e),
                    }
                }
                Ok(Value::Arr(arr))
            }
            RTk::Map { key, val } => {
                let es: Vec<(String, Value)> = match &raw {
                    Value::JObj(e) | Value::PreObj(e) => (**e).clone(),
                    Value::Map(m) => m.borrow().entries.clone(),
                    _ => return Err(fail("expected map".into(), None)),
                };
                let m = Rc::new(RefCell::new(MapV { entries: vec![], path: path.to_vec() }));
                for (k, v) in es {
                    match self.bind(Value::Str(k.clone()), key, path, parent, sc) {
                        Ok(_) => {}
                        Err(Fail::Taint) => continue,
                        Err(e) => return Err(e),
                    }
                    let mut p = path.to_vec();
                    p.push(Seg::Name(k.clone()));
                    match self.bind(v, val, &p, parent, sc) {
                        Ok(bv) => m.borrow_mut().set(k, bv),
                        Err(Fail::Taint) => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(Value::Map(m))
            }
            RTk::Union(arms) => {
                let rec_arms: Vec<&RT> = arms.iter().filter(|a| is_rec(a)).collect();
                if matches!(raw, Value::JObj(_) | Value::PreObj(_) | Value::Rec(_)) && !rec_arms.is_empty() {
                    let is_lit = |m: &Member| matches!(m.ty.as_ref().map(|t| &t.k), Some(RTk::Lit(_)));
                    let first = rec_members(rec_arms[0]);
                    let disc_names: Vec<String> = first
                        .iter()
                        .filter(|m| is_lit(m) && rec_arms.iter().all(|a| rec_members(a).iter().any(|x| x.name == m.name && is_lit(x))))
                        .map(|m| m.name.clone())
                        .collect();
                    for arm in &rec_arms {
                        let members = rec_members(arm);
                        let mut ok = true;
                        for dn in &disc_names {
                            let lit = members.iter().find(|x| &x.name == dn).and_then(|x| x.ty.clone()).and_then(|t| if let RTk::Lit(v) = &t.k { Some(v.clone()) } else { None }).unwrap_or(Value::Undef);
                            match self.raw_entry(&raw, dn)? {
                                Some(mv) => {
                                    if !value_eq(&self.raw_lit(&mv)?, &lit) {
                                        ok = false;
                                        break;
                                    }
                                }
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok {
                            return self.bind(raw, arm, path, parent, sc);
                        }
                    }
                    return Err(fail("no union arm matches discriminant".into(), None));
                }
                for arm in arms {
                    if self.kind_matches(&raw, arm)? {
                        return self.bind(raw, arm, path, parent, sc);
                    }
                }
                Err(fail("no union arm matches".into(), None))
            }
            RTk::Rec(_) => self.bind_record(raw, rt, path, parent, sc),
            RTk::Pred { base, preds } => {
                let v = self.bind(raw, base, path, parent, sc)?;
                for p in preds {
                    let f = self.ev(p, &Scope::new(&sc.root_name, sc.menv.clone()))?;
                    let ok = matches!(self.call(&f, vec![v.clone()], sc), Ok(Value::Bool(true)));
                    if !ok {
                        return Err(fail(format!("predicate {} not satisfied", json_str(&expr_name(p))), None));
                    }
                }
                Ok(v)
            }
            RTk::IsectN(arms) => {
                let mut v = raw.clone();
                for arm in arms {
                    v = self.bind(raw.clone(), arm, path, parent, sc)?;
                }
                Ok(v)
            }
            RTk::Any => Ok(raw),
            RTk::Func { .. } => match &raw {
                Value::Clo(_) | Value::Nat(_) | Value::Std(_) => Ok(raw),
                _ => Err(fail("expected function".into(), None)),
            },
        }
    }

    fn raw_entry(&self, raw: &Value, name: &str) -> R<Option<Value>> {
        match raw {
            Value::JObj(es) | Value::PreObj(es) => Ok(es.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())),
            Value::Rec(r) => {
                if r.borrow().has_slot(name) {
                    Ok(Some(self.force_slot(r, name)?))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    fn raw_lit(&self, v: &Value) -> R<Value> {
        match v {
            Value::PreVal(pv) => self.ev(&pv.expr, &pv.scope),
            other => Ok(other.clone()),
        }
    }

    fn kind_matches(&self, raw: &Value, rt: &RT) -> R<bool> {
        Ok(match &rt.k {
            RTk::Prim(n) => matches!(
                (n.as_str(), raw),
                ("int", Value::Int(_)) | ("float", Value::Float(_)) | ("bool", Value::Bool(_)) | ("string", Value::Str(_)) | ("null", Value::Null)
            ),
            RTk::Lit(v) => value_eq(&self.raw_lit(raw)?, v),
            RTk::Range { base, .. } => {
                if base == "int" {
                    matches!(raw, Value::Int(_))
                } else {
                    matches!(raw, Value::Float(_))
                }
            }
            RTk::Pattern { .. } => matches!(raw, Value::Str(_)),
            RTk::Arr { .. } => matches!(raw, Value::JArr(_) | Value::PreArr(_) | Value::Arr(_)),
            _ => true,
        })
    }

    fn eval_place(&self, e: &Rc<Expr>, sc: &Scope) -> R<Option<SegPath>> {
        let v = self.ev_nav(e, sc)?;
        Ok(match &v {
            Value::Segs(p) => Some((**p).clone()),
            Value::Rec(_) | Value::Arr(_) | Value::Map(_) => v.place(),
            _ => None,
        })
    }

    fn ev_nav(&self, e: &Rc<Expr>, sc: &Scope) -> R<Value> {
        match &**e {
            Expr::Member { x, name, .. } => {
                let x = self.deref(self.ev_nav(x, sc)?)?;
                let v = self.access(&x, name)?;
                if v.is_absent() {
                    if let Value::Rec(r) = &x {
                        let mut p = r.borrow().path.clone();
                        p.push(Seg::Name(name.clone()));
                        return Ok(Value::Segs(Rc::new(p)));
                    }
                }
                Ok(v)
            }
            Expr::Index { x, i } => {
                let x = self.deref(self.ev_nav(x, sc)?)?;
                let i = self.ev(i, sc)?;
                match &x {
                    Value::Arr(a) => {
                        let n = to_index(&i)?;
                        let b = a.borrow();
                        if n >= 0 && (n as usize) < b.items.len() {
                            Ok(b.items[n as usize].clone())
                        } else {
                            let mut p = b.path.clone();
                            p.push(Seg::Idx(n.max(0) as usize));
                            Ok(Value::Segs(Rc::new(p)))
                        }
                    }
                    Value::Map(m) => {
                        let Value::Str(k) = &i else { return err("map index needs a string") };
                        let b = m.borrow();
                        match b.get(k) {
                            Some(v) => Ok(v.clone()),
                            None => {
                                let mut p = b.path.clone();
                                p.push(Seg::Name(k.clone()));
                                Ok(Value::Segs(Rc::new(p)))
                            }
                        }
                    }
                    Value::Rec(r) => {
                        let Value::Str(k) = &i else { return err("index on record needs a string") };
                        let v = self.access(&x, k)?;
                        if v.is_absent() {
                            let mut p = r.borrow().path.clone();
                            p.push(Seg::Name(k.clone()));
                            Ok(Value::Segs(Rc::new(p)))
                        } else {
                            Ok(v)
                        }
                    }
                    _ => self.ev(e, sc),
                }
            }
            _ => self.ev(e, sc),
        }
    }

    fn bind_record(&self, raw: Value, rt: &RT, path: &[Seg], parent: Option<&Inst>, sc: &Scope) -> R<Value> {
        let RTk::Rec(rec) = &rt.k else { return err("bind_record on non-record type") };
        let entries: Vec<(String, Value)> = match &raw {
            Value::JObj(e) | Value::PreObj(e) => (**e).clone(),
            Value::Rec(r) => {
                let (order, extras, ders) = {
                    let b = r.borrow();
                    (b.entry_order.clone(), b.extras.clone(), b.slots.iter().filter(|(_, s)| s.kind == MKind::Der).map(|(n, _)| n.clone()).collect::<Vec<_>>())
                };
                let mut out = vec![];
                for n in order {
                    if ders.contains(&n) {
                        continue;
                    }
                    if let Some((_, v)) = extras.iter().find(|(k, _)| *k == n) {
                        out.push((n, v.clone()));
                    } else {
                        let v = self.force_slot(r, &n)?;
                        out.push((n, v));
                    }
                }
                out
            }
            _ => {
                self.env.report(Diag::error("expected record", path_str(path, None), Some("E4001")));
                return Err(Fail::Taint);
            }
        };
        let members = rec.members.borrow().clone();
        let inst: Inst = Rc::new(RefCell::new(RecInst {
            type_name: rt.name.borrow().clone(),
            rt: rt.clone(),
            path: path.to_vec(),
            parent: parent.cloned(),
            slots: vec![],
            entry_order: entries.iter().map(|(k, _)| k.clone()).collect(),
            extras: vec![],
            menv: sc.menv.clone(),
        }));
        if self.no_reg.get() == 0 {
            self.env.registry_push(inst.clone());
        }
        let mut supplied: HashMap<String, Value> = HashMap::new();
        for (k, v) in &entries {
            supplied.insert(k.clone(), v.clone());
        }
        for m in &members {
            let name = m.name.clone();
            let types: Vec<RT> = m.conj.clone().unwrap_or_else(|| m.ty.iter().cloned().collect());
            let menv = m.menv.clone().or_else(|| sc.menv.clone());
            let root_name = sc.root_name.clone();
            let has = supplied.get(&name).cloned();
            let mut push_deferred = false;
            let slot = match (m.kind, has) {
                (MKind::Der, has) => {
                    let expr = m.expr.clone().unwrap_or_else(|| Rc::new(Expr::Lit(Value::Null)));
                    // $referrers needs the whole universe: schedule for phase 2 up front
                    push_deferred = mentions_referrers(&expr);
                    Slot { kind: MKind::Der, state: SlotState::Unforced, value: Value::Undef, compute: Some(Compute::Derived { expr, ty: m.ty.clone(), supplied: has, name: name.clone(), root_name, menv }) }
                }
                (kind, Some(raw_v)) => Slot { kind, state: SlotState::Unforced, value: Value::Undef, compute: Some(Compute::Check { raw: raw_v, types, name: name.clone(), root_name, menv }) },
                (MKind::Dflt, None) => {
                    let expr = m.dflt.clone().unwrap_or_else(|| Rc::new(Expr::Lit(Value::Null)));
                    Slot { kind: MKind::Dflt, state: SlotState::Unforced, value: Value::Undef, compute: Some(Compute::Default { expr, types, name: name.clone(), root_name, menv }) }
                }
                (MKind::Opt, None) => Slot { kind: MKind::Opt, state: SlotState::Absent, value: Value::Undef, compute: None },
                (MKind::Req, None) => {
                    let mut p = path.to_vec();
                    p.push(Seg::Name(name.clone()));
                    self.env.report(Diag::error(format!("required member {name} missing"), path_str(&p, None), Some("E4002")));
                    Slot { kind: MKind::Req, state: SlotState::Invalid, value: Value::Undef, compute: None }
                }
            };
            inst.borrow_mut().slots.push((name.clone(), slot));
            if push_deferred {
                self.deferred_slots.borrow_mut().push((inst.clone(), name));
            }
        }
        for (k, v) in &entries {
            if members.iter().any(|m| m.name == *k) {
                continue;
            }
            if rec.open {
                inst.borrow_mut().set_extra(k, v.clone());
            } else {
                let nm = rt.name.borrow().as_ref().map(|n| format!(" {n}")).unwrap_or_default();
                let mut p = path.to_vec();
                p.push(Seg::Name(k.clone()));
                self.env.report(Diag::error(format!("undeclared member {k} on closed record{nm}"), path_str(&p, None), Some("E4003")));
            }
        }
        Ok(Value::Rec(inst))
    }

    fn run_compute(&self, inst: &Inst, c: &Compute) -> R<Value> {
        let path = inst.borrow().path.clone();
        let scope_for = |root_name: &str, menv: &Option<Rc<Env>>| Scope { inst: Some(inst.clone()), locals: Rc::new(HashMap::new()), root_name: root_name.to_string(), menv: menv.clone() };
        let member_path = |name: &str| {
            let mut p = path.clone();
            p.push(Seg::Name(name.to_string()));
            p
        };
        match c {
            Compute::Check { raw, types, name, root_name, menv } => {
                let isc = scope_for(root_name, menv);
                let mp = member_path(name);
                let mut v = Value::Null;
                for t in types {
                    v = self.bind(raw.clone(), t, &mp, Some(inst), &isc)?;
                }
                Ok(v)
            }
            Compute::Default { expr, types, name, root_name, menv } => {
                let isc = scope_for(root_name, menv);
                let mp = member_path(name);
                let v = self.ev(expr, &isc)?;
                let mut out = Value::Null;
                for t in types {
                    out = self.bind(v.clone(), t, &mp, Some(inst), &isc)?;
                }
                Ok(out)
            }
            Compute::Derived { expr, ty: t, supplied, name, root_name, menv } => {
                let isc = scope_for(root_name, menv);
                let mp = member_path(name);
                let mut v = self.ev(expr, &isc)?;
                if let Some(t) = t {
                    v = self.bind(v, t, &mp, Some(inst), &isc)?;
                } else if matches!(v, Value::PreObj(_) | Value::PreArr(_) | Value::JObj(_)) {
                    v = self.materialize(v, &mp)?;
                }
                if let Some(sv) = supplied {
                    let against = t.clone().unwrap_or_else(|| structural_of(&v));
                    self.no_reg.set(self.no_reg.get() + 1);
                    let restated = self.bind(sv.clone(), &against, &mp, Some(inst), &isc);
                    self.no_reg.set(self.no_reg.get() - 1);
                    let restated = restated?;
                    if !value_eq(&v, &restated) {
                        self.env.report(Diag::error(format!("derived member {name} restated with a differing value"), path_str(&mp, None), Some("E4005")));
                        return Err(Fail::Taint);
                    }
                }
                Ok(v)
            }
        }
    }

    fn materialize(&self, v: Value, path: &[Seg]) -> R<Value> {
        match v {
            Value::PreArr(items) => {
                let arr = Rc::new(RefCell::new(ArrV { items: vec![], path: path.to_vec() }));
                for (i, (_, it)) in items.iter().enumerate() {
                    let x = match it {
                        Value::PreVal(pv) => self.ev(&pv.expr, &pv.scope)?,
                        other => other.clone(),
                    };
                    let mut p = path.to_vec();
                    p.push(Seg::Idx(i));
                    let m = self.materialize(x, &p)?;
                    arr.borrow_mut().items.push(m);
                }
                Ok(Value::Arr(arr))
            }
            Value::PreObj(entries) => {
                let m = Rc::new(RefCell::new(MapV { entries: vec![], path: path.to_vec() }));
                for (k, pv) in entries.iter() {
                    let x = match pv {
                        Value::PreVal(pv) => self.ev(&pv.expr, &pv.scope)?,
                        other => other.clone(),
                    };
                    let mut p = path.to_vec();
                    p.push(Seg::Name(k.clone()));
                    let mv = self.materialize(x, &p)?;
                    m.borrow_mut().set(k.clone(), mv);
                }
                Ok(Value::Map(m))
            }
            other => Ok(other),
        }
    }

    fn with_expr(&self, base: &Rc<Expr>, patch: &Rc<Expr>, sc: &Scope) -> R<Value> {
        let base = self.deref(self.ev(base, sc)?)?;
        let merge = |entries: &mut Vec<(String, Value)>, patch: Value| -> R<()> {
            let Value::PreObj(pes) = patch else { return err("with: patch must be an object") };
            for (pk, pv) in pes.iter() {
                if let Some(e) = entries.iter_mut().find(|(n, _)| n == pk) {
                    e.1 = pv.clone();
                } else {
                    entries.push((pk.clone(), pv.clone()));
                }
            }
            Ok(())
        };
        if let Value::PreObj(bes) = &base {
            let p = self.ev(patch, sc)?;
            let mut entries = (**bes).clone();
            merge(&mut entries, p)?;
            return Ok(Value::PreObj(Rc::new(entries)));
        }
        let Value::Rec(r) = &base else { return err("with on non-record") };
        let p = self.ev(patch, sc)?;
        let (order, members) = {
            let b = r.borrow();
            (b.entry_order.clone(), rec_members(&b.rt))
        };
        let mut entries: Vec<(String, Value)> = vec![];
        for n in order {
            let (extra, skip) = {
                let b = r.borrow();
                match b.extra(&n) {
                    Some(v) => (Some(v.clone()), false),
                    None => (None, b.slot(&n).map(|s| s.kind == MKind::Der || s.state == SlotState::Absent).unwrap_or(true)),
                }
            };
            if let Some(v) = extra {
                entries.push((n, v));
                continue;
            }
            if skip {
                continue;
            }
            let v = self.force_slot(r, &n)?;
            entries.push((n, v));
        }
        for m in &members {
            if m.kind != MKind::Dflt || entries.iter().any(|(k, _)| *k == m.name) {
                continue;
            }
            let present = r.borrow().slot(&m.name).map(|s| s.state != SlotState::Absent).unwrap_or(false);
            if present {
                let v = self.force_slot(r, &m.name)?;
                entries.push((m.name.clone(), v));
            }
        }
        merge(&mut entries, p)?;
        Ok(Value::PreObj(Rc::new(entries)))
    }

    // ---------- slots ----------
    fn force_state(&self, inst: &Inst, name: &str) -> SlotState {
        self.force_slot_safe(inst, name);
        inst.borrow().slot(name).map(|s| s.state).unwrap_or(SlotState::Invalid)
    }

    fn force_slot_safe(&self, inst: &Inst, name: &str) {
        let _ = self.force_slot(inst, name);
    }

    pub fn force_slot(&self, inst: &Inst, name: &str) -> R<Value> {
        let (state, compute) = {
            let b = inst.borrow();
            let Some(s) = b.slot(name) else { return err(format!("no member {name}")) };
            match s.state {
                SlotState::Ok => return Ok(s.value.clone()),
                SlotState::Absent => return Ok(Value::Absent),
                SlotState::Invalid => return Err(Fail::Taint),
                _ => {}
            }
            (s.state, s.compute.clone())
        };
        let mut mp = inst.borrow().path.clone();
        mp.push(Seg::Name(name.to_string()));
        if state == SlotState::Forcing {
            self.env.report(Diag::error(format!("dependency cycle at {name}"), path_str(&mp, None), Some("E5007")));
            inst.borrow_mut().slot_mut(name).unwrap().state = SlotState::Invalid;
            return Err(Fail::Taint);
        }
        inst.borrow_mut().slot_mut(name).unwrap().state = SlotState::Forcing;
        let res = match &compute {
            Some(c) => self.run_compute(inst, c),
            None => Ok(Value::Null),
        };
        match res {
            Ok(v) => {
                let mut b = inst.borrow_mut();
                let s = b.slot_mut(name).unwrap();
                s.state = SlotState::Ok;
                s.value = v.clone();
                Ok(v)
            }
            Err(Fail::Defer) => {
                inst.borrow_mut().slot_mut(name).unwrap().state = SlotState::Unforced;
                self.deferred_slots.borrow_mut().push((inst.clone(), name.to_string()));
                Err(Fail::Defer)
            }
            Err(Fail::Eval(e)) => {
                {
                    let mut b = inst.borrow_mut();
                    let s = b.slot_mut(name).unwrap();
                    if s.state == SlotState::Forcing {
                        s.state = SlotState::Invalid;
                    }
                }
                self.env.report(Diag { severity: "error".into(), id: None, message: e.msg, path: path_str(&mp, None), code: e.code });
                Err(Fail::Taint)
            }
            Err(Fail::Taint) => {
                let mut b = inst.borrow_mut();
                let s = b.slot_mut(name).unwrap();
                if s.state == SlotState::Forcing {
                    s.state = SlotState::Invalid;
                }
                Err(Fail::Taint)
            }
        }
    }

    pub fn force_const_in(&self, env: &Rc<Env>, name: &str, root_name: &str) -> R<Value> {
        let c = env.consts.borrow().get(name).cloned();
        let Some(c) = c else { return err(format!("unknown constant {name}")) };
        if c.state.get() {
            return Ok(c.value.borrow().clone());
        }
        c.state.set(true);
        let sc = Scope::new(root_name, Some(env.clone()));
        let mut v = self.ev(&c.expr, &sc)?;
        if matches!(v, Value::PreObj(_) | Value::PreArr(_) | Value::JObj(_)) {
            v = match &c.ty {
                Some(t) => {
                    let rt = env.resolve(t, None).or_else(err)?;
                    self.bind(v, &rt, &[Seg::Name(name.to_string())], None, &sc)?
                }
                None => self.materialize(v, &[Seg::Name(name.to_string())])?,
            };
        }
        *c.value.borrow_mut() = v.clone();
        Ok(v)
    }

    // ---------- driving ----------
    pub fn force_all(&self, v: &Value) {
        match v {
            Value::Rec(r) => {
                let names: Vec<String> = r.borrow().slots.iter().map(|(n, _)| n.clone()).collect();
                for n in names {
                    self.force_slot_safe(r, &n);
                    let child = {
                        let b = r.borrow();
                        b.slot(&n).filter(|s| s.state == SlotState::Ok).map(|s| s.value.clone())
                    };
                    if let Some(c) = child {
                        self.force_all(&c);
                    }
                }
            }
            Value::Arr(a) => {
                let items = a.borrow().items.clone();
                for x in &items {
                    self.force_all(x);
                }
            }
            Value::Map(m) => {
                let vals: Vec<Value> = m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                for x in &vals {
                    self.force_all(x);
                }
            }
            _ => {}
        }
    }

    /// force every root, run the $referrers phase, then the assertion pass
    pub fn drive(&self, env: &Env) {
        for v in env.root_values() {
            self.force_all(&v);
        }
        self.phase.set(2);
        let mut i = 0;
        loop {
            let item = {
                let d = self.deferred_slots.borrow();
                if i >= d.len() {
                    break;
                }
                d[i].clone()
            };
            self.force_slot_safe(&item.0, &item.1);
            i += 1;
        }
        for v in env.root_values() {
            self.force_all(&v);
        }
        self.validate_all("");
    }

    pub fn validate_all(&self, root_name: &str) {
        for inst in self.env.registry_snapshot() {
            let asserts = match &inst.borrow().rt.k {
                RTk::Rec(r) => r.asserts.borrow().clone(),
                _ => vec![],
            };
            self.run_asserts(&inst, &asserts, root_name);
        }
    }

    fn run_asserts(&self, inst: &Inst, asserts: &[AssertItem], root_name: &str) {
        let (menv0, ipath, type_name) = {
            let b = inst.borrow();
            (b.menv.clone(), path_str(&b.path, None), b.type_name.clone())
        };
        let sc0 = Scope { inst: Some(inst.clone()), locals: Rc::new(HashMap::new()), root_name: root_name.to_string(), menv: menv0.clone() };
        for a in asserts {
            let sc = if a.menv.is_some() { sc0.with_menv(a.menv.clone()) } else { sc0.clone() };
            if a.when {
                let Ok(cond) = self.ev(&a.cond, &sc) else { continue };
                if matches!(cond, Value::Bool(true)) {
                    let inner: Vec<AssertItem> = a
                        .body
                        .iter()
                        .filter_map(|b| match b {
                            MemberAst::Assert { name, cond, tail } => Some(AssertItem { when: false, name: name.clone(), cond: cond.clone(), tail: tail.clone(), body: vec![], origin: a.origin.clone(), menv: a.menv.clone() }),
                            MemberAst::When { cond, body } => Some(AssertItem { when: true, name: String::new(), cond: cond.clone(), tail: None, body: body.clone(), origin: a.origin.clone(), menv: a.menv.clone() }),
                            _ => None,
                        })
                        .collect();
                    self.run_asserts(inst, &inner, root_name);
                }
                continue;
            }
            let ok = match self.ev(&a.cond, &sc) {
                Ok(v) => v,
                Err(Fail::Eval(e)) => {
                    self.env.report(Diag { severity: "error".into(), id: None, message: format!("{}: {}", a.name, e.msg), path: ipath.clone(), code: e.code });
                    continue;
                }
                Err(_) => continue,
            };
            if matches!(ok, Value::Bool(true)) {
                continue;
            }
            let id = format!("{}.{}", a.origin.clone().or_else(|| type_name.clone()).unwrap_or_default(), a.name);
            match &a.tail {
                None => self.env.report(Diag { severity: "error".into(), id: Some(id), message: format!("assert {} failed", a.name), path: ipath.clone(), code: Some("E6001".into()) }),
                Some(Tail::Inline { severity, template }) => {
                    let msg = self.render_lenient(template, &sc);
                    let code = match severity.as_str() {
                        "error" => "E6001",
                        "warn" => "W6001",
                        _ => "I6001",
                    };
                    self.env.report(Diag { severity: severity.clone(), id: Some(id), message: msg, path: ipath.clone(), code: Some(code.into()) });
                }
                Some(Tail::Ref { name, args }) => {
                    let d = menv0.as_ref().and_then(|e| e.diags.borrow().get(name).cloned()).or_else(|| self.env.diags.borrow().get(name).cloned());
                    let Some(d) = d else {
                        self.env.report(Diag::error(format!("unknown diagnostic {name}"), ipath.clone(), None));
                        continue;
                    };
                    let argv: Vec<Value> = args.iter().map(|x| self.ev(x, &sc).unwrap_or(Value::Absent)).collect();
                    let mut locals = HashMap::new();
                    for (i, p) in d.params.iter().enumerate() {
                        locals.insert(p.name.clone(), argv.get(i).cloned().unwrap_or(Value::Absent));
                    }
                    let psc = Scope { inst: None, locals: Rc::new(locals), root_name: root_name.to_string(), menv: menv0.clone() };
                    let msg = self.render_lenient(&d.template, &psc);
                    let code = if d.severity == "error" { "E6001" } else { "W6001" };
                    self.env.report(Diag { severity: d.severity.clone(), id: Some(id), message: msg, path: ipath.clone(), code: Some(code.into()) });
                }
            }
        }
    }

    fn render_lenient(&self, template: &[TPart], sc: &Scope) -> String {
        let mut s = String::new();
        for p in template {
            match p {
                TPart::Text(t) => s.push_str(t),
                TPart::Expr(x) => {
                    if let Ok(v) = self.ev(x, sc) {
                        if let Ok(t) = self.to_str(&v) {
                            s.push_str(&t);
                        }
                    }
                }
            }
        }
        s
    }

    // ---------- serialization ----------
    pub fn serialize(&self, v: &Value, root_name: &str) -> String {
        self.go(v, root_name).unwrap_or_default()
    }

    fn go(&self, x: &Value, root: &str) -> Option<String> {
        Some(match x {
            Value::Absent | Value::Undef => return None,
            Value::Null => "null".into(),
            Value::Bool(b) => if *b { "true".into() } else { "false".into() },
            Value::Int(i) => i.to_string(),
            Value::Float(f) => fmt_f(*f),
            Value::Str(s) => json_str(s),
            Value::Q { dim, value } => {
                let unit = self.env.base_unit_of.borrow().get(dim).cloned().unwrap_or_else(|| dim.clone());
                format!("{{\"value\":{},\"unit\":{}}}", fmt_f(*value), json_str(&unit))
            }
            Value::Ref(p) => json_str(&path_str(p, Some(root))),
            Value::Arr(a) => format!("[{}]", a.borrow().items.iter().filter_map(|i| self.go(i, root)).collect::<Vec<_>>().join(",")),
            Value::Map(m) => format!(
                "{{{}}}",
                m.borrow().entries.iter().filter_map(|(k, v)| self.go(v, root).map(|g| format!("{}:{g}", json_str(k)))).collect::<Vec<_>>().join(",")
            ),
            Value::Rec(r) => {
                let b = r.borrow();
                let mut parts = vec![];
                let mut done: HashSet<String> = HashSet::new();
                for n in &b.entry_order {
                    done.insert(n.clone());
                    if let Some(v) = b.extra(n) {
                        parts.push(format!("{}:{}", json_str(n), self.raw_json(v, root)));
                        continue;
                    }
                    let Some(s) = b.slot(n) else { continue };
                    if matches!(s.state, SlotState::Invalid | SlotState::Absent) || s.kind == MKind::Der {
                        continue;
                    }
                    if let Some(g) = self.go(&s.value, root) {
                        parts.push(format!("{}:{g}", json_str(n)));
                    }
                }
                for m in rec_members(&b.rt) {
                    if done.contains(&m.name) && m.kind != MKind::Der {
                        continue;
                    }
                    let Some(s) = b.slot(&m.name) else { continue };
                    if matches!(s.state, SlotState::Invalid | SlotState::Absent | SlotState::Unforced) {
                        continue;
                    }
                    if let Some(g) = self.go(&s.value, root) {
                        parts.push(format!("{}:{g}", json_str(&m.name)));
                    }
                }
                format!("{{{}}}", parts.join(","))
            }
            Value::JObj(_) | Value::JArr(_) => self.raw_json(x, root),
            _ => return None,
        })
    }

    fn raw_json(&self, v: &Value, root: &str) -> String {
        match v {
            Value::Null => "null".into(),
            Value::Bool(b) => if *b { "true".into() } else { "false".into() },
            Value::Int(i) => i.to_string(),
            Value::Float(f) => fmt_f(*f),
            Value::Str(s) => json_str(s),
            Value::JArr(items) => format!("[{}]", items.iter().map(|x| self.raw_json(x, root)).collect::<Vec<_>>().join(",")),
            Value::JObj(es) => format!("{{{}}}", es.iter().map(|(k, x)| format!("{}:{}", json_str(k), self.raw_json(x, root))).collect::<Vec<_>>().join(",")),
            Value::PreVal(pv) => match self.ev(&pv.expr, &pv.scope) {
                Ok(v) => self.go(&v, root).unwrap_or_else(|| "null".into()),
                Err(_) => "null".into(),
            },
            other => self.go(other, root).unwrap_or_else(|| "null".into()),
        }
    }
}
