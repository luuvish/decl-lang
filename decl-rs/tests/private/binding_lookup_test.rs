//! Native input ownership and ordering through compiled record lookup storage.
use super::*;

fn record_type(width: usize, required: &[usize], open: bool) -> RT {
    let rec = rec_type(open);
    *rec.members.borrow_mut() = Rc::new(
        (0..width)
            .map(|index| Member {
                kind: if required.contains(&index) {
                    MKind::Req
                } else {
                    MKind::Opt
                },
                name: format!("k{index}"),
                hidden: false,
                ty: Some(ty(RTk::Any)),
                conj: None,
                dflt: None,
                expr: None,
                menv: None,
            })
            .collect(),
    );
    ty(RTk::Rec(rec))
}

fn engine() -> Rc<Engine> {
    let eng = Engine::bare(Env::new());
    *eng.programs.borrow_mut() = Some(Rc::new(Programs::default()));
    eng
}

fn bind_at(eng: &Engine, raw: Value, rt: &RT, at: &str) -> Inst {
    let Value::Rec(inst) = eng
        .bind(
            raw,
            rt,
            &[Seg::Name("root".into()), Seg::Name(at.into())],
            None,
            &Scope::new("root", None),
        )
        .ok()
        .expect("native record binds")
    else {
        panic!("record result");
    };
    inst
}

fn assert_int(value: R<Value>, expected: i64) {
    assert!(matches!(value, Ok(Value::Int(n)) if n.to_i64() == Some(expected)));
}

#[test]
fn compiled_lookup_preserves_native_duplicates_order_and_sparse_absence() {
    // Small, wide sparse, populated, and duplicate-heavy records have the same
    // ownership/order contract despite their different lookup storage needs.
    for (width, present) in [
        (4, 3),
        (128, 12),
        (32, 17),
        (128, 1),
        (18, 9),
        (19, 9),
        (19, 10),
    ] {
        let eng = engine();
        let rt = record_type(width, &[], true);
        let mut entries: Vec<_> = (0..present)
            .rev()
            .map(|i| (format!("k{i}"), Value::Int(Num::from(i as i64))))
            .collect();
        for i in 0..64 {
            entries.push(("k0".into(), Value::Int(Num::from(i))));
        }
        entries.push(("extra key λ".into(), Value::Bool(false)));
        for i in 0..present {
            entries.push((format!("k{i}"), Value::Int(Num::from(1000 + i as i64))));
        }
        entries.push(("extra key λ".into(), Value::Bool(true)));
        let order: Vec<_> = entries.iter().map(|(k, _)| k.clone()).collect();
        let inst = bind_at(&eng, Value::JObj(Rc::new(entries)), &rt, "ordered");
        assert_eq!(inst.borrow().entry_order(), order);
        for index in 0..present {
            assert_int(
                eng.force_slot(&inst, &format!("k{index}")),
                1000 + index as i64,
            );
        }
        for index in present..width {
            assert!(matches!(
                eng.force_slot(&inst, &format!("k{index}")),
                Ok(Value::Absent)
            ));
        }
        let bound = inst.borrow();
        assert_eq!(bound.extras.len(), 1);
        assert_eq!(bound.extras[0].0, "extra key λ");
        assert!(matches!(bound.extras[0].1, Value::Bool(true)));
        assert_eq!(bound.slots.len(), width);
        assert!(eng.env.diagnostics_vec().is_empty());
    }
}

fn callback(value: bool, called: Rc<RefCell<Vec<bool>>>) -> Value {
    Value::PreVal(Rc::new(PreValV {
        expr: Rc::new(Expr::Call {
            fun: Rc::new(Expr::Lit(Value::native(move |_| {
                called.borrow_mut().push(value);
                Ok(Value::Bool(value))
            }))),
            args: vec![],
        }),
        scope: Scope::new("root", None),
    }))
}

#[test]
fn compiled_lookup_captures_only_the_selected_value_at_its_slot() {
    let eng = engine();
    let called = Rc::new(RefCell::new(vec![]));
    let discarded = callback(true, called.clone());
    let selected = callback(false, called.clone());
    let Value::PreVal(first) = &discarded else {
        panic!("native value")
    };
    let weak_first = Rc::downgrade(first);
    let Value::PreVal(last) = &selected else {
        panic!("native value")
    };
    let weak_last = Rc::downgrade(last);
    let mut entries = vec![("k1".into(), discarded)];
    for i in 2..=17 {
        entries.push((format!("k{i}"), Value::Int(Num::from(i))));
    }
    entries.push(("k1".into(), selected));
    let observations = Rc::new(RefCell::new(vec![]));
    let counts = observations.clone();
    let observed_first = weak_first.clone();
    let observed_last = weak_last.clone();
    *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
        counts
            .borrow_mut()
            .push((observed_first.strong_count(), observed_last.strong_count()));
        None
    }));
    let inst = bind_at(
        &eng,
        Value::JObj(Rc::new(entries)),
        &record_type(32, &[0, 31], false),
        "owners",
    );
    *eng.env.tagger.borrow_mut() = None;
    assert_eq!(*observations.borrow(), [(1, 1), (1, 2)]);
    assert!(weak_first.upgrade().is_none());
    assert_eq!(weak_last.strong_count(), 1);
    assert!(called.borrow().is_empty());
    assert!(matches!(
        eng.force_slot(&inst, "k1"),
        Ok(Value::Bool(false))
    ));
    assert_eq!(*called.borrow(), [false]);
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].path, "root.owners.k0");
    assert_eq!(diagnostics[1].path, "root.owners.k31");
    drop(inst);
    drop(eng);
    assert!(weak_last.upgrade().is_none());
}

#[test]
fn compiled_lookup_keeps_input_and_schema_snapshots_during_reentrant_replacement() {
    let eng = engine();
    let rt = record_type(32, &[0], false);
    let external = Rc::new(RefCell::new(Rc::new(
        (1..=17)
            .map(|i| (format!("k{i}"), Value::Int(Num::from(i))))
            .collect::<Vec<_>>(),
    )));
    let weak_input = Rc::downgrade(&external.borrow());
    let weak_external = Rc::downgrade(&external);
    let weak_engine = Rc::downgrade(&eng);
    let weak_type = Rc::downgrade(&rt);
    let inner = Rc::new(RefCell::new(None));
    let inner_result = inner.clone();
    let called = Rc::new(Cell::new(false));
    let callback_called = called.clone();
    *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
        assert!(!callback_called.replace(true));
        let external = weak_external.upgrade().unwrap();
        Rc::make_mut(&mut external.borrow_mut()).clear();
        let rt = weak_type.upgrade().unwrap();
        let RTk::Rec(rec) = &rt.k else {
            panic!("record type")
        };
        {
            let mut members = rec.members.borrow_mut();
            let members = Rc::make_mut(&mut members);
            members[1].name = "replacement".into();
            members.rotate_right(1);
        }
        let entries = rec
            .members
            .borrow()
            .iter()
            .enumerate()
            .map(|(index, m)| (m.name.clone(), Value::Int(Num::from(2000 + index as i64))))
            .collect();
        let eng = weak_engine.upgrade().unwrap();
        *inner_result.borrow_mut() =
            Some(bind_at(&eng, Value::JObj(Rc::new(entries)), &rt, "inner"));
        None
    }));
    let raw = Value::JObj(external.borrow().clone());
    let outer = bind_at(&eng, raw, &rt, "outer");
    *eng.env.tagger.borrow_mut() = None;
    assert!(called.get());
    assert!(external.borrow().is_empty());
    assert!(
        weak_input.upgrade().is_none(),
        "the old raw input does not escape binding"
    );
    drop(external);
    let inner = inner.borrow_mut().take().unwrap();
    assert!(outer.borrow().slot("replacement").is_none());
    assert!(inner.borrow().slot("k1").is_none());
    for i in 1..=17 {
        assert_int(eng.force_slot(&outer, &format!("k{i}")), i);
    }
    for (index, member) in rec_members(&rt).iter().enumerate() {
        assert_int(eng.force_slot(&inner, &member.name), 2000 + index as i64);
    }
    assert_eq!(
        eng.programs
            .borrow()
            .as_ref()
            .unwrap()
            .compiled_schemas
            .get(),
        2
    );
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E4002"));
    assert_eq!(diagnostics[0].path, "root.outer.k0");
}

#[test]
fn compiled_lookup_values_survive_collection_after_native_source_release() {
    for present in [2, 12, 33] {
        for mutable in [false, true] {
            let eng = engine();
            let child = bind_at(
                &eng,
                Value::JObj(Rc::new(vec![("k0".into(), Value::Bool(true))])),
                &record_type(1, &[0], true),
                "child",
            );
            child
                .borrow_mut()
                .set_extra("self", Value::Rec(child.clone()));
            let weak_child = Rc::downgrade(&child);
            eng.env.registry_clear();
            let mut entries = vec![("k1".into(), Value::Rec(child))];
            for index in 2..=present {
                entries.push((format!("k{index}"), Value::Int(Num::from(index))));
            }
            let raw = if mutable {
                Value::Map(Rc::new(RefCell::new(MapV {
                    entries: entries.into_iter().collect(),
                    path: PrefixPath::default(),
                })))
            } else {
                Value::JObj(Rc::new(entries))
            };
            let external = Rc::new(RefCell::new(raw));
            let weak_external = Rc::downgrade(&external);
            let observed_child = weak_child.clone();
            let called = Rc::new(Cell::new(0));
            let callback_called = called.clone();
            *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
                callback_called.set(callback_called.get() + 1);
                let external = weak_external.upgrade().unwrap();
                let mut source = external.borrow_mut();
                if let Value::Map(map) = &*source {
                    map.borrow_mut().entries.clear();
                }
                *source = Value::Null;
                drop(source);
                // No upgraded observer or previous registry entry roots the
                // child during collection: only raw or the owned snapshot does.
                collect_cycles();
                let child = observed_child
                    .upgrade()
                    .expect("borrowed input remains a GC root");
                assert_eq!(
                    child.borrow().slot("k0").unwrap().state,
                    SlotState::Unforced
                );
                assert!(matches!(child.borrow().extra("self"), Some(Value::Rec(_))));
                None
            }));
            let raw = external.borrow().clone();
            let inst = bind_at(&eng, raw, &record_type(64, &[0], false), "gc");
            *eng.env.tagger.borrow_mut() = None;
            assert_eq!(called.get(), 1);
            assert!(matches!(*external.borrow(), Value::Null));
            drop(external);
            let Value::Rec(child) = eng.force_slot(&inst, "k1").ok().unwrap() else {
                panic!("retained child");
            };
            assert!(Rc::ptr_eq(&child, &weak_child.upgrade().unwrap()));
            assert!(matches!(
                eng.force_slot(&child, "k0"),
                Ok(Value::Bool(true))
            ));
            assert_eq!(eng.env.diagnostics_vec().len(), 1);
            drop(child);
            drop(inst);
            drop(eng);
            collect_cycles();
            assert!(weak_child.upgrade().is_none());
        }
    }
}

#[test]
fn compiled_orders_share_only_equal_keys_and_release_weak_cache_payloads() {
    let eng = engine();
    let rt = record_type(3, &[], true);
    eng.programs.borrow().as_ref().unwrap().schema(&rt);
    let input = |keys: &[&str], n: i64| {
        Value::JObj(Rc::new(
            keys.iter()
                .map(|key| ((*key).into(), Value::Int(Num::from(n))))
                .collect(),
        ))
    };
    let a = bind_at(&eng, input(&["k0", "extra", "k0"], 1), &rt, "a");
    let b = bind_at(&eng, input(&["k0", "extra", "k0"], 2), &rt, "b");
    let snapshot = a.borrow().entry_order_snapshot();
    assert!(Rc::ptr_eq(&snapshot, &b.borrow().entry_order));
    assert_int(eng.force_slot(&a, "k0"), 1);
    assert_int(eng.force_slot(&b, "k0"), 2);
    b.borrow_mut().entry_order_mut().reverse();
    b.borrow_mut().entry_order_mut().push("native".into());
    assert_eq!(a.borrow().entry_order(), ["k0", "extra", "k0"]);
    assert_eq!(snapshot.as_slice(), ["k0", "extra", "k0"]);
    assert_eq!(b.borrow().entry_order(), ["k0", "extra", "k0", "native"]);
    assert!(!Rc::ptr_eq(&snapshot, &b.borrow().entry_order));

    let c = bind_at(&eng, input(&["extra", "k0", "k0"], 3), &rt, "c");
    assert_eq!(c.borrow().entry_order(), ["extra", "k0", "k0"]);
    assert!(!Rc::ptr_eq(&snapshot, &c.borrow().entry_order));
    let d = bind_at(&eng, input(&["extra", "k0", "k0"], 4), &rt, "d");
    assert!(Rc::ptr_eq(&c.borrow().entry_order, &d.borrow().entry_order));
    let weak = Rc::downgrade(&c.borrow().entry_order);
    eng.env.registry_retain(|_| false);
    drop((a, b, c, d, snapshot));
    assert!(weak.upgrade().is_none());
    // Keeping Programs and its compiled schema alive must not retain key text.
    assert!(eng.programs.borrow().is_some());
    let next = bind_at(&eng, input(&["extra", "k0", "k0"], 5), &rt, "next");
    assert_eq!(next.borrow().entry_order(), ["extra", "k0", "k0"]);
    assert_int(eng.force_slot(&next, "k0"), 5);
}

#[test]
fn record_binding_keeps_order_snapshot_across_native_reentry() {
    let eng = engine();
    let rt = record_type(2, &[], false);
    let source = Rc::new(RefCell::new(std::rc::Weak::<RefCell<RecInst>>::new()));
    let capture = source.clone();
    let first = Value::PreVal(Rc::new(PreValV {
        expr: Rc::new(Expr::Call {
            fun: Rc::new(Expr::Lit(Value::native(move |_| {
                capture
                    .borrow()
                    .upgrade()
                    .unwrap()
                    .borrow_mut()
                    .entry_order_mut()
                    .reverse();
                Ok(Value::Int(Num::from(7)))
            }))),
            args: vec![],
        }),
        scope: Scope::new("root", None),
    }));
    let original = bind_at(
        &eng,
        Value::JObj(Rc::new(vec![
            ("k0".into(), first),
            ("k1".into(), Value::Int(Num::from(8))),
        ])),
        &rt,
        "original",
    );
    *source.borrow_mut() = Rc::downgrade(&original);
    let captured_order = original.borrow().entry_order_snapshot();
    let bound = bind_at(&eng, Value::Rec(original.clone()), &rt, "bound");
    assert_eq!(original.borrow().entry_order(), ["k1", "k0"]);
    assert_eq!(captured_order.as_slice(), ["k0", "k1"]);
    assert_eq!(bound.borrow().entry_order(), ["k0", "k1"]);
    assert_int(eng.force_slot(&bound, "k0"), 7);
    assert_int(eng.force_slot(&bound, "k1"), 8);
}
