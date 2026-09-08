//! The query engine's incremental core (src/qengine/db.rs): memoization,
//! dependency tracking, and the two early cutoffs. Recompute counts prove the
//! cutoffs. The Rust counterpart of decl-ts/tests/qengine/db_test.ts.
use decl_lang::qengine::db::{Db, DbErr};
use decl_lang::semantics::{Num, Value};
use std::cell::Cell;
use std::rc::Rc;

fn int(n: i64) -> Value {
    Value::Int(Num::from(n))
}
fn as_int(v: &Value) -> i64 {
    match v {
        Value::Int(n) => n.to_i64().expect("small int"),
        _ => panic!("not an int: {v:?}"),
    }
}
fn ok(r: Result<Value, DbErr>) -> Value {
    match r {
        Ok(v) => v,
        Err(_) => panic!("query failed"),
    }
}

#[test]
fn memoize_and_cutoffs() {
    // sum = a + b ; doubled = sum * 2 ; sign = sign(a)
    let calls = Rc::new((Cell::new(0i32), Cell::new(0i32), Cell::new(0i32))); // sum, doubled, sign
    let c = calls.clone();
    let db = Rc::new(Db::new(Box::new(move |k: &str| -> Option<_> {
        let c = c.clone();
        match k {
            "sum" => Some(Box::new(move |db: &Db| {
                c.0.set(c.0.get() + 1);
                Ok(int(as_int(&db.query("a")?) + as_int(&db.query("b")?)))
            }) as Box<dyn Fn(&Db) -> Result<Value, DbErr>>),
            "doubled" => Some(Box::new(move |db: &Db| {
                c.1.set(c.1.get() + 1);
                Ok(int(as_int(&db.query("sum")?) * 2))
            })),
            "sign" => Some(Box::new(move |db: &Db| {
                c.2.set(c.2.get() + 1);
                Ok(int(as_int(&db.query("a")?).signum()))
            })),
            _ => None,
        }
    })));

    db.set_input("a", int(1));
    db.set_input("b", int(2));
    assert_eq!(as_int(&ok(db.query("doubled"))), 6);
    assert_eq!(
        (calls.0.get(), calls.1.get(), calls.2.get()),
        (1, 1, 0),
        "only the demanded chain computes"
    );

    ok(db.query("doubled")); // nothing changed
    assert_eq!(
        (calls.0.get(), calls.1.get()),
        (1, 1),
        "no change recomputes nothing"
    );

    db.set_input("b", int(10)); // sum and doubled both change
    assert_eq!(as_int(&ok(db.query("doubled"))), 22);
    assert_eq!(
        (calls.0.get(), calls.1.get()),
        (2, 2),
        "a real change recomputes the chain"
    );

    // a and b change but their sum does not: sum recomputes, its value is
    // unchanged, so doubled cuts off (value cutoff propagates)
    db.set_input("a", int(5));
    db.set_input("b", int(6)); // 5 + 6 == 11, the same sum as 1 + 10
    assert_eq!(as_int(&ok(db.query("doubled"))), 22);
    assert_eq!(
        (calls.0.get(), calls.1.get()),
        (3, 2),
        "value cutoff stops the dependent"
    );
}

#[test]
fn verify_cutoff_unrelated_input() {
    let sum = Rc::new(Cell::new(0i32));
    let s = sum.clone();
    let db = Db::new(Box::new(move |k: &str| -> Option<_> {
        if k == "sum" {
            let s = s.clone();
            Some(Box::new(move |db: &Db| {
                s.set(s.get() + 1);
                Ok(int(as_int(&db.query("a")?) + as_int(&db.query("b")?)))
            }) as Box<dyn Fn(&Db) -> Result<Value, DbErr>>)
        } else {
            None
        }
    }));
    db.set_input("a", int(1));
    db.set_input("b", int(2));
    db.set_input("c", int(9));
    ok(db.query("sum"));
    db.set_input("c", int(99)); // sum never read c
    assert_eq!(as_int(&ok(db.query("sum"))), 3);
    assert_eq!(sum.get(), 1, "an unrelated input recomputes nothing");
}

#[test]
fn a_query_that_reads_itself_is_a_cycle() {
    let db = Db::new(Box::new(|k: &str| -> Option<_> {
        match k {
            "x" => {
                Some(Box::new(|db: &Db| db.query("y")) as Box<dyn Fn(&Db) -> Result<Value, DbErr>>)
            }
            "y" => Some(Box::new(|db: &Db| db.query("x"))),
            _ => None,
        }
    }));
    match db.query("x") {
        Err(DbErr::Cycle(_)) => {}
        other => panic!("expected a cycle, got {:?}", other.is_ok()),
    }
}
