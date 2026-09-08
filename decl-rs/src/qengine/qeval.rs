//! The decl layer of the query engine (qengine/DESIGN.md), the port of
//! decl-ts/src/qengine/qeval.ts: compile expressions to query computes and
//! evaluate a module's outputs through the incremental core (db.rs). Value
//! semantics — equality, serialization, type binding, arithmetic — are reused
//! from the value layer (`Engine`); only the evaluation strategy is new. Built
//! up stage by stage; a form not yet compiled raises [`Unsupported`].
use crate::ast::{Expr, TypeAst};
use crate::engine::{num_cmp, Engine};
use crate::qengine::db::{Db, DbErr};
use crate::semantics::{
    err_code, path_str, value_eq, Diag, Env, EvalErr, Fail, Scope, Seg, Value, R,
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// an expression or member form this stage of the port does not compile yet
pub struct Unsupported(pub String);

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
    #[allow(dead_code)] // consumed by later stages (refs, $this/$root, ctx)
    root_name: String,
    locals: FxHashSet<String>, // comprehension loop variables in scope
    menv: Rc<Env>,             // the module scope (consts, funcs, unit table)
    entry_env: Rc<Env>,        // the universe entry (its consts are query-native)
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
                _ => Ok(Rc::new(move |q, l| apply_bin(&op, &lc(q, l)?, &rc(q, l)?))),
            }
        }
        Expr::Name(name) => {
            let nm = name.clone();
            if c.locals.contains(&nm) {
                return Ok(Rc::new(move |_q, l| {
                    Ok(l.get(&nm).cloned().unwrap_or(Value::Undef))
                }));
            }
            // an entry-module const is query-native (memoized here)
            if c.menv.consts.borrow().contains_key(&nm) && Rc::ptr_eq(&c.menv, &c.entry_env) {
                let key = format!("const:{nm}");
                return Ok(Rc::new(move |q, _l| q.query(&key)));
            }
            Err(Unsupported(format!("name {nm}")))
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

/// the decl-layer evaluator over one universe (a module, or a whole universe)
pub struct QEval {
    env: Rc<Env>,
    helper: Rc<Engine>,
    db: Db,
    const_ops: RefCell<FxHashMap<String, Op>>,
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
                const_ops: RefCell::new(FxHashMap::default()),
                diagnostics: RefCell::new(Vec::new()),
            }
        })
    }

    /// the compute for a query key: a const (compiled lazily) for now
    fn op_for(&self, key: &str) -> Option<Op> {
        if let Some(name) = key.strip_prefix("const:") {
            if let Some(op) = self.const_ops.borrow().get(name) {
                return Some(op.clone());
            }
            let con = self.env.consts.borrow().get(name)?.clone();
            let c = CCtx {
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
        let mut outputs: Vec<(String, String)> = Vec::new();
        for (name, tyast, expr) in roots {
            let rt = match self.env.resolve(tyast, None) {
                Ok(rt) => rt,
                Err(_) => continue, // a type that does not resolve: the checker's domain
            };
            let c = CCtx {
                root_name: name.clone(),
                locals: FxHashSet::default(),
                menv: self.env.clone(),
                entry_env: self.env.clone(),
            };
            let op = match compile(expr, &c) {
                Ok(op) => op,
                Err(u) => {
                    // an unhandled form: skip the whole evaluation (the caller
                    // falls back to the tree walker)
                    self.diagnostics.borrow_mut().push(Diag::error(
                        format!("qeval unsupported: {}", u.0),
                        name.clone(),
                        Some("__QUNSUP__"),
                    ));
                    return QReport {
                        ok: false,
                        outputs: Vec::new(),
                        diagnostics: vec![],
                    };
                }
            };
            let sc = Scope::new(name, Some(self.env.clone()));
            let path = [Seg::Name(name.as_str().into())];
            match op(self, &Locals::default())
                .and_then(|raw| self.helper.bind(raw, &rt, &path, None, &sc))
            {
                Ok(v) => {
                    self.env.set_root(name, v.clone());
                    outputs.push((name.clone(), self.helper.serialize(&v, name, false)));
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
        let diags = self.diagnostics.borrow().clone();
        let ok = !diags.iter().any(|d| d.severity == "error");
        QReport {
            ok,
            outputs: if ok { outputs } else { Vec::new() },
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

// silence unused-field warnings until later stages consume them
#[allow(dead_code)]
fn _uses(q: &QEval) {
    let _ = &q.db;
    let _ = path_str(&[], None);
}
