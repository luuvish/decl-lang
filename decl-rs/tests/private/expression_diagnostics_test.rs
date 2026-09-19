use super::*;
use crate::ast::{Decl, ForClause};
use crate::engine::Engine;
use crate::qengine::programs::Programs;
use crate::semantics::*;
use std::collections::HashMap;

fn lit(n: i64) -> Rc<Expr> {
    Rc::new(Expr::Lit(Value::Int(Num::from(n))))
}
fn call(f: NatFn) -> Rc<Expr> {
    Rc::new(Expr::Call {
        fun: Rc::new(Expr::Lit(Value::Nat(f))),
        args: vec![],
    })
}
fn comp(line: usize, iter: Rc<Expr>, head: Rc<Expr>, filters: Vec<Rc<Expr>>) -> Rc<Expr> {
    let e = Rc::new(Expr::Comp {
        head,
        clauses: vec![ForClause {
            v: "n".into(),
            iter,
            filters,
        }],
    });
    ast::set_expr_loc(
        &e,
        Loc {
            sl: line,
            sc: 3,
            el: line,
            ec: 30,
        },
    );
    e
}
fn input() -> Rc<Expr> {
    Rc::new(Expr::Arr(vec![
        (false, lit(1)),
        (false, lit(2)),
        (false, lit(3)),
    ]))
}
fn module(exprs: &[Rc<Expr>]) -> Module {
    Module {
        path: "/native/selected.decl".into(),
        env: Env::new(),
        exports: Rc::new(RefCell::new(HashMap::new())),
        decls: exprs
            .iter()
            .enumerate()
            .map(|(i, e)| Decl {
                body: DeclBody::Const {
                    name: format!("c{i}"),
                    ty: None,
                    expr: e.clone(),
                },
                exported: false,
                annotations: vec![],
                loc: None,
            })
            .collect(),
    }
}
fn engine(compiled: bool) -> Rc<Engine> {
    let e = Engine::bare(Env::new());
    if compiled {
        *e.programs.borrow_mut() = Some(Rc::new(Programs::default()));
    }
    e
}

#[test]
fn selected_scan_counts_both_backends_without_forcing_lazy_heads() {
    for compiled in [false, true] {
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        let head = call(Rc::new(Box::new(move |_| {
            count.set(count.get() + 1);
            Ok(Value::Int(Num::from(9)))
        })));
        let filter = Rc::new(Expr::Bin {
            op: ">".into(),
            l: Rc::new(Expr::Name("n".into())),
            r: lit(1),
        });
        let expr = comp(10, input(), head, vec![filter]);
        let m = module(std::slice::from_ref(&expr));
        let before = Rc::strong_count(&expr);
        let session = install(8).unwrap();
        register(&m, 0, 10, 1).unwrap();
        assert_eq!(Rc::strong_count(&expr), before);
        let eng = engine(compiled);
        let value = eng.ev(&expr, &Scope::new("root", None)).ok().unwrap();
        assert_eq!(calls.get(), 0);
        let report = session.finish();
        assert!(report.valid);
        assert_eq!(report.records.len(), 1);
        let row = &report.records[0];
        assert_eq!(row.compiled, compiled);
        assert_eq!(row.work.clauses[0], [1, 3, 3, 1, 2]);
        assert_eq!(row.work.emitted_heads, 2);
        assert!(matches!(row.outcome, Outcome::Ok));
        let items = eng.iterate(&value).ok().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(calls.get(), 2);
    }
}

#[test]
fn nested_native_demand_preserves_parent_order_and_values_counts() {
    for compiled in [false, true] {
        let eng = engine(compiled);
        let inner = comp(20, input(), Rc::new(Expr::Name("n".into())), vec![]);
        let weak = Rc::downgrade(&eng);
        let nested = inner.clone();
        let outer = comp(
            10,
            call(Rc::new(Box::new(move |_| {
                let eng = weak.upgrade().unwrap();
                // A real standard-library call belongs to the nearest watched scope.
                let map = Value::Map(Rc::new(RefCell::new(MapV {
                    entries: vec![("x".into(), Value::Null)].into_iter().collect(),
                    path: PrefixPath::default(),
                })));
                eng.call(
                    &Value::Std(Rc::new(vec!["map".into(), "values".into()])),
                    vec![map],
                    &Scope::new("", None),
                )?;
                eng.ev(&nested, &Scope::new("", None))
            }))),
            Rc::new(Expr::Name("n".into())),
            vec![],
        );
        let m = module(&[outer.clone(), inner]);
        let session = install(8).unwrap();
        register(&m, 0, 10, 1).unwrap();
        register(&m, 1, 20, 1).unwrap();
        assert!(eng.ev(&outer, &Scope::new("", None)).is_ok());
        let report = session.finish();
        assert!(report.valid);
        assert_eq!(report.records.len(), 2);
        let (child, parent) = (&report.records[0], &report.records[1]);
        assert_eq!((child.site_id, parent.site_id), (1, 0));
        assert_eq!(child.parent_id, Some(parent.id));
        assert!(
            parent.start.wall_ns <= child.start.wall_ns && child.end.wall_ns <= parent.end.wall_ns
        );
        assert_eq!(
            (
                parent.work.values_attempts,
                parent.work.values_returns,
                parent.work.values_items
            ),
            (1, 1, 1)
        );
        assert_eq!(parent.work.clauses[0], [1, 3, 0, 0, 3]);
        assert_eq!(child.work.clauses[0], [1, 3, 0, 0, 3]);
    }
}

#[test]
fn deferred_failed_and_unwound_attempts_keep_partial_counters() {
    for mode in 0..4 {
        let expr = comp(
            10,
            call(Rc::new(Box::new(move |_| match mode {
                0 => Err(Fail::Defer),
                1 => err("native failure"),
                2 => Err(Fail::Taint),
                _ => panic!("native unwind"),
            }))),
            lit(1),
            vec![],
        );
        let m = module(std::slice::from_ref(&expr));
        let session = install(8).unwrap();
        register(&m, 0, 10, 1).unwrap();
        let eng = engine(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            eng.ev(&expr, &Scope::new("", None))
        }));
        assert_eq!(result.is_err(), mode == 3);
        let report = session.finish();
        assert!(report.valid);
        assert_eq!(report.records.len(), 1);
        let record = &report.records[0];
        assert_eq!(record.work.clauses[0], [1, 0, 0, 0, 0]);
        assert!(matches!(
            (mode, record.outcome),
            (0, Outcome::Defer)
                | (1, Outcome::EvalError)
                | (2, Outcome::Taint)
                | (3, Outcome::Unwound)
        ));
    }
}

#[test]
fn registration_is_weak_and_cow_or_reinstallation_cannot_reuse_tokens() {
    let mut expr = comp(10, input(), lit(1), vec![]);
    let m = module(std::slice::from_ref(&expr));
    let session = install(8).unwrap();
    register(&m, 0, 10, 1).unwrap();
    assert!(register(&m, 0, 10, 1).is_err());
    assert!(register(&m, 1, 99, 1).is_err());
    let token = selected(&expr, true).unwrap();
    let weak = Rc::downgrade(&expr);
    drop(m);
    assert_eq!(Rc::strong_count(&expr), 1);
    Rc::make_mut(&mut expr);
    assert!(weak.upgrade().is_none());
    assert!(selected(&expr, false).is_none());
    let mut stale = Attempt::begin(Some(token), 1);
    let report = session.finish();
    assert!(!report.valid);
    assert_eq!(report.open_attempts, 1);
    let next = install(8).unwrap();
    stale.finish(&Ok(()));
    let mut ignored = Attempt::begin(Some(token), 1);
    ignored.finish(&Ok(()));
    let report = next.finish();
    assert!(report.valid);
    assert!(report.records.is_empty());
}

#[test]
fn capacity_and_counter_failures_are_explicit_without_changing_results() {
    let expr = comp(10, input(), lit(1), vec![]);
    let m = module(std::slice::from_ref(&expr));
    let session = install(1).unwrap();
    register(&m, 0, 10, 1).unwrap();
    let eng = engine(false);
    for _ in 0..2 {
        assert!(eng.ev(&expr, &Scope::new("", None)).is_ok());
    }
    let report = session.finish();
    assert!(!report.valid);
    assert_eq!(report.attempts, 2);
    assert_eq!(report.records.len(), 1);
    assert_eq!(report.dropped_records, 1);
    let session = install(1).unwrap();
    register(&m, 0, 10, 1).unwrap();
    let mut attempt = Attempt::begin(selected(&expr, false), 1);
    update(attempt.handle(), |w| {
        w.emitted_heads = u64::MAX;
        false
    });
    emitted(attempt.handle());
    attempt.finish(&Ok(()));
    let report = session.finish();
    assert!(!report.valid && report.counter_overflow);
    let invalid = Stamp {
        wall_ns: None,
        ..Stamp::default()
    };
    assert!(!invalid.valid());
}

#[test]
fn three_clauses_preserve_empty_iteration_and_filter_short_circuit_counts() {
    for compiled in [false, true] {
        for empty in [false, true] {
            let filter_calls = Rc::new(Cell::new(0));
            let observed = filter_calls.clone();
            let expr = Rc::new(Expr::Comp {
                head: lit(7),
                clauses: vec![
                    ForClause {
                        v: "key".into(),
                        iter: Rc::new(Expr::Arr(if empty {
                            vec![]
                        } else {
                            vec![(false, lit(1)), (false, lit(2))]
                        })),
                        filters: vec![],
                    },
                    ForClause {
                        v: "n".into(),
                        iter: input(),
                        filters: vec![
                            Rc::new(Expr::Bin {
                                op: "==".into(),
                                l: Rc::new(Expr::Name("n".into())),
                                r: Rc::new(Expr::Name("key".into())),
                            }),
                            call(Rc::new(Box::new(move |_| {
                                observed.set(observed.get() + 1);
                                Ok(Value::Bool(true))
                            }))),
                        ],
                    },
                    ForClause {
                        v: "child".into(),
                        iter: Rc::new(Expr::Arr(vec![(false, lit(0)), (false, lit(1))])),
                        filters: vec![],
                    },
                ],
            });
            ast::set_expr_loc(
                &expr,
                Loc {
                    sl: 30,
                    sc: 0,
                    el: 40,
                    ec: 1,
                },
            );
            let m = module(std::slice::from_ref(&expr));
            let session = install(8).unwrap();
            register(&m, 0, 30, 3).unwrap();
            let eng = engine(compiled);
            let result = eng.ev(&expr, &Scope::new("", None)).ok().unwrap();
            let report = session.finish();
            assert!(report.valid);
            assert_eq!(report.records.len(), 1);
            let row = &report.records[0];
            if empty {
                assert_eq!(row.work.clauses[0], [1, 0, 0, 0, 0]);
                assert_eq!(row.work.clauses[1], [0; 5]);
                assert_eq!(row.work.clauses[2], [0; 5]);
                assert_eq!(filter_calls.get(), 0);
                assert_eq!(row.work.emitted_heads, 0);
            } else {
                assert_eq!(row.work.clauses[0], [1, 2, 0, 0, 2]);
                assert_eq!(row.work.clauses[1], [2, 6, 8, 4, 2]);
                assert_eq!(row.work.clauses[2], [2, 4, 0, 0, 4]);
                assert_eq!(filter_calls.get(), 2);
                assert_eq!(row.work.emitted_heads, 4);
            }
            assert!(row.work.clauses[3..].iter().all(|c| *c == [0; 5]));
            assert_eq!(
                eng.iterate(&result).ok().unwrap().len(),
                if empty { 0 } else { 4 }
            );
        }
    }
}
