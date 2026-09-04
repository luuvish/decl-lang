//! The subsumption judgment ⊑ (§3.17) — the runtime needs it for `match`
//! arm selection over bound records and generic value-argument checks.
use crate::ast::Expr;
use crate::semantics::*;
use num_traits::ToPrimitive;
use std::collections::HashMap;
use std::rc::Rc;

pub fn subsumes(env: &Rc<Env>, a: &RT, b: &RT) -> bool {
    let mut assume: HashMap<usize, Vec<usize>> = HashMap::new();
    sub(env, a, b, &mut assume)
}

fn sub(env: &Rc<Env>, a: &RT, b: &RT, assume: &mut HashMap<usize, Vec<usize>>) -> bool {
    if Rc::ptr_eq(a, b) {
        return true;
    }
    let (ia, ib) = (Rc::as_ptr(a) as usize, Rc::as_ptr(b) as usize);
    if is_rec(a) && is_rec(b) {
        if assume.get(&ia).map(|s| s.contains(&ib)).unwrap_or(false) {
            return true;
        }
    }
    if let RTk::Union(arms) = &a.k {
        return arms.iter().all(|x| sub(env, x, b, assume));
    }
    if let RTk::Union(arms) = &b.k {
        return arms.iter().any(|x| sub(env, a, x, assume));
    }
    if let RTk::IsectN(arms) = &a.k {
        return arms.iter().any(|x| sub(env, x, b, assume));
    }
    if let RTk::IsectN(arms) = &b.k {
        return arms.iter().all(|x| sub(env, a, x, assume));
    }
    if let RTk::Pred { base: bb, preds: bp } = &b.k {
        return match &a.k {
            RTk::Pred { base: ab, preds: ap } => sub(env, ab, bb, assume) && bp.iter().all(|p| ap.iter().any(|q| pred_eq(p, q))),
            RTk::Lit(v) => lit_satisfies(env, v, bb, bp),
            _ => false,
        };
    }
    if let RTk::Pred { base, .. } = &a.k {
        return sub(env, base, b, assume);
    }
    match &b.k {
        RTk::Prim(bn) => match &a.k {
            RTk::Prim(an) => an == bn,
            RTk::Lit(v) => lit_kind(v) == bn,
            RTk::Range { base, .. } => base == bn,
            RTk::Pattern { .. } => bn == "string",
            _ => false,
        },
        RTk::Lit(bv) => matches!(&a.k, RTk::Lit(av) if lit_eq(av, bv)),
        RTk::Range { lo, hi, excl, base } => match &a.k {
            RTk::Lit(v) => lit_kind(v) == base && in_range(v, lo, hi, *excl),
            RTk::Range { lo: alo, hi: ahi, excl: aexcl, base: abase } => {
                if abase != base {
                    return false;
                }
                let a_hi = if *aexcl { dec(ahi) } else { ahi.clone() };
                let b_hi = if *excl { dec(hi) } else { hi.clone() };
                num_ge(alo, lo) && num_le(&a_hi, &b_hi)
            }
            _ => false,
        },
        RTk::Pattern { src, re } => match &a.k {
            RTk::Lit(Value::Str(s)) => re.is_match(s),
            RTk::Pattern { src: asrc, .. } => asrc == src,
            _ => false,
        },
        RTk::Arr { elem, lo, hi } => match &a.k {
            RTk::Arr { elem: ae, lo: alo, hi: ahi } => {
                sub(env, ae, elem, assume)
                    && alo.unwrap_or(0) >= lo.unwrap_or(0)
                    && ahi.unwrap_or(i64::MAX) <= hi.unwrap_or(i64::MAX)
            }
            _ => false,
        },
        RTk::Map { key, val } => match &a.k {
            RTk::Map { key: ak, val: av } => sub(env, ak, key, assume) && sub(env, av, val, assume),
            _ => false,
        },
        RTk::Quantity(d) => matches!(&a.k, RTk::Quantity(ad) if ad == d),
        RTk::Ref(t) => matches!(&a.k, RTk::Ref(at) if sub(env, at, t, assume)),
        RTk::Func { params, ret } => match &a.k {
            RTk::Func { params: ap, ret: ar } => {
                ap.len() == params.len() && params.iter().zip(ap).all(|(bp, ap)| sub(env, bp, ap, assume)) && sub(env, ar, ret, assume)
            }
            _ => false,
        },
        RTk::Rec(br) => {
            let RTk::Rec(ar) = &a.k else { return false };
            assume.entry(ia).or_default().push(ib);
            let bm = br.members.borrow().clone();
            let am = ar.members.borrow().clone();
            for m in &bm {
                if m.hidden {
                    continue; // not part of the value: ⊑ never compares it (D34)
                }
                let sm = am.iter().find(|x| x.name == m.name);
                let m_types: Vec<RT> = m.conj.clone().unwrap_or_else(|| m.ty.iter().cloned().collect());
                let s_types: Vec<RT> = sm.map(|s| s.conj.clone().unwrap_or_else(|| s.ty.iter().cloned().collect())).unwrap_or_default();
                let mut type_ok = || m_types.is_empty() || s_types.is_empty() || m_types.iter().all(|mt| s_types.iter().any(|st| sub(env, st, mt, assume)));
                let ok = match m.kind {
                    MKind::Req => sm.map(|s| s.kind != MKind::Opt).unwrap_or(false) && type_ok(),
                    MKind::Opt | MKind::Dflt => sm.is_none() || type_ok(),
                    MKind::Der => sm.is_some() && type_ok(),
                };
                if !ok {
                    if let Some(v) = assume.get_mut(&ia) {
                        v.retain(|x| *x != ib);
                    }
                    return false;
                }
            }
            true
        }
        RTk::Any => true,
        _ => false,
    }
}

fn lit_kind(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "string",
        Value::Bool(_) => "bool",
        Value::Null => "null",
        _ => "unknown",
    }
}
fn lit_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}
fn dec(v: &Value) -> Value {
    match v {
        Value::Int(i) => Value::Int(i - 1),
        Value::Float(f) => Value::Float(f - 1.0),
        other => other.clone(),
    }
}
fn num_ge(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x >= y,
        (Value::Float(x), Value::Float(y)) => x >= y,
        (Value::Int(x), Value::Float(y)) => x.to_f64().map(|f| f >= *y).unwrap_or(false),
        (Value::Float(x), Value::Int(y)) => y.to_f64().map(|f| *x >= f).unwrap_or(false),
        _ => false,
    }
}
fn num_le(a: &Value, b: &Value) -> bool {
    num_ge(b, a)
}
fn in_range(v: &Value, lo: &Value, hi: &Value, excl: bool) -> bool {
    let h = if excl { dec(hi) } else { hi.clone() };
    num_ge(v, lo) && num_le(v, &h)
}
fn pred_eq(a: &Rc<Expr>, b: &Rc<Expr>) -> bool {
    match (&**a, &**b) {
        (Expr::Name(x), Expr::Name(y)) => x == y,
        (Expr::Call { fun: fa, args: aa }, Expr::Call { fun: fb, args: ab }) => {
            pred_eq(fa, fb)
                && aa.len() == ab.len()
                && aa.iter().zip(ab).all(|(x, y)| matches!((&**x, &**y), (Expr::Lit(p), Expr::Lit(q)) if lit_eq(p, q)))
        }
        _ => false,
    }
}
fn lit_satisfies(env: &Rc<Env>, v: &Value, base: &RT, preds: &[Rc<Expr>]) -> bool {
    let eng = crate::engine::Engine::bare(env.clone());
    let sc = Scope::new("", None);
    if !subsumes(env, &ty(RTk::Lit(v.clone())), base) {
        return false;
    }
    for p in preds {
        let ok = eng.ev(p, &sc).and_then(|f| eng.call(&f, vec![v.clone()], &sc));
        if !matches!(ok, Ok(Value::Bool(true))) {
            return false;
        }
    }
    true
}

// ---------------- structural emptiness (§3.17; the checker's E4011/E4012) ----------------
/// JavaScript `>`: strings compare lexically, a string against a number is NaN (false)
fn js_gt(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x > y,
        (Value::Str(_), _) | (_, Value::Str(_)) => false,
        _ => num_ge(a, b) && !value_eq(a, b),
    }
}

pub fn structurally_empty(env: &Rc<Env>, t: &RT) -> bool {
    match &t.k {
        RTk::Range { lo, hi, excl, .. } => {
            let h = if *excl && !matches!(hi, Value::Str(_)) { dec(hi) } else { hi.clone() };
            js_gt(lo, &h)
        }
        RTk::Arr { lo, hi, .. } => matches!((lo, hi), (Some(l), Some(h)) if l > h),
        RTk::IsectN(arms) => {
            for i in 0..arms.len() {
                for j in i + 1..arms.len() {
                    if disjoint(env, &arms[i], &arms[j]) {
                        return true;
                    }
                }
            }
            arms.iter().any(|a| structurally_empty(env, a))
        }
        RTk::Union(arms) => arms.iter().all(|a| structurally_empty(env, a)),
        _ => false,
    }
}

fn kind_of(t: &RT) -> Option<String> {
    Some(match &t.k {
        RTk::Prim(n) => n.clone(),
        RTk::Lit(v) => lit_kind(v).to_string(),
        RTk::Range { base, .. } => base.clone(),
        RTk::Pattern { .. } => "string".into(),
        RTk::Arr { .. } => "array".into(),
        RTk::Rec(_) | RTk::Map { .. } | RTk::Quantity(_) => "object".into(),
        _ => return None,
    })
}

fn disjoint(env: &Rc<Env>, a: &RT, b: &RT) -> bool {
    let (ka, kb) = (kind_of(a), kind_of(b));
    if let (Some(x), Some(y)) = (&ka, &kb) {
        if x != y {
            return true;
        }
    }
    match (&a.k, &b.k) {
        (RTk::Range { lo: alo, hi: ahi, excl: aexcl, base: abase }, RTk::Range { lo: blo, hi: bhi, excl: bexcl, base: bbase }) if abase == bbase => {
            let a_hi = if *aexcl { dec(ahi) } else { ahi.clone() };
            let b_hi = if *bexcl { dec(bhi) } else { bhi.clone() };
            js_gt(alo, &b_hi) || js_gt(blo, &a_hi)
        }
        (RTk::Lit(x), RTk::Lit(y)) => !lit_eq(x, y),
        (RTk::Lit(v), RTk::Range { lo, hi, excl, base }) => !(lit_kind(v) == base && in_range(v, lo, hi, *excl)),
        (RTk::Range { .. }, RTk::Lit(_)) => disjoint(env, b, a),
        _ => false,
    }
}
