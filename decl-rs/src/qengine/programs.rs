//! Executable expressions shared by syntax identity. Operations receive their
//! engine and lexical frame at execution time; none capture a record instance.
use crate::ast::*;
use crate::engine::{to_index, Engine};
use crate::semantics::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// One shared expression, supplied with an engine and runtime scope.
pub type Op = Rc<dyn Fn(&Engine, &Scope) -> R<Value>>;
type Cache = RefCell<FxHashMap<usize, (Rc<Expr>, Op)>>;

pub(crate) struct MemberPlan {
    pub types: Rc<Vec<RT>>,
    pub expression: Option<Op>,
    pub fallback: Option<Op>,
    pub deferred: bool,
}

pub(crate) struct Schema {
    source: Rc<Vec<Member>>,
    pub members: Vec<MemberPlan>,
    pub names: FxHashSet<String>,
}

/// Executable expressions and schema plans shared for one module universe.
#[derive(Default)]
pub struct Programs {
    expressions: Cache,
    callees: Cache,
    navigations: Cache,
    schemas: RefCell<FxHashMap<usize, (RT, Rc<Schema>)>>,
    /// Number of distinct expression programs compiled.
    pub compiled: Cell<usize>,
    /// Number of distinct schema plans compiled.
    pub compiled_schemas: Cell<usize>,
}

impl Programs {
    pub(crate) fn schema(&self, rt: &RT) -> Rc<Schema> {
        let id = Rc::as_ptr(rt) as usize;
        let source = rec_members(rt);
        if let Some((_, hit)) = self.schemas.borrow().get(&id) {
            if Rc::ptr_eq(&hit.source, &source) {
                return hit.clone();
            }
        }
        let members = source
            .iter()
            .map(|m| MemberPlan {
                types: Rc::new(
                    m.conj
                        .clone()
                        .unwrap_or_else(|| m.ty.iter().cloned().collect()),
                ),
                expression: m.expr.as_ref().map(|e| self.get(e)),
                fallback: m.dflt.as_ref().map(|e| self.get(e)),
                deferred: m.expr.as_ref().is_some_and(|e| mentions_referrers(e)),
            })
            .collect();
        let names = source.iter().map(|m| m.name.clone()).collect();
        let schema = Rc::new(Schema {
            source,
            members,
            names,
        });
        self.schemas
            .borrow_mut()
            .insert(id, (rt.clone(), schema.clone()));
        self.compiled_schemas.set(self.compiled_schemas.get() + 1);
        schema
    }

    pub(crate) fn get(&self, e: &Rc<Expr>) -> Op {
        let id = Rc::as_ptr(e) as usize;
        if let Some((_, op)) = self.expressions.borrow().get(&id) {
            return op.clone();
        }
        let op = self.compile(e);
        self.expressions
            .borrow_mut()
            .insert(id, (e.clone(), op.clone()));
        self.compiled.set(self.compiled.get() + 1);
        op
    }

    pub(crate) fn callee(&self, e: &Rc<Expr>) -> Op {
        let id = Rc::as_ptr(e) as usize;
        if let Some((_, op)) = self.callees.borrow().get(&id) {
            return op.clone();
        }
        let op: Op = if let Expr::Member { x, name, .. } = &**e {
            let base = self.callee(x);
            let name = name.clone();
            Rc::new(move |eng, sc| {
                let x = base(eng, sc)?;
                match &x {
                    Value::Std(p) => {
                        let mut p = (**p).clone();
                        p.push(name.clone());
                        Ok(Value::Std(Rc::new(p)))
                    }
                    Value::NsRef(ns) => eng.ns_value(ns, &name, sc),
                    _ => eng.access(&eng.deref(x)?, &name),
                }
            })
        } else {
            self.get(e)
        };
        self.callees
            .borrow_mut()
            .insert(id, (e.clone(), op.clone()));
        op
    }

    pub(crate) fn nav(&self, e: &Rc<Expr>) -> Op {
        let id = Rc::as_ptr(e) as usize;
        if let Some((_, op)) = self.navigations.borrow().get(&id) {
            return op.clone();
        }
        let op: Op = match &**e {
            Expr::Paren(x) => self.nav(x),
            Expr::If { c, t, f } => {
                let (c, t, f) = (self.get(c), self.nav(t), self.nav(f));
                Rc::new(move |eng, sc| {
                    if eng.truthy(&c(eng, sc)?)? {
                        t(eng, sc)
                    } else {
                        f(eng, sc)
                    }
                })
            }
            Expr::Member { x, name, .. } => {
                let (base, name) = (self.nav(x), name.clone());
                Rc::new(move |eng, sc| {
                    let raw = base(eng, sc)?;
                    if let Value::Segs(p) = raw {
                        let mut p = (*p).clone();
                        p.push(Seg::Name(name.as_str().into()));
                        return Ok(Value::Segs(Rc::new(p)));
                    }
                    let x = eng.deref(raw)?;
                    let v = eng.access(&x, &name)?;
                    if v.is_absent() {
                        if let Value::Rec(r) = x {
                            let mut p = r.borrow().path.to_vec();
                            p.push(Seg::Name(name.as_str().into()));
                            return Ok(Value::Segs(Rc::new(p)));
                        }
                    }
                    Ok(v)
                })
            }
            Expr::Index { x, i } => {
                let (base, index, ordinary) = (self.nav(x), self.get(i), self.get(e));
                Rc::new(move |eng, sc| {
                    let raw = base(eng, sc)?;
                    let i = index(eng, sc)?;
                    if let Value::Segs(p) = raw {
                        let mut p = (*p).clone();
                        p.push(match &i {
                            Value::Str(k) => Seg::Key(k.clone()),
                            _ => Seg::Idx(to_index(&i)?.max(0) as usize),
                        });
                        return Ok(Value::Segs(Rc::new(p)));
                    }
                    let x = eng.deref(raw)?;
                    match &x {
                        Value::Arr(a) => {
                            let n = to_index(&i)?;
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
                            let Value::Str(k) = &i else {
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
                            let Value::Str(k) = &i else {
                                return err("index on record needs a string");
                            };
                            let v = eng.access(&x, k)?;
                            if v.is_absent() {
                                let mut p = r.borrow().path.to_vec();
                                p.push(Seg::Name(k.clone()));
                                Ok(Value::Segs(Rc::new(p)))
                            } else {
                                Ok(v)
                            }
                        }
                        _ => ordinary(eng, sc),
                    }
                })
            }
            _ => self.get(e),
        };
        self.navigations
            .borrow_mut()
            .insert(id, (e.clone(), op.clone()));
        op
    }

    fn compile(&self, e: &Rc<Expr>) -> Op {
        match &**e {
            Expr::Lit(v) => {
                let v = v.clone();
                Rc::new(move |_, _| Ok(v.clone()))
            }
            Expr::Pattern(s) => {
                let s = s.clone();
                Rc::new(move |_, _| Ok(Value::Pat(s.clone())))
            }
            Expr::UnitLit { num, unit } => {
                let (num, unit) = (*num, unit.clone());
                Rc::new(move |eng, _| {
                    let (key, to_base) = eng.env.unit_info(&unit).or_else(err)?;
                    Ok(Value::Q {
                        dim: key,
                        value: num * to_base,
                    })
                })
            }
            Expr::Paren(x) => self.get(x),
            Expr::Name(n) => {
                let n = n.clone();
                Rc::new(move |eng, sc| eng.name_value(&n, sc))
            }
            Expr::Ctx(n) => {
                let n = n.clone();
                Rc::new(move |eng, sc| eng.context_value(&n, sc))
            }
            Expr::Referrers { ty, member } => {
                let (ty, member) = (ty.clone(), member.clone());
                Rc::new(move |eng, sc| eng.referrers(&ty, &member, sc))
            }
            Expr::Spread(_) => Rc::new(|_, _| err("spread outside an object literal")),
            Expr::Obj(entries) => {
                let entries = entries.clone();
                Rc::new(move |_, sc| {
                    Ok(Value::PreObj(Rc::new(
                        entries
                            .iter()
                            .map(|(k, e)| (k.clone(), pre(e, sc)))
                            .collect(),
                    )))
                })
            }
            Expr::Arr(items) => {
                let items = items.clone();
                Rc::new(move |_, sc| {
                    Ok(Value::PreArr(Rc::new(
                        items.iter().map(|(sp, e)| (*sp, pre(e, sc))).collect(),
                    )))
                })
            }
            Expr::Template(parts) => {
                let parts: Vec<_> = parts
                    .iter()
                    .map(|p| match p {
                        TPart::Text(s) => Ok(s.clone()),
                        TPart::Expr(e) => Err(self.get(e)),
                    })
                    .collect();
                Rc::new(move |eng, sc| {
                    let mut out = String::new();
                    for p in &parts {
                        match p {
                            Ok(s) => out.push_str(s),
                            Err(op) => out.push_str(&eng.to_str(&op(eng, sc)?)?),
                        }
                    }
                    Ok(Value::Str(out.into()))
                })
            }
            Expr::If { c, t, f } => {
                let (c, t, f) = (self.get(c), self.get(t), self.get(f));
                Rc::new(move |eng, sc| {
                    if eng.truthy(&c(eng, sc)?)? {
                        t(eng, sc)
                    } else {
                        f(eng, sc)
                    }
                })
            }
            Expr::Un { op, x } => {
                let (op, x) = (op.clone(), self.get(x));
                Rc::new(move |eng, sc| {
                    let x = x(eng, sc)?;
                    match op.as_str() {
                        "!" => Ok(Value::Bool(!eng.truthy(&x)?)),
                        "-" => match x {
                            Value::Absent => err("absent consumed"),
                            Value::Q { dim, value } => Ok(Value::Q { dim, value: -value }),
                            Value::Int(i) => Ok(Value::Int(-i)),
                            Value::Float(f) => Ok(Value::Float(-f)),
                            _ => err("bad operand for unary -"),
                        },
                        "~" => match x {
                            Value::Int(i) => Ok(Value::Int(&(-i) - 1)),
                            _ => err("bad operand for ~"),
                        },
                        _ => err("un"),
                    }
                })
            }
            Expr::Bin { op, l, r } => {
                if op == "|>" {
                    let (fun, mut args) = match &**r {
                        Expr::Call { fun, args } => (fun.clone(), args.clone()),
                        _ => (r.clone(), vec![]),
                    };
                    args.insert(0, l.clone());
                    return self.get(&Rc::new(Expr::Call { fun, args }));
                }
                let (op, l, r) = (op.clone(), self.get(l), self.get(r));
                match op.as_str() {
                    "&&" => Rc::new(move |eng, sc| {
                        Ok(Value::Bool(
                            eng.truthy(&l(eng, sc)?)? && eng.truthy(&r(eng, sc)?)?,
                        ))
                    }),
                    "||" => Rc::new(move |eng, sc| {
                        Ok(Value::Bool(
                            eng.truthy(&l(eng, sc)?)? || eng.truthy(&r(eng, sc)?)?,
                        ))
                    }),
                    "??" => Rc::new(move |eng, sc| {
                        let v = l(eng, sc)?;
                        if matches!(v, Value::Absent | Value::Null) {
                            r(eng, sc)
                        } else {
                            Ok(v)
                        }
                    }),
                    _ => Rc::new(move |eng, sc| eng.apply_bin(&op, l(eng, sc)?, r(eng, sc)?)),
                }
            }
            Expr::Member { x, name, safe } => {
                let (base, name, safe) = (self.get(x), name.clone(), *safe);
                Rc::new(move |eng, sc| {
                    let x = base(eng, sc)?;
                    if let Value::NsRef(ns) = &x {
                        return eng.ns_value(ns, &name, sc);
                    }
                    if safe && matches!(x, Value::Null | Value::Absent) {
                        return Ok(Value::Absent);
                    }
                    eng.access(&eng.deref(x)?, &name)
                })
            }
            Expr::Index { x, i } => {
                let (base, index) = (self.get(x), self.get(i));
                Rc::new(move |eng, sc| {
                    let x = eng.mat_val(base(eng, sc)?)?;
                    let i = index(eng, sc)?;
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
                            Value::Str(k) => {
                                Ok(m.borrow().get(k).cloned().unwrap_or(Value::Absent))
                            }
                            _ => Ok(Value::Absent),
                        },
                        Value::Rec(_) => match &i {
                            Value::Str(k) => eng.access(&x, k),
                            _ => err("index on record needs a string"),
                        },
                        _ => err("index on non-collection"),
                    }
                })
            }
            Expr::Call { fun, args } => {
                let (fun, args) = (
                    self.callee(fun),
                    args.iter().map(|a| self.get(a)).collect::<Vec<_>>(),
                );
                Rc::new(move |eng, sc| {
                    let args = args.iter().map(|a| a(eng, sc)).collect::<R<Vec<_>>>()?;
                    eng.call(&fun(eng, sc)?, args, sc)
                })
            }
            Expr::Lambda { params, body } => {
                self.get(body);
                let (params, body) = (params.clone(), body.clone());
                Rc::new(move |_, sc| {
                    Ok(Value::Clo(Rc::new(Closure {
                        params: params.clone(),
                        body: body.clone(),
                        scope: sc.clone(),
                    })))
                })
            }
            Expr::With { base, patch } => {
                let (base, patch) = (self.get(base), self.get(patch));
                Rc::new(move |eng, sc| {
                    let v = eng.deref(base(eng, sc)?)?;
                    if !matches!(v, Value::PreObj(_) | Value::Map(_) | Value::Rec(_)) {
                        return err("with on non-record");
                    }
                    eng.with_value(v, patch(eng, sc)?)
                })
            }
            Expr::Match { subject, arms } => {
                let subject = self.get(subject);
                let arms: Vec<_> = arms
                    .iter()
                    .map(|a| (a.v.clone(), a.ty.clone(), self.get(&a.body)))
                    .collect();
                Rc::new(move |eng, sc| {
                    let v = eng.deref(subject(eng, sc)?)?;
                    let run = |name: &str, op: &Op| {
                        op(eng, &sc.with_locals(sc.locals.with(name.into(), v.clone())))
                    };
                    let mut fallback = None;
                    for (name, ty, op) in &arms {
                        let Some(ty) = ty else {
                            fallback = Some((name, op));
                            continue;
                        };
                        let menv = sc.menv.as_ref().unwrap_or(&eng.env);
                        let rt = menv.resolve(ty, None).or_else(err)?;
                        if eng.member_of(&v, &rt, sc) {
                            return run(name, op);
                        }
                    }
                    if let Some((name, op)) = fallback {
                        return run(name, op);
                    }
                    err("match: no arm matched")
                })
            }
            Expr::Comp { head, clauses } => {
                self.get(head);
                let (head, clauses) = (head.clone(), self.clauses(clauses));
                Rc::new(move |eng, sc| {
                    let mut items = vec![];
                    visit(eng, sc, &clauses, &mut |scope| {
                        items.push((false, pre(&head, scope)));
                        Ok(())
                    })?;
                    Ok(Value::PreArr(Rc::new(items)))
                })
            }
            Expr::MapComp { key, val, clauses } => {
                let (key, val, clauses) = (self.get(key), self.get(val), self.clauses(clauses));
                Rc::new(move |eng, sc| {
                    let mut entries = vec![];
                    visit(eng, sc, &clauses, &mut |scope| {
                        let Value::Str(k) = key(eng, scope)? else {
                            return err("map key must be string");
                        };
                        let k = k.to_string();
                        if entries.iter().any(|(n, _)| *n == k) {
                            return err_code(format!("duplicate key {k}"), "E5004");
                        }
                        entries.push((k, val(eng, scope)?));
                        Ok(())
                    })?;
                    Ok(Value::PreObj(Rc::new(entries)))
                })
            }
        }
    }

    fn clauses(&self, clauses: &[ForClause]) -> Vec<Clause> {
        clauses
            .iter()
            .map(|c| Clause {
                name: c.v.as_str().into(),
                iter: self.get(&c.iter),
                filters: c.filters.iter().map(|f| self.get(f)).collect(),
            })
            .collect()
    }
}

fn pre(expr: &Rc<Expr>, scope: &Scope) -> Value {
    Value::PreVal(Rc::new(PreValV {
        expr: expr.clone(),
        scope: scope.clone(),
    }))
}

struct Clause {
    name: Rc<str>,
    iter: Op,
    filters: Vec<Op>,
}

fn visit(
    eng: &Engine,
    sc: &Scope,
    clauses: &[Clause],
    emit: &mut dyn FnMut(&Scope) -> R<()>,
) -> R<()> {
    let Some(c) = clauses.first() else {
        return emit(sc);
    };
    for item in eng.iterate(&(c.iter)(eng, sc)?)? {
        let next = sc.with_locals(sc.locals.with(c.name.clone(), item));
        let mut keep = true;
        for f in &c.filters {
            if !eng.truthy(&f(eng, &next)?)? {
                keep = false;
                break;
            }
        }
        if keep {
            visit(eng, &next, &clauses[1..], emit)?;
        }
    }
    Ok(())
}
