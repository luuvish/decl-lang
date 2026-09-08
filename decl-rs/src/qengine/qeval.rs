//! The decl layer of the query engine (qengine/DESIGN.md), the port of
//! decl-ts/src/qengine/qeval.ts: compile expressions to query computes and
//! evaluate a module's outputs through the incremental core (db.rs). Value
//! semantics — equality, serialization, type binding, arithmetic — are reused
//! from the value layer (`Engine`); only the evaluation strategy is new. Built
//! up stage by stage; a form not yet compiled raises [`Unsupported`].
use crate::ast::{Expr, ForClause, MatchArm, TPart, TypeAst};
use crate::engine::{num_cmp, to_index, Engine, Inst};
use crate::qengine::db::{Db, DbErr};
use crate::semantics::{
    compile_pattern, err, err_code, path_str, pattern_error, rec_members, seg_text, sort_diags,
    value_eq, ArrV, Compute, Diag, Env, EvalErr, Fail, MKind, MapV, Num, RTk, RecInst, Scope, Seg,
    Slot, SlotState, Value, R, RT,
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
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
            Err(Unsupported(format!("name {nm}")))
        }
        Expr::Member { x, name, safe } => {
            let nm = name.clone();
            let safe = *safe;
            let xc = compile(x, c)?;
            Ok(Rc::new(move |q, l| {
                let mut xv = xc(q, l)?;
                if safe && matches!(xv, Value::Null | Value::Absent) {
                    return Ok(Value::Absent);
                }
                if matches!(xv, Value::Ref(_)) {
                    xv = q.helper.deref(xv)?;
                }
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
                    entries,
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
                    entries: out,
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
        Expr::Ctx(nm) => compile_ctx(nm, c),
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

/// the decl-layer evaluator over one universe (a module, or a whole universe)
pub struct QEval {
    env: Rc<Env>,
    helper: Rc<Engine>,
    db: Db,
    weak: Weak<QEval>, // for bridge thunks and forceMember (query-graph records)
    const_ops: RefCell<FxHashMap<String, Op>>,
    slot_jobs: RefCell<FxHashMap<String, Op>>, // a record slot's compiled compute
    diagnostics: RefCell<Vec<Diag>>,
}

impl QEval {
    fn new(env: Rc<Env>) -> Rc<QEval> {
        Rc::new_cyclic(|weak: &Weak<QEval>| {
            let helper = Engine::new(env.clone());
            let w = weak.clone();
            let db = Db::new(Box::new(move |key: &str| {
                let q = w.upgrade().expect("qeval alive");
                let op = q.op_for(key)?;
                let w2 = w.clone();
                let compute: crate::qengine::db::Compute = Box::new(move |_db: &Db| {
                    let q = w2.upgrade().expect("qeval alive");
                    op(&q, &Locals::default()).map_err(DbErr::Fail)
                });
                Some(compute)
            }));
            QEval {
                env,
                helper,
                db,
                weak: weak.clone(),
                const_ops: RefCell::new(FxHashMap::default()),
                slot_jobs: RefCell::new(FxHashMap::default()),
                diagnostics: RefCell::new(Vec::new()),
            }
        })
    }

    /// a thunk the value layer runs to force a query-graph slot (Compute::Bridge)
    fn bridge(&self, key: String) -> Compute {
        let w = self.weak.clone();
        Compute::Bridge(Rc::new(move || {
            w.upgrade().expect("qeval alive").query(&key)
        }))
    }

    /// Bind an object literal to a record type: a RecInst whose members are slot
    /// queries. A record-typed member recurses; a scalar/other member compiles
    /// and type-binds. Slots carry a bridge compute so the value layer can force
    /// them; `materialize`/`force_all` fills them.
    fn bind_record(
        &self,
        entries: &[(String, Rc<Expr>)],
        rt: &RT,
        path: Vec<Seg>,
        parent: Option<Inst>,
    ) -> R<Value> {
        let inst_id = path_str(&path, None);
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
            menv: Some(self.env.clone()),
        }));
        self.env.registry_push(inst.clone());
        let cctx = CCtx {
            self_inst: Some(inst.clone()),
            root_name: root_name.clone(),
            locals: FxHashSet::default(),
            menv: self.env.clone(),
            entry_env: self.env.clone(),
        };
        let members = rec_members(rt);
        // pass 1: declare every slot first, so a member expression compiled in
        // pass 2 can resolve its sibling names (has_slot). The value expression
        // (with its type, and the member path) is planned per live member.
        type Plan = Option<(String, Rc<Expr>, Option<RT>, Vec<Seg>)>;
        let mut plan: Vec<Plan> = Vec::new();
        {
            let mut slots: Vec<(String, Slot)> = Vec::new();
            for m in &members {
                let mut member_path = path.clone();
                member_path.push(Seg::Name(Rc::from(m.name.as_str())));
                let key = format!("slot:{inst_id}.{}", m.name);
                let has = supplied.contains_key(&m.name);
                let ve: Option<(Rc<Expr>, Option<RT>)> = if m.kind == MKind::Der {
                    Some((
                        m.expr
                            .clone()
                            .ok_or_else(|| unsup("derived without expr"))?,
                        m.ty.clone(),
                    ))
                } else if has {
                    let t = m.ty.clone().ok_or_else(|| unsup("member without type"))?;
                    Some((supplied.get(&m.name).unwrap().clone(), Some(t)))
                } else if m.kind == MKind::Dflt {
                    let t = m.ty.clone().ok_or_else(|| unsup("member without type"))?;
                    Some((
                        m.dflt
                            .clone()
                            .ok_or_else(|| unsup("default without expr"))?,
                        Some(t),
                    ))
                } else {
                    None
                };
                let state = match (&ve, m.kind) {
                    (Some(_), _) => SlotState::Unforced,
                    (None, MKind::Opt) => SlotState::Absent,
                    (None, _) => {
                        self.diagnostics.borrow_mut().push(Diag {
                            severity: "error".into(),
                            id: None,
                            message: format!("required member {} missing", m.name),
                            path: path_str(&member_path, None),
                            code: Some("E4002".into()),
                            loc: None,
                            by: Some(format!("root:{root_name}")),
                        });
                        SlotState::Invalid
                    }
                };
                plan.push(ve.map(|(e, t)| (key, e, t, member_path)));
                slots.push((
                    m.name.clone(),
                    Slot {
                        kind: m.kind,
                        hidden: m.hidden,
                        state,
                        value: Value::Undef,
                        compute: None,
                    },
                ));
            }
            inst.borrow_mut().slots = slots;
        }
        // pass 2: compile each live member's value op and give the slot a bridge
        for (m, entry) in members.iter().zip(plan) {
            if let Some((key, expr, ty, member_path)) = entry {
                let op = match &ty {
                    Some(t) => self.bind_value(&expr, t, member_path, Some(inst.clone()), &cctx),
                    None => compile(&expr, &cctx),
                }
                .map_err(|u| unsup(&u.0))?;
                self.slot_jobs.borrow_mut().insert(key.clone(), op);
                inst.borrow_mut().slot_mut(&m.name).unwrap().compute = Some(self.bridge(key));
            }
        }
        // supplied keys not declared by the type: an error on a closed record
        for (k, _) in entries {
            if members.iter().any(|m| &m.name == k) {
                continue;
            }
            if matches!(&rt.k, crate::semantics::RTk::Rec(r) if r.open.get()) {
                return Err(unsup("open-record extras"));
            }
            self.diagnostics.borrow_mut().push(Diag {
                severity: "error".into(),
                id: None,
                message: format!(
                    "undeclared member {k} on closed record{}",
                    rt.name
                        .borrow()
                        .as_deref()
                        .map(|n| format!(" {n}"))
                        .unwrap_or_default()
                ),
                path: path_str(
                    &{
                        let mut p = path.clone();
                        p.push(Seg::Name(Rc::from(k.as_str())));
                        p
                    },
                    None,
                ),
                code: Some("E4003".into()),
                by: Some(format!("root:{root_name}")),
                loc: None,
            });
        }
        Ok(Value::Rec(inst))
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
            return Ok(Rc::new(move |q, _l| {
                q.bind_record(&entries, &ty, path.clone(), parent.clone())
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
                        entries: Vec::new(),
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
        if key.starts_with("slot:") {
            return self.slot_jobs.borrow().get(key).cloned();
        }
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

    fn run(self: &Rc<Self>, roots: &[(String, TypeAst, Rc<Expr>)]) -> QReport {
        // bind every root, then force the whole universe, then validate (§9.3)
        let mut built: Vec<(String, Value)> = Vec::new();
        for (name, tyast, expr) in roots {
            let rt = match self.env.resolve(tyast, None) {
                Ok(rt) => rt,
                Err(_) => continue, // a type that does not resolve: the checker's domain
            };
            let c = CCtx {
                self_inst: None,
                root_name: name.clone(),
                locals: FxHashSet::default(),
                menv: self.env.clone(),
                entry_env: self.env.clone(),
            };
            let path = vec![Seg::Name(name.as_str().into())];
            let op = match self.bind_value(expr, &rt, path, None, &c) {
                Ok(op) => op,
                Err(u) => {
                    self.diagnostics.borrow_mut().push(Diag::error(
                        format!("qeval unsupported: {}", u.0),
                        name.clone(),
                        Some(QUNSUP),
                    ));
                    return self.report();
                }
            };
            match op(self, &Locals::default()) {
                Ok(v) => {
                    self.env.set_root(name, v.clone());
                    built.push((name.clone(), v));
                }
                Err(Fail::Eval(e)) => self.diagnostics.borrow_mut().push(Diag {
                    severity: "error".into(),
                    id: None,
                    message: e.msg,
                    path: name.clone(),
                    code: e.code,
                    loc: None,
                    by: None,
                }),
                Err(_) => {} // Taint: the value layer already reported it
            }
        }
        // force every slot of the whole universe (query-graph slots via the
        // bridge; value-layer records natively), then validate assertions (§6)
        for (_, v) in &built {
            self.helper.force_all(v);
        }
        self.helper.validate_all("");
        let mut outputs: Vec<(String, String)> = Vec::new();
        for (name, v) in &built {
            outputs.push((name.clone(), self.helper.serialize(v, name, false)));
        }
        let report = self.report();
        QReport {
            ok: report.ok,
            outputs: if report.ok { outputs } else { Vec::new() },
            diagnostics: report.diagnostics,
        }
    }

    /// combine this run's own diagnostics with the value layer's, sort them
    /// (§6.7), and decide `ok`
    fn report(&self) -> QReport {
        let mut diags = self.diagnostics.borrow().clone();
        diags.extend(self.env.diagnostics_vec());
        let diags = sort_diags(diags);
        let ok = !diags.iter().any(|d| d.severity == "error");
        QReport {
            ok,
            outputs: Vec::new(),
            diagnostics: diags,
        }
    }
}

/// the trailing member name of a slot key (`slot:hub.ports["a"].sel$` -> `sel$`)
fn member_of_slot_key(key: &str) -> &str {
    if let Some(i) = key.rfind('.') {
        &key[i + 1..]
    } else {
        key
    }
}

/// evaluate a single module's outputs
pub fn qevaluate(env: Rc<Env>) -> Result<QReport, Unsupported> {
    let roots: Vec<(String, TypeAst, Rc<Expr>)> = env.outputs.borrow().clone();
    let ev = QEval::new(env);
    let report = ev.run(&roots);
    // a compile-time unsupported form is signalled by the sentinel diagnostic
    if report
        .diagnostics
        .iter()
        .any(|d| d.code.as_deref() == Some("__QUNSUP__"))
    {
        return Err(Unsupported("form".into()));
    }
    Ok(report)
}
