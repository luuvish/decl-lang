//! The decl layer of the query engine (qengine/DESIGN.md), the port of
//! decl-ts/src/qengine/qeval.ts: compile expressions to query computes and
//! evaluate a module's outputs through the incremental core (db.rs). Value
//! semantics — equality, serialization, type binding, arithmetic — are reused
//! from the value layer (`Engine`); only the evaluation strategy is new. Built
//! up stage by stage; a form not yet compiled raises [`Unsupported`].
use crate::ast::{Expr, ForClause, MatchArm, TPart, TypeAst};
use crate::engine::{num_cmp, to_index, Engine, Inst, RootSrc};
use crate::qengine::db::{Db, DbErr};
use crate::semantics::{
    compile_pattern, err, err_code, path_str, pattern_error, rec_members, seg_text, sort_diags,
    value_eq, ArrV, Compute, Diag, Env, EvalErr, Fail, MKind, MapV, Num, RTk, RecInst, Scope, Seg,
    Slot, SlotState, Value, R, RT,
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// an expression or member form this stage of the port does not compile yet
pub struct Unsupported(pub String);

/// the sentinel code marking a form the query engine hit at run time but does
/// not handle; qevaluate turns it back into [`Unsupported`] so the caller falls
/// back to the tree walker
const QUNSUP: &str = "__QUNSUP__";
fn unsup(msg: &str) -> Fail {
    Fail::Eval(EvalErr {
        msg: format!("qeval unsupported: {msg}"),
        code: Some(QUNSUP.into()),
    })
}

/// bindings in scope (comprehension loop variables)
pub type Locals = FxHashMap<String, Value>;
/// a compiled expression: a closure over the query engine and local bindings
type Op = Rc<dyn Fn(&QEval, &Locals) -> R<Value>>;

/// the result of evaluating a module's outputs
pub struct QReport {
    /// no error diagnostic was produced
    pub ok: bool,
    /// each output, serialized as canonical JSON text
    pub outputs: Vec<(String, String)>,
    /// evaluation and validation diagnostics
    pub diagnostics: Vec<Diag>,
}

/// the lexical context a compile happens in
struct CCtx {
    self_inst: Option<Inst>, // the record instance being constructed (§4.3 siblings)
    root_name: String,       // the enclosing evaluation root's name ($root, ctx)
    locals: FxHashSet<String>, // comprehension loop variables in scope
    menv: Rc<Env>,           // the module scope (consts, funcs, unit table)
    entry_env: Rc<Env>,      // the universe entry (its consts are query-native)
}

fn truthy(v: &Value) -> R<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => err_code("non-bool condition", "E5000"),
    }
}

fn apply_un(op: &str, x: Value) -> R<Value> {
    match op {
        "!" => Ok(Value::Bool(!truthy(&x)?)),
        "-" => match x {
            Value::Int(i) => Ok(Value::Int(-i)),
            Value::Float(f) => Ok(Value::Float(-f)),
            Value::Q { dim, value } => Ok(Value::Q { dim, value: -value }),
            _ => err_code("bad operand for unary -", "E5000"),
        },
        "~" => match x {
            Value::Int(i) => Ok(Value::Int(&(-i) - 1)),
            _ => err_code("bad operand for ~", "E5000"),
        },
        _ => err_code("un", "E5000"),
    }
}

/// value-level binary operators (§4.4–4.5), mirroring the value layer's binop
fn apply_bin(op: &str, l: &Value, r: &Value) -> R<Value> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    if op == "==" {
        return Ok(Value::Bool(value_eq(l, r)));
    }
    if op == "!=" {
        return Ok(Value::Bool(!value_eq(l, r)));
    }
    if l.is_absent() || r.is_absent() {
        return err_code("absent consumed", "E5000");
    }
    let both_num = matches!(
        (l, r),
        (Value::Int(_), Value::Int(_)) | (Value::Float(_), Value::Float(_))
    );
    let both_s = matches!((l, r), (Value::Str(_), Value::Str(_)));
    match (op, l, r) {
        ("+", Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}").into())),
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
        ("<" | "<=" | ">" | ">=", _, _) if both_num || both_s => {
            let o = num_cmp(l, r);
            Ok(Value::Bool(matches!(
                (op, o),
                ("<", Some(Less))
                    | ("<=", Some(Less | Equal))
                    | (">", Some(Greater))
                    | (">=", Some(Greater | Equal))
            )))
        }
        ("&", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
        ("|", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
        ("^", Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
        ("<<" | ">>", Value::Int(a), Value::Int(b)) => {
            if b.is_negative() {
                return err_code("negative shift count", "E5003");
            }
            let n = b.to_usize().ok_or(Fail::Eval(EvalErr {
                msg: "shift count too large".into(),
                code: None,
            }))?;
            Ok(Value::Int(if op == "<<" { a << n } else { a >> n }))
        }
        _ => err_code(format!("bad operands for {op}"), "E5000"),
    }
}

/// the `in` membership operator (§4.5): element/key/index presence. The
/// container is materialized (a lazy prevalue flattened); `in` over a record
/// needs slot forcing, so it defers to the value layer (Unsupported).
fn in_op(q: &QEval, l: Value, r: Value) -> R<Value> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let container = q.helper.mat_val(r)?;
    match &container {
        Value::Range { lo, hi, excl } => {
            let ge = matches!(num_cmp(&l, lo), Some(Greater | Equal));
            let hi_ok = if *excl {
                num_cmp(&l, hi) == Some(Less)
            } else {
                matches!(num_cmp(&l, hi), Some(Less | Equal))
            };
            Ok(Value::Bool(ge && hi_ok))
        }
        Value::Arr(a) => Ok(Value::Bool(
            a.borrow().items.iter().any(|x| value_eq(&l, x)),
        )),
        Value::Map(m) => match &l {
            Value::Str(k) => Ok(Value::Bool(m.borrow().has(k))),
            _ => Ok(Value::Bool(false)),
        },
        Value::Rec(_) => Err(unsup("`in` over a record")),
        _ => err("in: bad container"),
    }
}

/// does a type contain a record anywhere (so its values live in the query
/// graph)? `seen` breaks the cycle of a type that names itself (§3.1)
fn has_rec(t: &RT, seen: &mut Vec<usize>) -> bool {
    let id = Rc::as_ptr(t) as usize;
    if seen.contains(&id) {
        return false;
    }
    seen.push(id);
    match &t.k {
        RTk::Rec(_) => true,
        RTk::Arr { elem, .. } => has_rec(elem, seen),
        RTk::Map { val, .. } => has_rec(val, seen),
        RTk::Union(arms) => arms.borrow().iter().any(|a| has_rec(a, seen)),
        _ => false,
    }
}

/// a fresh `CCtx` sharing everything but the in-scope local names
fn with_locals(c: &CCtx, locals: FxHashSet<String>) -> CCtx {
    CCtx {
        self_inst: c.self_inst.clone(),
        root_name: c.root_name.clone(),
        locals,
        menv: c.menv.clone(),
        entry_env: c.entry_env.clone(),
    }
}

/// a compiled comprehension clause: what it ranges over and its `if` filters
struct CClause {
    v: String,
    iter: Op,
    filters: Vec<Op>,
}

/// compile a comprehension's clauses (§4.8): each clause sees the prior loop
/// variables in its range, and its own variable in its filters
fn compile_clauses(
    clauses: &[ForClause],
    all_vars: &[String],
    c: &CCtx,
) -> Result<Vec<CClause>, Unsupported> {
    let mut ccs = Vec::new();
    for (i, cl) in clauses.iter().enumerate() {
        let mut prior = c.locals.clone();
        for v in &all_vars[..i] {
            prior.insert(v.clone());
        }
        let iter = compile(&cl.iter, &with_locals(c, prior.clone()))?;
        let mut with_this = prior;
        with_this.insert(cl.v.clone());
        let filters = cl
            .filters
            .iter()
            .map(|f| compile(f, &with_locals(c, with_this.clone())))
            .collect::<Result<Vec<Op>, Unsupported>>()?;
        ccs.push(CClause {
            v: cl.v.clone(),
            iter,
            filters,
        });
    }
    Ok(ccs)
}

/// whether every filter of a clause holds for the given bindings
fn filters_pass(q: &QEval, cl: &CClause, loc: &Locals) -> R<bool> {
    for f in &cl.filters {
        if !truthy(&(f)(q, loc)?)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// run an array comprehension, appending each head value in nested-loop order
fn run_comp(
    q: &QEval,
    ccs: &[CClause],
    i: usize,
    loc: &Locals,
    head: &Op,
    out: &mut Vec<Value>,
) -> R<()> {
    if i == ccs.len() {
        out.push((head)(q, loc)?);
        return Ok(());
    }
    let cl = &ccs[i];
    let it = q.helper.mat_val((cl.iter)(q, loc)?)?;
    for el in q.helper.iterate(&it)? {
        let mut loc2 = loc.clone();
        loc2.insert(cl.v.clone(), el);
        if filters_pass(q, cl, &loc2)? {
            run_comp(q, ccs, i + 1, &loc2, head, out)?;
        }
    }
    Ok(())
}

/// run a map comprehension, appending key/value pairs in nested-loop order
fn run_mapcomp(
    q: &QEval,
    ccs: &[CClause],
    i: usize,
    loc: &Locals,
    key: &Op,
    val: &Op,
    out: &mut Vec<(String, Value)>,
) -> R<()> {
    if i == ccs.len() {
        let k = (key)(q, loc)?;
        let Value::Str(ks) = &k else {
            return err("map key must be string");
        };
        if out.iter().any(|(ek, _)| ek.as_str() == ks.as_ref()) {
            return err_code(format!("duplicate key {ks}"), "E5004");
        }
        out.push((ks.to_string(), (val)(q, loc)?));
        return Ok(());
    }
    let cl = &ccs[i];
    let it = q.helper.mat_val((cl.iter)(q, loc)?)?;
    for el in q.helper.iterate(&it)? {
        let mut loc2 = loc.clone();
        loc2.insert(cl.v.clone(), el);
        if filters_pass(q, cl, &loc2)? {
            run_mapcomp(q, ccs, i + 1, &loc2, key, val, out)?;
        }
    }
    Ok(())
}

/// a compiled `match` arm: the type it selects on (none for the catch-all),
/// the bound variable, and the body
struct MArm {
    v: String,
    rt: Option<RT>,
    body: Op,
}

/// how a record slot's compute is built in `bind_record`'s second pass
enum SlotPlan {
    /// a slot with no compute (absent optional, invalid required/hidden)
    Skip,
    /// a `ref<T>` member: a place navigation checked for integrity (§7.4–7.5)
    Ref { place: Rc<Expr> },
    /// a derived member (§5.4), optionally restating a supplied value
    Der {
        expr: Rc<Expr>,
        ty: Option<RT>,
        restate: Option<Rc<Expr>>,
        member_path: Vec<Seg>,
    },
    /// a supplied or defaulted value member, bound through its type(s)
    Supply {
        expr: Rc<Expr>,
        types: Vec<RT>,
        member_path: Vec<Seg>,
    },
}

/// resolve a `match` arm's type against the module scope; the catch-all has none
fn compile_arm_type(a: &MatchArm, c: &CCtx) -> Result<Option<RT>, Unsupported> {
    match &a.ty {
        Some(t) => Ok(Some(
            c.menv
                .resolve(t, None)
                .map_err(|_| Unsupported("match arm type".into()))?,
        )),
        None => Ok(None),
    }
}

/// compile a context variable (§7.3): `$this`/`$parent`/`$root` are references
/// to the containing instances; `$key`/`$path` are plain values
fn compile_ctx(nm: &str, c: &CCtx) -> Result<Op, Unsupported> {
    let self_inst = c.self_inst.clone();
    let root_name = c.root_name.clone();
    match nm {
        "$this" => Ok(Rc::new(move |_q, _l| match &self_inst {
            Some(i) => Ok(Value::Ref(i.borrow().path.clone())),
            None => err_code("$this outside a record instance", "E4090"),
        })),
        "$parent" => Ok(Rc::new(move |_q, _l| {
            match self_inst.as_ref().and_then(|i| i.borrow().parent.clone()) {
                Some(p) => Ok(Value::Ref(p.borrow().path.clone())),
                None => err_code("$parent: the evaluation root has no owner", "E4090"),
            }
        })),
        "$root" => Ok(Rc::new(move |_q, _l| {
            Ok(Value::Ref(Rc::new(vec![Seg::Name(Rc::from(
                root_name.as_str(),
            ))])))
        })),
        "$key" => Ok(Rc::new(move |_q, _l| {
            let Some(i) = self_inst.as_ref() else {
                return err_code("$key: the instance is not a collection element", "E4090");
            };
            let b = i.borrow();
            let Some(parent) = &b.parent else {
                return err_code("$key: the instance is not a collection element", "E4090");
            };
            if b.path.len() < parent.borrow().path.len() + 2 {
                return err_code("$key: the instance is not a collection element", "E4090");
            }
            Ok(match b.path.last() {
                Some(Seg::Idx(k)) => Value::Int(Num::from(*k)),
                Some(Seg::Name(k)) | Some(Seg::Key(k)) => Value::Str(k.clone()),
                None => Value::Absent,
            })
        })),
        "$path" => Ok(Rc::new(move |_q, _l| match &self_inst {
            Some(i) => Ok(Value::Str(path_str(&i.borrow().path, None).into())),
            None => err_code("$path outside a record instance", "E4090"),
        })),
        _ => Err(Unsupported(format!("ctx {nm}"))),
    }
}

/// compile a call's callee (§4.9): a member chain over `std` builds a std path
/// (`std.math.min`) rather than a data access; a namespace export resolves
/// through the value layer; anything else compiles normally (a name may resolve
/// to a module function's closure)
fn compile_callee(e: &Rc<Expr>, c: &CCtx) -> Result<Op, Unsupported> {
    if let Expr::Member { x, name, .. } = &**e {
        let nm = name.clone();
        let xop = compile_callee(x, c)?;
        return Ok(Rc::new(move |q, l| {
            let x = xop(q, l)?;
            match x {
                Value::Std(p) => {
                    let mut p2 = (*p).clone();
                    p2.push(nm.clone());
                    Ok(Value::Std(Rc::new(p2)))
                }
                Value::NsRef(ns) => {
                    let sc = Scope::new("", Some(q.env.clone()));
                    q.helper.mat_val(q.helper.ns_value(&ns, &nm, &sc)?)
                }
                other => {
                    let d = if matches!(other, Value::Ref(_)) {
                        q.helper.deref(other)?
                    } else {
                        other
                    };
                    q.helper.access(&d, &nm)
                }
            }
        }));
    }
    compile(e, c)
}

/// a compiled place navigation: the path segments a ref-position expression
/// denotes, or None when the expression is not a place
type PlaceOp = Rc<dyn Fn(&QEval, &Locals) -> R<Option<Vec<Seg>>>>;

/// navigate an expression as a place (§7.4): a step past a missing member, key,
/// or index still names the location (accumulated in a `Segs` value); a present
/// step yields the value there. Mirrors the value layer's `ev_nav`.
fn compile_nav(e: &Rc<Expr>, c: &CCtx) -> Result<Op, Unsupported> {
    match &**e {
        Expr::If { c: cc, t, f } => {
            let cop = compile(cc, c)?;
            let top = compile_nav(t, c)?;
            let fop = compile_nav(f, c)?;
            Ok(Rc::new(move |q, l| {
                if truthy(&cop(q, l)?)? {
                    top(q, l)
                } else {
                    fop(q, l)
                }
            }))
        }
        Expr::Paren(x) => compile_nav(x, c),
        Expr::Member { x, name, .. } => {
            let nm = name.clone();
            let xop = compile_nav(x, c)?;
            Ok(Rc::new(move |q, l| {
                let x0 = xop(q, l)?;
                if let Value::Segs(p) = &x0 {
                    let mut p = (**p).clone();
                    p.push(Seg::Name(Rc::from(nm.as_str())));
                    return Ok(Value::Segs(Rc::new(p)));
                }
                let x = q.helper.deref(x0)?;
                let v = q.helper.access(&x, &nm)?;
                if v.is_absent() {
                    if let Value::Rec(r) = &x {
                        let mut p = r.borrow().path.to_vec();
                        p.push(Seg::Name(Rc::from(nm.as_str())));
                        return Ok(Value::Segs(Rc::new(p)));
                    }
                }
                Ok(v)
            }))
        }
        Expr::Index { x, i } => {
            let normal = compile(e, c)?; // a non-collection base falls to normal eval
            let xop = compile_nav(x, c)?;
            let iop = compile(i, c)?;
            Ok(Rc::new(move |q, l| {
                let x0 = xop(q, l)?;
                let iv = iop(q, l)?;
                if let Value::Segs(p) = &x0 {
                    let mut p = (**p).clone();
                    match &iv {
                        Value::Str(k) => p.push(Seg::Key(k.clone())),
                        _ => p.push(Seg::Idx(to_index(&iv)?.max(0) as usize)),
                    }
                    return Ok(Value::Segs(Rc::new(p)));
                }
                let x = q.helper.deref(x0)?;
                match &x {
                    Value::Arr(a) => {
                        let n = to_index(&iv)?;
                        let b = a.borrow();
                        if n >= 0 && (n as usize) < b.items.len() {
                            Ok(b.items[n as usize].clone())
                        } else {
                            let mut p = b.path.to_vec();
                            p.push(Seg::Idx(n.max(0) as usize));
                            Ok(Value::Segs(Rc::new(p)))
                        }
                    }
                    Value::Map(m) => {
                        let Value::Str(k) = &iv else {
                            return err("map index needs a string");
                        };
                        let b = m.borrow();
                        match b.get(k) {
                            Some(v) => Ok(v.clone()),
                            None => {
                                let mut p = b.path.to_vec();
                                p.push(Seg::Key(k.clone()));
                                Ok(Value::Segs(Rc::new(p)))
                            }
                        }
                    }
                    Value::Rec(r) => {
                        let Value::Str(k) = &iv else {
                            return err("index on record needs a string");
                        };
                        let v = q.helper.access(&x, k)?;
                        if v.is_absent() {
                            let mut p = r.borrow().path.to_vec();
                            p.push(Seg::Name(k.clone()));
                            Ok(Value::Segs(Rc::new(p)))
                        } else {
                            Ok(v)
                        }
                    }
                    _ => normal(q, l),
                }
            }))
        }
        _ => compile(e, c),
    }
}

/// the path segments a ref-position expression denotes, or None if not a place
fn compile_place(e: &Rc<Expr>, c: &CCtx) -> Result<PlaceOp, Unsupported> {
    let nav = compile_nav(e, c)?;
    Ok(Rc::new(move |q, l| {
        let v = nav(q, l)?;
        Ok(match v {
            Value::Segs(p) => Some((*p).clone()),
            other => other.place(),
        })
    }))
}

/// compile an expression to a query compute (a closure over the query engine)
fn compile(e: &Rc<Expr>, c: &CCtx) -> Result<Op, Unsupported> {
    match &**e {
        Expr::Lit(v) => {
            let v = v.clone();
            Ok(Rc::new(move |_q, _l| Ok(v.clone())))
        }
        Expr::UnitLit { num, unit } => {
            let num = *num;
            let unit = unit.clone();
            Ok(Rc::new(move |q, _l| {
                let (dim, to_base) = q
                    .env
                    .unit_info(&unit)
                    .map_err(|m| Fail::Eval(EvalErr { msg: m, code: None }))?;
                Ok(Value::Q {
                    dim,
                    value: num * to_base,
                })
            }))
        }
        Expr::Paren(x) => compile(x, c),
        Expr::Un { op, x } => {
            let op = op.clone();
            let xc = compile(x, c)?;
            Ok(Rc::new(move |q, l| apply_un(&op, xc(q, l)?)))
        }
        Expr::If { c: cc, t, f } => {
            let cop = compile(cc, c)?;
            let top = compile(t, c)?;
            let fop = compile(f, c)?;
            Ok(Rc::new(move |q, l| {
                if truthy(&cop(q, l)?)? {
                    top(q, l)
                } else {
                    fop(q, l)
                }
            }))
        }
        Expr::Bin { op, l, r } => {
            if op == "|>" {
                // first-argument insertion (§4.9): `l |> f(a)` is `f(l, a)`
                let call = match &**r {
                    Expr::Call { fun, args } => {
                        let mut a = vec![l.clone()];
                        a.extend(args.iter().cloned());
                        Expr::Call {
                            fun: fun.clone(),
                            args: a,
                        }
                    }
                    _ => Expr::Call {
                        fun: r.clone(),
                        args: vec![l.clone()],
                    },
                };
                return compile(&Rc::new(call), c);
            }
            let op = op.clone();
            let lc = compile(l, c)?;
            let rc = compile(r, c)?;
            match op.as_str() {
                "&&" => Ok(Rc::new(move |q, l| {
                    Ok(Value::Bool(if truthy(&lc(q, l)?)? {
                        truthy(&rc(q, l)?)?
                    } else {
                        false
                    }))
                })),
                "||" => Ok(Rc::new(move |q, l| {
                    Ok(Value::Bool(if truthy(&lc(q, l)?)? {
                        true
                    } else {
                        truthy(&rc(q, l)?)?
                    }))
                })),
                "??" => Ok(Rc::new(move |q, l| {
                    let v = lc(q, l)?;
                    if matches!(v, Value::Absent | Value::Null) {
                        rc(q, l)
                    } else {
                        Ok(v)
                    }
                })),
                ".." | "..<" => {
                    let excl = op == "..<";
                    Ok(Rc::new(move |q, l| {
                        Ok(Value::Range {
                            lo: Box::new(lc(q, l)?),
                            hi: Box::new(rc(q, l)?),
                            excl,
                        })
                    }))
                }
                "matches" => Ok(Rc::new(move |q, l| {
                    let (s, p) = (lc(q, l)?, rc(q, l)?);
                    let (Value::Str(s), Value::Pat(p)) = (&s, &p) else {
                        return err_code("matches needs a string and a pattern", "E5000");
                    };
                    if let Some(bad) = pattern_error(p) {
                        return err_code(format!("malformed pattern /{p}/: {bad}"), "E4119");
                    }
                    let re = compile_pattern(p)
                        .map_err(|m| Fail::Eval(EvalErr { msg: m, code: None }))?;
                    Ok(Value::Bool(re.is_match(s)))
                })),
                "in" => Ok(Rc::new(move |q, l| in_op(q, lc(q, l)?, rc(q, l)?))),
                _ => Ok(Rc::new(move |q, l| {
                    let (lv, rv) = (lc(q, l)?, rc(q, l)?);
                    if matches!(lv, Value::Q { .. }) || matches!(rv, Value::Q { .. }) {
                        if let "+" | "-" | "*" | "/" | "<" | "<=" | ">" | ">=" = op.as_str() {
                            return q.helper.q_arith(&op, &lv, &rv);
                        }
                    }
                    apply_bin(&op, &lv, &rv)
                })),
            }
        }
        Expr::Name(name) => {
            let nm = name.clone();
            if c.locals.contains(&nm) {
                return Ok(Rc::new(move |_q, l| {
                    Ok(l.get(&nm).cloned().unwrap_or(Value::Undef))
                }));
            }
            // a member of the enclosing record — or an ancestor, nearest first
            // (§8 scoping) — shadows module names; read it through access so an
            // absent optional yields ABSENT
            let mut cur = c.self_inst.clone();
            while let Some(inst) = cur {
                if inst.borrow().has_slot(&nm) {
                    let owner = inst.clone();
                    let nm2 = nm.clone();
                    return Ok(Rc::new(move |q, _l| {
                        q.helper.access(&Value::Rec(owner.clone()), &nm2)
                    }));
                }
                cur = inst.borrow().parent.clone();
            }
            // an entry-module const is query-native (memoized here)
            if c.menv.consts.borrow().contains_key(&nm) && Rc::ptr_eq(&c.menv, &c.entry_env) {
                let key = format!("const:{nm}");
                return Ok(Rc::new(move |q, _l| q.query(&key)));
            }
            // otherwise a module value (a function's closure, an import, a
            // namespace, another module's const), `std`, another evaluation
            // root, or an input — all resolved through the value layer, in the
            // tree walker's order; a form that resolves to none stays Unsupported
            let menv = c.menv.clone();
            let root_name = c.root_name.clone();
            Ok(Rc::new(move |q, _l| {
                if let Some(v) = q.helper.module_value(&menv, &nm, &root_name)? {
                    return Ok(v);
                }
                if nm == "std" {
                    return Ok(Value::Std(Rc::new(vec![])));
                }
                q.helper.record(format!("root:{nm}"));
                if let Some(v) = q.helper.root(&nm) {
                    return Ok(v);
                }
                if let Some(v) = q.helper.demand_input(&menv, &nm)? {
                    return Ok(v);
                }
                Err(unsup(&format!("name {nm}")))
            }))
        }
        Expr::Member { x, name, safe } => {
            let nm = name.clone();
            let safe = *safe;
            let xc = compile(x, c)?;
            Ok(Rc::new(move |q, l| {
                let xv = xc(q, l)?;
                if let Value::NsRef(ns) = &xv {
                    // a namespace export (§8): resolved and materialized
                    let sc = Scope::new("", Some(q.env.clone()));
                    return q.helper.mat_val(q.helper.ns_value(ns, &nm, &sc)?);
                }
                if safe && matches!(xv, Value::Null | Value::Absent) {
                    return Ok(Value::Absent);
                }
                let xv = if matches!(xv, Value::Ref(_)) {
                    q.helper.deref(xv)?
                } else {
                    xv
                };
                q.helper.access(&xv, &nm)
            }))
        }
        Expr::Index { x, i } => {
            let xc = compile(x, c)?;
            let ic = compile(i, c)?;
            Ok(Rc::new(move |q, l| {
                // a literal (or document) operand is materialized before indexing
                let x = q.helper.mat_val(xc(q, l)?)?;
                let idx = ic(q, l)?;
                match &x {
                    Value::Arr(a) => {
                        let n = to_index(&idx)?;
                        let b = a.borrow();
                        if n < 0 || n as usize >= b.items.len() {
                            return err_code(format!("index {n} out of bounds"), "E5005");
                        }
                        Ok(b.items[n as usize].clone())
                    }
                    Value::Map(m) => match &idx {
                        Value::Str(k) => Ok(m.borrow().get(k).cloned().unwrap_or(Value::Absent)),
                        _ => Ok(Value::Absent),
                    },
                    Value::Rec(_) => match &idx {
                        Value::Str(k) => q.helper.access(&x, k),
                        _ => err("index on record needs a string"),
                    },
                    _ => err("index on non-collection"),
                }
            }))
        }
        Expr::Arr(items) => {
            let parts = items
                .iter()
                .map(|(sp, ex)| Ok((*sp, compile(ex, c)?)))
                .collect::<Result<Vec<(bool, Op)>, Unsupported>>()?;
            Ok(Rc::new(move |q, l| {
                let mut out: Vec<Value> = Vec::new();
                for (sp, op) in &parts {
                    let v = op(q, l)?;
                    if *sp {
                        for x in q.helper.iterate(&q.helper.mat_val(v)?)? {
                            out.push(x);
                        }
                    } else {
                        out.push(v);
                    }
                }
                Ok(Value::Arr(Rc::new(RefCell::new(ArrV {
                    items: out,
                    path: Rc::new(vec![]),
                }))))
            }))
        }
        Expr::Comp { head, clauses } => {
            let all_vars: Vec<String> = clauses.iter().map(|cl| cl.v.clone()).collect();
            let mut head_locals = c.locals.clone();
            for v in &all_vars {
                head_locals.insert(v.clone());
            }
            let head_op = compile(head, &with_locals(c, head_locals))?;
            let ccs = compile_clauses(clauses, &all_vars, c)?;
            Ok(Rc::new(move |q, l| {
                let mut out = Vec::new();
                run_comp(q, &ccs, 0, l, &head_op, &mut out)?;
                Ok(Value::Arr(Rc::new(RefCell::new(ArrV {
                    items: out,
                    path: Rc::new(vec![]),
                }))))
            }))
        }
        Expr::Obj(entries) => {
            // an object literal reaches compile only in a map-typed position (a
            // record-typed one is dispatched to bindRecord); build a map value,
            // which the type binder validates against `map<K, V>`. A spread entry
            // (§4.2) copies the entries of an object-valued expression in place.
            enum Part {
                Spread(Op),
                Kv(String, Op),
            }
            let mut parts: Vec<Part> = Vec::new();
            for (k, v) in entries {
                if let Expr::Spread(inner) = &**v {
                    parts.push(Part::Spread(compile(inner, c)?));
                } else {
                    parts.push(Part::Kv(k.clone(), compile(v, c)?));
                }
            }
            Ok(Rc::new(move |q, l| {
                let mut entries: Vec<(String, Value)> = Vec::new();
                fn put(entries: &mut Vec<(String, Value)>, k: String, v: Value) -> R<()> {
                    if entries.iter().any(|(ek, _)| *ek == k) {
                        return err_code(format!("duplicate key {k}"), "E5004");
                    }
                    entries.push((k, v));
                    Ok(())
                }
                for p in &parts {
                    match p {
                        Part::Spread(op) => {
                            let mut s = op(q, l)?;
                            if matches!(s, Value::Ref(_)) {
                                s = q.helper.deref(s)?;
                            }
                            for (k, v) in q.helper.spread_entries(s)? {
                                put(&mut entries, k, v)?;
                            }
                        }
                        Part::Kv(k, op) => put(&mut entries, k.clone(), op(q, l)?)?,
                    }
                }
                Ok(Value::Map(Rc::new(RefCell::new(MapV {
                    entries: entries.into_iter().collect(),
                    path: Rc::new(vec![]),
                }))))
            }))
        }
        Expr::MapComp { key, val, clauses } => {
            let all_vars: Vec<String> = clauses.iter().map(|cl| cl.v.clone()).collect();
            let mut scope = c.locals.clone();
            for v in &all_vars {
                scope.insert(v.clone());
            }
            let key_op = compile(key, &with_locals(c, scope.clone()))?;
            let val_op = compile(val, &with_locals(c, scope))?;
            let ccs = compile_clauses(clauses, &all_vars, c)?;
            Ok(Rc::new(move |q, l| {
                let mut out = Vec::new();
                run_mapcomp(q, &ccs, 0, l, &key_op, &val_op, &mut out)?;
                Ok(Value::Map(Rc::new(RefCell::new(MapV {
                    entries: out.into_iter().collect(),
                    path: Rc::new(vec![]),
                }))))
            }))
        }
        Expr::Template(parts) => {
            enum TP {
                Text(String),
                Op(Op),
            }
            let compiled = parts
                .iter()
                .map(|p| match p {
                    TPart::Text(s) => Ok(TP::Text(s.clone())),
                    TPart::Expr(e) => Ok(TP::Op(compile(e, c)?)),
                })
                .collect::<Result<Vec<TP>, Unsupported>>()?;
            Ok(Rc::new(move |q, l| {
                let mut s = String::new();
                for p in &compiled {
                    match p {
                        TP::Text(t) => s.push_str(t),
                        TP::Op(op) => s.push_str(&q.helper.to_str(&op(q, l)?)?),
                    }
                }
                Ok(Value::Str(s.into()))
            }))
        }
        Expr::Pattern(re) => {
            let re = re.clone();
            Ok(Rc::new(move |_q, _l| Ok(Value::Pat(re.clone()))))
        }
        Expr::Match { subject, arms } => {
            let subj = compile(subject, c)?;
            let mut marms: Vec<MArm> = Vec::new();
            for a in arms {
                let rt = compile_arm_type(a, c)?;
                let mut loc = c.locals.clone();
                loc.insert(a.v.clone());
                let body = compile(&a.body, &with_locals(c, loc))?;
                marms.push(MArm {
                    v: a.v.clone(),
                    rt,
                    body,
                });
            }
            Ok(Rc::new(move |q, l| {
                let mut subjv = subj(q, l)?;
                if matches!(subjv, Value::Ref(_)) {
                    subjv = q.helper.deref(subjv)?;
                }
                let sc = Scope::new("", Some(q.env.clone()));
                let mut catch: Option<&MArm> = None;
                for a in &marms {
                    match &a.rt {
                        None => catch = Some(a),
                        Some(rt) => {
                            if q.helper.member_of(&subjv, rt, &sc) {
                                let mut l2 = l.clone();
                                l2.insert(a.v.clone(), subjv.clone());
                                return (a.body)(q, &l2);
                            }
                        }
                    }
                }
                if let Some(a) = catch {
                    let mut l2 = l.clone();
                    l2.insert(a.v.clone(), subjv);
                    return (a.body)(q, &l2);
                }
                err("match: no arm matched")
            }))
        }
        Expr::Call { fun, args } => {
            let fn_op = compile_callee(fun, c)?;
            let arg_ops = args
                .iter()
                .map(|a| compile(a, c))
                .collect::<Result<Vec<Op>, Unsupported>>()?;
            Ok(Rc::new(move |q, l| {
                let f = fn_op(q, l)?;
                let mut argv = Vec::with_capacity(arg_ops.len());
                for op in &arg_ops {
                    argv.push(op(q, l)?);
                }
                let sc = Scope::new("", Some(q.env.clone()));
                q.helper.call(&f, argv, &sc)
            }))
        }
        Expr::Lambda { params, body } => {
            // a closure over the current locals; its body runs through the value
            // layer, which is pure over the closure's parameters and captures
            let params = params.clone();
            let body = body.clone();
            let menv = c.menv.clone();
            let root_name = c.root_name.clone();
            let self_inst = c.self_inst.clone();
            Ok(Rc::new(move |_q, l| {
                let scope = Scope {
                    inst: self_inst.clone(),
                    locals: crate::semantics::Locals::from_map(l),
                    root_name: root_name.clone(),
                    menv: Some(menv.clone()),
                };
                Ok(Value::Clo(Rc::new(crate::semantics::Closure {
                    params: params.clone(),
                    body: body.clone(),
                    scope,
                })))
            }))
        }
        Expr::With { .. } => {
            // `base with { patch }` (§4.2): the value layer reads the query-graph
            // base through the compute bridge and folds the patch in; the fresh
            // unbound result is materialized
            let with_expr = e.clone();
            let menv = c.menv.clone();
            let root_name = c.root_name.clone();
            let self_inst = c.self_inst.clone();
            Ok(Rc::new(move |q, l| {
                let scope = Scope {
                    inst: self_inst.clone(),
                    locals: crate::semantics::Locals::from_map(l),
                    root_name: root_name.clone(),
                    menv: Some(menv.clone()),
                };
                q.helper.mat_val(q.helper.ev(&with_expr, &scope)?)
            }))
        }
        Expr::Ctx(nm) => compile_ctx(nm, c),
        Expr::Referrers { ty, member } => {
            // §7.6: the references that point at this instance, answered from the
            // round's frozen universe by the value layer (Defer before phase 2)
            let ty = ty.clone();
            let member = member.clone();
            let self_inst = c.self_inst.clone();
            Ok(Rc::new(move |q, _l| {
                let sc = Scope::new("", Some(q.env.clone())).with_inst(self_inst.clone());
                q.helper.referrers(&ty, &member, &sc)
            }))
        }
        other => Err(Unsupported(expr_kind(other).to_string())),
    }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Lit(_) => "lit",
        Expr::UnitLit { .. } => "unitlit",
        Expr::Template(_) => "template",
        Expr::Name(_) => "name",
        Expr::Ctx(_) => "ctx",
        Expr::Referrers { .. } => "referrers",
        Expr::Obj(_) => "obj",
        Expr::Spread(_) => "spread",
        Expr::Arr(_) => "arr",
        Expr::Comp { .. } => "comp",
        Expr::MapComp { .. } => "mapcomp",
        Expr::Bin { .. } => "bin",
        Expr::Un { .. } => "un",
        Expr::Paren(_) => "paren",
        Expr::If { .. } => "if",
        Expr::Lambda { .. } => "lambda",
        Expr::Call { .. } => "call",
        Expr::Member { .. } => "member",
        Expr::Index { .. } => "index",
        Expr::With { .. } => "with",
        Expr::Pattern(_) => "pattern",
        Expr::Match { .. } => "match",
    }
}

/// the decl-layer evaluator over one round of a universe. Its records are built
/// into the shared `env` and forced through `helper` — the same value layer and
/// rounds machinery the tree walker uses; only slot computes differ (a bridge
/// into the query graph). One QEval per round; a round's frozen records outlive
/// it, so the caller keeps every round's QEval alive while any is a `prev`.
pub struct QEval {
    env: Rc<Env>,
    helper: Rc<Engine>,
    db: Db,
    weak: Weak<QEval>, // for bridge thunks and forceMember (query-graph records)
    const_ops: RefCell<FxHashMap<String, Op>>,
    // Round-owned compiled bodies. Slots hold only a weak arena reference and
    // an integer index: a body may capture its instance, so storing the body
    // on that instance would create a reference cycle.
    slot_ops: RefCell<Vec<Op>>,
}

impl QEval {
    /// a QEval over one round's fresh value-layer engine
    fn with_engine(env: Rc<Env>, helper: Rc<Engine>) -> Rc<QEval> {
        Rc::new_cyclic(|weak: &Weak<QEval>| {
            let w = weak.clone();
            let db = Db::new(Box::new(move |key: &str| {
                let q = w.upgrade().expect("qeval alive");
                let op = q.op_for(key)?;
                let w2 = w.clone();
                let key = key.to_string();
                let compute: crate::qengine::db::Compute = Box::new(move |_db: &Db| {
                    let q = w2.upgrade().expect("qeval alive");
                    q.helper
                        .step(&key, || op(&q, &Locals::default()))
                        .map_err(DbErr::Fail)
                });
                Some(compute)
            }));
            QEval {
                env,
                helper,
                db,
                weak: weak.clone(),
                const_ops: RefCell::new(FxHashMap::default()),
                slot_ops: RefCell::new(Vec::new()),
            }
        })
    }

    /// a thunk the value layer runs to force a query-graph slot (Compute::Bridge)
    fn bridge(&self, op: Op) -> Compute {
        let w = self.weak.clone();
        let index = self.slot_ops.borrow().len();
        self.slot_ops.borrow_mut().push(op);
        Compute::Bridge(Rc::new(move || {
            // The value-layer slot already memoizes its value, detects cycles,
            // and handles deferral/taint. Invoke its compiled body directly:
            // a second string-keyed memo would store the same value twice and
            // duplicate the dependency graph already held by the engine.
            let q = w.upgrade().expect("qeval alive");
            // Release the arena borrow before executing: binding a nested
            // record can append more bodies to the same arena.
            let op = q.slot_ops.borrow()[index].clone();
            op(&q, &Locals::default())
        }))
    }

    /// report a build-time member error to the shared env, so a rounds driver
    /// truncates it between rounds exactly as the tree walker's diagnostics.
    /// `by` groups it under its root for `$referrers` rounds but is not
    /// serialized (§12.2)
    fn diag(&self, message: String, path: Vec<Seg>, code: &str, root_name: &str) {
        self.env.report(Diag {
            severity: "error".into(),
            id: None,
            message,
            path: path_str(&path, None),
            code: Some(code.into()),
            loc: None,
            by: Some(format!("root:{root_name}")),
        });
    }

    /// Bind an object literal to a record type: a RecInst whose members are slot
    /// queries. A `ref<T>` member holds a navigation checked for reference
    /// integrity; a derived member may restate a supplied value; a record-typed
    /// member recurses; a scalar or conjunction member type-binds through the
    /// value layer. Slots carry a bridge compute so the value layer can force
    /// them; `force_all` fills them. Two passes so a member expression can
    /// resolve a sibling declared later (§4.3).
    fn bind_record(
        &self,
        entries: &[(String, Rc<Expr>)],
        rt: &RT,
        path: Vec<Seg>,
        parent: Option<Inst>,
        menv: &Rc<Env>,
    ) -> R<Value> {
        let root_name = seg_text(&path[0]);
        let supplied: FxHashMap<String, Rc<Expr>> = entries.iter().cloned().collect();
        let entry_order: Vec<String> = entries.iter().map(|(k, _)| k.clone()).collect();
        let inst: Inst = Rc::new(RefCell::new(RecInst {
            type_name: rt.name.borrow().clone(),
            rt: rt.clone(),
            path: Rc::new(path.clone()),
            ps: RefCell::new(None),
            parent,
            slots: Vec::new(),
            entry_order,
            extras: Vec::new(),
            menv: Some(menv.clone()),
        }));
        self.env.registry_push(inst.clone());
        let cctx = CCtx {
            self_inst: Some(inst.clone()),
            root_name: root_name.clone(),
            locals: FxHashSet::default(),
            menv: menv.clone(),
            entry_env: self.env.clone(),
        };
        let members = rec_members(rt);

        // pass 1: decide each slot's kind and initial state and declare it, so a
        // member expression compiled in pass 2 can resolve its sibling names;
        // record how each live slot's compute is built
        let mut plans: Vec<SlotPlan> = Vec::with_capacity(members.len());
        let mut slots: Vec<(String, Slot)> = Vec::with_capacity(members.len());
        let mut push_slot = |name: &str, kind: MKind, hidden: bool, state: SlotState| {
            slots.push((
                name.to_string(),
                Slot {
                    kind,
                    hidden,
                    state,
                    value: Value::Undef,
                    compute: None,
                },
            ));
        };
        for m in members.iter() {
            let mut member_path = path.clone();
            member_path.push(Seg::Name(Rc::from(m.name.as_str())));
            let has = supplied.get(&m.name).cloned();
            let types: Vec<RT> = m
                .conj
                .clone()
                .unwrap_or_else(|| m.ty.iter().cloned().collect());

            // a `ref<T>` member holds a navigation (§7.4), forced for integrity
            let is_ref = matches!(&m.ty, Some(t) if matches!(t.k, RTk::Ref(_))) && m.conj.is_none();
            if is_ref {
                let place = if m.kind == MKind::Der {
                    m.expr.clone()
                } else if has.is_some() {
                    has.clone()
                } else if m.kind == MKind::Dflt {
                    m.dflt.clone()
                } else {
                    None
                };
                match place {
                    None if m.kind == MKind::Opt => {
                        push_slot(&m.name, MKind::Opt, false, SlotState::Absent);
                        plans.push(SlotPlan::Skip);
                    }
                    None => {
                        self.diag(
                            format!("required member {} missing", m.name),
                            member_path,
                            "E4002",
                            &root_name,
                        );
                        push_slot(&m.name, MKind::Req, false, SlotState::Invalid);
                        plans.push(SlotPlan::Skip);
                    }
                    Some(pe) => {
                        push_slot(&m.name, m.kind, m.hidden, SlotState::Unforced);
                        plans.push(SlotPlan::Ref { place: pe });
                    }
                }
                continue;
            }

            // a derived member (§5.4): a hidden one may not be supplied; a
            // restated one must restate an identical value
            if m.kind == MKind::Der {
                if has.is_some() && m.hidden {
                    self.diag(
                        format!("hidden member {} supplied", m.name),
                        member_path,
                        "E4006",
                        &root_name,
                    );
                    push_slot(&m.name, MKind::Der, true, SlotState::Invalid);
                    plans.push(SlotPlan::Skip);
                    continue;
                }
                let member_rt = m.ty.clone();
                let rec_bearing = member_rt
                    .as_ref()
                    .map(|t| has_rec(t, &mut Vec::new()))
                    .unwrap_or(false);
                if has.is_some() && rec_bearing {
                    return Err(unsup("restated derived record"));
                }
                let expr = m
                    .expr
                    .clone()
                    .ok_or_else(|| unsup("derived without expr"))?;
                push_slot(&m.name, MKind::Der, m.hidden, SlotState::Unforced);
                plans.push(SlotPlan::Der {
                    expr,
                    ty: member_rt,
                    restate: has,
                    member_path,
                });
                continue;
            }

            // a supplied or defaulted value member; else absent (opt) or a
            // required-member error (req)
            if let Some(sv) = has {
                push_slot(&m.name, m.kind, m.hidden, SlotState::Unforced);
                plans.push(SlotPlan::Supply {
                    expr: sv,
                    types,
                    member_path,
                });
            } else if m.kind == MKind::Dflt {
                let d = m
                    .dflt
                    .clone()
                    .ok_or_else(|| unsup("default without expr"))?;
                push_slot(&m.name, MKind::Dflt, false, SlotState::Unforced);
                plans.push(SlotPlan::Supply {
                    expr: d,
                    types,
                    member_path,
                });
            } else if m.kind == MKind::Opt {
                push_slot(&m.name, MKind::Opt, false, SlotState::Absent);
                plans.push(SlotPlan::Skip);
            } else {
                self.diag(
                    format!("required member {} missing", m.name),
                    member_path,
                    "E4002",
                    &root_name,
                );
                push_slot(&m.name, MKind::Req, false, SlotState::Invalid);
                plans.push(SlotPlan::Skip);
            }
        }
        inst.borrow_mut().slots = slots;

        // pass 2: build each live slot's compute op and hand it a bridge
        for (m, plan) in members.iter().zip(plans) {
            let op = match self.build_plan(plan, &inst, &cctx, &root_name)? {
                Some(op) => op,
                None => continue,
            };
            inst.borrow_mut().slot_mut(&m.name).unwrap().compute = Some(self.bridge(op));
        }

        // supplied keys not declared by the type: an error on a closed record
        for (k, _) in entries {
            if members.iter().any(|m| &m.name == k) {
                continue;
            }
            if matches!(&rt.k, RTk::Rec(r) if r.open.get()) {
                return Err(unsup("open-record extras"));
            }
            let mut p = path.clone();
            p.push(Seg::Name(Rc::from(k.as_str())));
            self.diag(
                format!(
                    "undeclared member {k} on closed record{}",
                    rt.name
                        .borrow()
                        .as_deref()
                        .map(|n| format!(" {n}"))
                        .unwrap_or_default()
                ),
                p,
                "E4003",
                &root_name,
            );
        }
        Ok(Value::Rec(inst))
    }

    /// Build one slot's compiled body, or None for a slot with no compute.
    fn build_plan(
        &self,
        plan: SlotPlan,
        inst: &Inst,
        cctx: &CCtx,
        root_name: &str,
    ) -> R<Option<Op>> {
        let job: Op = match plan {
            SlotPlan::Skip => return Ok(None),
            SlotPlan::Ref { place } => {
                let place_op = compile_place(&place, cctx).map_err(|u| unsup(&u.0))?;
                let job: Op = Rc::new(move |q, l| {
                    let segs = place_op(q, l)?.ok_or_else(|| {
                        Fail::Eval(EvalErr {
                            msg: "not a place in ref position".into(),
                            code: None,
                        })
                    })?;
                    // reference integrity (§7.5): the place must hold a value
                    if q.helper.resolve_segs(&segs)?.is_undef() {
                        return Err(Fail::Eval(EvalErr {
                            msg: format!("dangling reference {}", path_str(&segs, None)),
                            code: Some("E6002".into()),
                        }));
                    }
                    Ok(Value::Ref(Rc::new(segs)))
                });
                job
            }
            SlotPlan::Der {
                expr,
                ty,
                restate,
                member_path,
            } => {
                let value_op = match &ty {
                    Some(t) => {
                        self.bind_value(&expr, t, member_path.clone(), Some(inst.clone()), cctx)
                    }
                    None => compile(&expr, cctx),
                }
                .map_err(|u| unsup(&u.0))?;
                let restate_op: Option<Op> = match &restate {
                    Some(sv) => Some(compile(sv, cctx).map_err(|u| unsup(&u.0))?),
                    None => None,
                };
                let name = seg_text(member_path.last().unwrap());
                let inst2 = inst.clone();
                let root2 = root_name.to_string();
                let mp = member_path;
                let job: Op = Rc::new(move |q, l| {
                    let v = value_op(q, l)?;
                    if let Some(rop) = &restate_op {
                        // a derived member also supplied restates identically (§5.4)
                        let raw_r = rop(q, l)?;
                        let restated = match &ty {
                            Some(t) => {
                                let sc = Scope::new(&root2, Some(q.env.clone()))
                                    .with_inst(Some(inst2.clone()));
                                q.helper.bind(raw_r, t, &mp, Some(&inst2), &sc)?
                            }
                            None => raw_r,
                        };
                        if !value_eq(&v, &restated) {
                            return Err(Fail::Eval(EvalErr {
                                msg: format!(
                                    "derived member {name} restated with a differing value"
                                ),
                                code: Some("E4005".into()),
                            }));
                        }
                    }
                    Ok(v)
                });
                job
            }
            SlotPlan::Supply {
                expr,
                types,
                member_path,
            } => {
                let job = self
                    .supply_produce(&expr, &types, member_path, inst.clone(), cctx)
                    .map_err(|u| unsup(&u.0))?;
                job
            }
        };
        Ok(Some(job))
    }

    /// build the compute for a supplied or defaulted member value. A single type
    /// binds through [`Self::bind_value`] (which keeps records in the query
    /// graph); a conjunction (§3.11) validates the raw value against each
    /// conjunct through the value layer.
    fn supply_produce(
        &self,
        val: &Rc<Expr>,
        types: &[RT],
        member_path: Vec<Seg>,
        inst: Inst,
        cctx: &CCtx,
    ) -> Result<Op, Unsupported> {
        if types.len() == 1 {
            return self.bind_value(val, &types[0], member_path, Some(inst), cctx);
        }
        let op = compile(val, cctx)?;
        let types = types.to_vec();
        let root_name = cctx.root_name.clone();
        let mp = member_path;
        Ok(Rc::new(move |q, l| {
            let raw = op(q, l)?;
            let mut v = raw.clone();
            let sc = Scope::new(&root_name, Some(q.env.clone())).with_inst(Some(inst.clone()));
            for ty in &types {
                v = q.helper.bind(raw.clone(), ty, &mp, Some(&inst), &sc)?;
            }
            Ok(v)
        }))
    }

    /// Bind a value expression to a type, keeping records in the query graph. A
    /// record literal recurses into [`Self::bind_record`]; a literal array or
    /// map whose elements contain records builds each element at its own path
    /// (so a nested record's slots stay query jobs, reachable by access and
    /// navigation); anything else compiles and type-binds through the value
    /// layer. Record-bearing unions and non-literal record-bearing collections
    /// are a later stage.
    fn bind_value(
        &self,
        val: &Rc<Expr>,
        ty: &RT,
        path: Vec<Seg>,
        parent: Option<Inst>,
        cctx: &CCtx,
    ) -> Result<Op, Unsupported> {
        // a record literal binds in the query graph; a record value from any
        // other expression is left to the value layer, forced at materialize
        if let (RTk::Rec(_), Expr::Obj(entries)) = (&ty.k, &**val) {
            let entries = entries.clone();
            let ty = ty.clone();
            let menv = cctx.menv.clone();
            return Ok(Rc::new(move |q, _l| {
                q.bind_record(&entries, &ty, path.clone(), parent.clone(), &menv)
            }));
        }
        // a literal array of record-bearing elements: build each at its own path
        if let (RTk::Arr { elem, lo, hi }, Expr::Arr(items)) = (&ty.k, &**val) {
            if has_rec(elem, &mut Vec::new()) && !items.iter().any(|(sp, _)| *sp) {
                let (lo, hi) = (*lo, *hi);
                let elem_ops = items
                    .iter()
                    .enumerate()
                    .map(|(i, (_, ex))| {
                        let mut p = path.clone();
                        p.push(Seg::Idx(i));
                        self.bind_value(ex, elem, p, parent.clone(), cctx)
                    })
                    .collect::<Result<Vec<Op>, Unsupported>>()?;
                let apath = path;
                return Ok(Rc::new(move |q, l| {
                    let mut items = Vec::with_capacity(elem_ops.len());
                    for op in &elem_ops {
                        items.push(op(q, l)?);
                    }
                    if let Some(lo) = lo {
                        let n = items.len() as i64;
                        let h = hi.unwrap_or(i64::MAX);
                        if n < lo || n > h {
                            return err(format!("array size {n} outside {lo}..{h}"));
                        }
                    }
                    Ok(Value::Arr(Rc::new(RefCell::new(ArrV {
                        items,
                        path: Rc::new(apath.clone()),
                    }))))
                }));
            }
        }
        // a literal map of record-bearing values: one query record per key
        if let (RTk::Map { val: vty, .. }, Expr::Obj(entries)) = (&ty.k, &**val) {
            let spread = entries.iter().any(|(_, v)| matches!(&**v, Expr::Spread(_)));
            if has_rec(vty, &mut Vec::new()) && !spread {
                let entry_ops = entries
                    .iter()
                    .map(|(k, ve)| {
                        let mut p = path.clone();
                        p.push(Seg::Key(Rc::from(k.as_str())));
                        Ok((
                            k.clone(),
                            self.bind_value(ve, vty, p, parent.clone(), cctx)?,
                        ))
                    })
                    .collect::<Result<Vec<(String, Op)>, Unsupported>>()?;
                let mpath = path;
                return Ok(Rc::new(move |q, l| {
                    let m = MapV {
                        entries: Default::default(),
                        path: Rc::new(mpath.clone()),
                    };
                    let m = Rc::new(RefCell::new(m));
                    for (k, op) in &entry_ops {
                        let v = op(q, l)?;
                        m.borrow_mut().set(k.clone(), v);
                    }
                    Ok(Value::Map(m))
                }));
            }
        }
        // otherwise compile + type-bind through the value layer
        let op = compile(val, cctx)?;
        let ty = ty.clone();
        let root_name = seg_text(&path[0]);
        Ok(Rc::new(move |q, l| {
            let raw = op(q, l)?;
            let sc = Scope::new(&root_name, Some(q.env.clone())).with_inst(parent.clone());
            q.helper.bind(raw, &ty, &path, parent.as_ref(), &sc)
        }))
    }

    /// the compute for a query key: a const (compiled lazily) for now
    fn op_for(&self, key: &str) -> Option<Op> {
        if let Some(name) = key.strip_prefix("const:") {
            if let Some(op) = self.const_ops.borrow().get(name) {
                return Some(op.clone());
            }
            let con = self.env.consts.borrow().get(name)?.clone();
            let c = CCtx {
                self_inst: None,
                root_name: String::new(),
                locals: FxHashSet::default(),
                menv: self.env.clone(),
                entry_env: self.env.clone(),
            };
            let op = compile(&con.expr, &c).ok()?;
            self.const_ops
                .borrow_mut()
                .insert(name.to_string(), op.clone());
            return Some(op);
        }
        None
    }

    /// read a query's value, translating the core's cycle into an E5007 error
    fn query(&self, key: &str) -> R<Value> {
        self.helper.record(key.to_string());
        match self.db.query(key) {
            Ok(v) => Ok(v),
            Err(DbErr::Fail(f)) => Err(f),
            Err(DbErr::Cycle(cyc)) => {
                let end = |k: &str| member_of_slot_key(k).to_string();
                let (a, b) = (end(&cyc[0]), end(&cyc[cyc.len() - 1]));
                err_code(format!("dependency cycle: {a} -> {b}"), "E5007")
            }
        }
    }

    /// Bind every output root for one round into the shared env. A record-bearing
    /// root written as a literal (`{ … }` or `[ … ]`) builds through the query
    /// graph — its slots defer to later rounds as `$referrers` needs; any other
    /// root (a scalar, or a record from a call/comprehension/reference) binds
    /// through the value layer, which handles its own deferral across rounds. An
    /// unhandled form trips `unsupported`, so the caller falls back.
    fn bind_roots(self: &Rc<Self>, roots: &[RootSpec], unsupported: &Cell<bool>) {
        for (name, tyast, expr, menv) in roots {
            if !self.helper.should_bind_root(name) {
                continue;
            }
            let rt = match menv.resolve(tyast, None) {
                Ok(rt) => rt,
                Err(_) => continue, // a type that does not resolve: the checker's domain
            };
            let literal = matches!(&**expr, Expr::Obj(_) | Expr::Arr(_));
            if has_rec(&rt, &mut Vec::new()) && literal {
                let c = CCtx {
                    self_inst: None,
                    root_name: name.clone(),
                    locals: FxHashSet::default(),
                    menv: menv.clone(),
                    entry_env: self.env.clone(),
                };
                let path = vec![Seg::Name(name.as_str().into())];
                match self.bind_value(expr, &rt, path, None, &c) {
                    Ok(op) => match self
                        .helper
                        .step(&format!("root:{name}"), || op(self, &Locals::default()))
                    {
                        Ok(v) => self.env.set_root(name, v),
                        Err(Fail::Eval(e)) if e.code.as_deref() == Some(QUNSUP) => {
                            unsupported.set(true)
                        }
                        Err(Fail::Eval(e)) => self.env.report(Diag {
                            severity: "error".into(),
                            id: None,
                            message: e.msg,
                            path: name.clone(),
                            code: e.code,
                            loc: None,
                            by: None,
                        }),
                        Err(Fail::Defer) => self.helper.deferred_roots.borrow_mut().push(
                            crate::engine::DeferredRoot {
                                name: name.clone(),
                                src: crate::engine::OwnedRootSrc::Expr(expr.clone()),
                                rt: rt.clone(),
                                sc: Scope::new(name, Some(menv.clone())),
                            },
                        ),
                        Err(_) => {} // Taint: the value layer already reported it
                    },
                    Err(_) => unsupported.set(true),
                }
            } else {
                // the value layer binds a scalar or non-literal root, deferring
                // a `$referrers`-dependent one to a later round (§7.6, §9.4)
                let sc = Scope::new(name, Some(menv.clone()));
                self.helper.bind_root(name, RootSrc::Expr(expr), &rt, &sc);
            }
        }
    }
}

/// one evaluation root: its name, declared type, expression, and module scope
type RootSpec = (String, TypeAst, Rc<Expr>, Rc<Env>);

/// the trailing member name of a slot key (`slot:hub.ports["a"].sel$` -> `sel$`)
fn member_of_slot_key(key: &str) -> &str {
    if let Some(i) = key.rfind('.') {
        &key[i + 1..]
    } else {
        key
    }
}

/// Evaluate a single module's outputs (its own env is the whole universe). The
/// universe is evaluated in rounds by the value layer's driver (`Engine::
/// evaluate`, §7.6): each round runs a fresh QEval that binds the roots into the
/// shared env and forces them, answering `$referrers` from the previous round's
/// frozen universe, until the queried edges settle (E5009 if they never do).
/// The query-graph records a round builds outlive it (their slots bridge back to
/// their QEval), so every round's QEval is kept alive until the driver returns.
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
/// bound inputs and roots through a fresh QEval each round, forcing through the
/// value layer's rounds machinery until `$referrers` settles. `hook_mods` are
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
    let holder: RefCell<Vec<Rc<QEval>>> = RefCell::new(Vec::new());
    let unsupported = Cell::new(false);
    let bind = |eng: &Rc<Engine>| {
        let ev = QEval::with_engine(entry.clone(), eng.clone());
        holder.borrow_mut().push(ev.clone());
        let weak = Rc::downgrade(&ev);
        eng.round_resets.borrow_mut().push(Rc::new(move || {
            if let Some(ev) = weak.upgrade() {
                ev.db.clear();
            }
        }));
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
        ev.bind_roots(roots, &unsupported);
    };
    let eng = Engine::evaluate_query(entry, &bind);
    eng.validate_all("");
    let diags = sort_diags(entry.diagnostics_vec()); // §6.7
    entry.diag_set(diags.clone());
    // a form the query engine does not handle (at bind or at force time) is
    // signalled by the flag or the sentinel diagnostic: the caller falls back
    if unsupported.get() || diags.iter().any(|d| d.code.as_deref() == Some(QUNSUP)) {
        return Err(Unsupported("form".into()));
    }
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
