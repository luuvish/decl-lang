//! Whole-descriptor reuse is metadata sharing, never shared slot state.
use super::*;

fn member(name: &str, kind: MKind, expr: Rc<Expr>) -> Member {
    Member {
        kind,
        name: name.into(),
        hidden: false,
        ty: Some(ty(RTk::Any)),
        conj: None,
        dflt: (kind == MKind::Dflt).then(|| expr.clone()),
        expr: (kind == MKind::Der).then_some(expr),
        menv: None,
    }
}

fn literal(value: i64) -> Rc<Expr> {
    Rc::new(Expr::Lit(Value::Int(Num::from(value))))
}

fn record_type(members: Vec<Member>) -> RT {
    let rec = rec_type(false);
    *rec.members.borrow_mut() = Rc::new(members);
    ty(RTk::Rec(rec))
}

fn engine(compiled: bool) -> Rc<Engine> {
    let eng = Engine::bare(Env::new());
    if compiled {
        *eng.programs.borrow_mut() = Some(Rc::new(Programs::default()));
    }
    eng
}

fn bind(
    eng: &Engine,
    rt: &RT,
    scope: &Scope,
    at: &str,
    entries: Vec<(String, Value)>,
) -> Rc<RefCell<RecInst>> {
    let Value::Rec(inst) = eng
        .bind(
            Value::JObj(Rc::new(entries)),
            rt,
            &[Seg::Name(scope.root_name.clone()), Seg::Name(at.into())],
            None,
            scope,
        )
        .ok()
        .expect("bind record")
    else {
        panic!("record");
    };
    inst
}

fn descriptor(inst: &Rc<RefCell<RecInst>>, index: usize) -> Rc<Compute> {
    inst.borrow().slots[index].1.computation_snapshot().unwrap()
}

fn assert_int(value: R<Value>, expected: i64) {
    assert!(matches!(value, Ok(Value::Int(n)) if n.to_i64() == Some(expected)));
}

#[test]
fn eligible_descriptors_share_but_values_states_and_instance_paths_do_not() {
    let eng = engine(true);
    let rt = record_type(vec![
        member("provided", MKind::Req, literal(0)),
        member(
            "fallback",
            MKind::Dflt,
            Rc::new(Expr::Name("provided".into())),
        ),
        member("where", MKind::Der, Rc::new(Expr::Ctx("$path".into()))),
    ]);
    let scope = Scope::new("root", None);
    let first = bind(
        &eng,
        &rt,
        &scope,
        "first",
        vec![("provided".into(), Value::Int(Num::from(1)))],
    );
    let second = bind(
        &eng,
        &rt,
        &scope,
        "second",
        vec![("provided".into(), Value::Int(Num::from(2)))],
    );
    assert!(!Rc::ptr_eq(&descriptor(&first, 0), &descriptor(&second, 0)));
    for i in [1, 2] {
        assert!(Rc::ptr_eq(&descriptor(&first, i), &descriptor(&second, i)));
    }
    assert_int(eng.force_slot(&first, "fallback"), 1);
    assert_eq!(second.borrow().slots[1].1.state, SlotState::Unforced);
    assert_int(eng.force_slot(&second, "fallback"), 2);
    for (inst, path) in [(&first, "root.first"), (&second, "root.second")] {
        assert!(matches!(eng.force_slot(inst, "where"), Ok(Value::Str(s)) if s.as_ref() == path));
    }
    let supplied = bind(
        &eng,
        &rt,
        &scope,
        "supplied",
        vec![
            ("provided".into(), Value::Int(Num::from(3))),
            ("fallback".into(), Value::Int(Num::from(4))),
            ("where".into(), Value::Str("root.supplied".into())),
        ],
    );
    assert!(matches!(&*descriptor(&supplied, 1), Compute::Check { .. }));
    assert!(matches!(
        &*descriptor(&supplied, 2),
        Compute::Derived {
            supplied: Some(_),
            ..
        }
    ));
    assert!(!Rc::ptr_eq(
        &descriptor(&first, 2),
        &descriptor(&supplied, 2)
    ));
    assert_int(eng.force_slot(&supplied, "fallback"), 4);
    assert!(eng.force_slot(&supplied, "where").is_ok());
    assert!(eng.env.diagnostics_vec().is_empty());
}

#[test]
fn cache_matches_root_and_effective_environment_allocation_identity() {
    let eng = engine(true);
    let rt = record_type(vec![member("value", MKind::Dflt, literal(7))]);
    let scope = Scope::new("root", None);
    let a = bind(&eng, &rt, &scope, "a", vec![]);
    let b = bind(&eng, &rt, &scope.clone(), "b", vec![]);
    assert!(Rc::ptr_eq(&descriptor(&a, 0), &descriptor(&b, 0)));
    let equal_text = Scope::new("root", None);
    assert!(!Rc::ptr_eq(&scope.root_name, &equal_text.root_name));
    let c = bind(&eng, &rt, &equal_text, "c", vec![]);
    assert!(!Rc::ptr_eq(&descriptor(&a, 0), &descriptor(&c, 0)));
    let env_a = Env::new();
    let env_b = Env::new();
    let scoped_a = scope.with_menv(Some(env_a.clone()));
    let d = bind(&eng, &rt, &scoped_a, "d", vec![]);
    let e = bind(&eng, &rt, &scoped_a, "e", vec![]);
    assert!(Rc::ptr_eq(&descriptor(&d, 0), &descriptor(&e, 0)));
    assert!(!Rc::ptr_eq(&descriptor(&a, 0), &descriptor(&d, 0)));
    let f = bind(
        &eng,
        &rt,
        &scope.with_menv(Some(env_b.clone())),
        "f",
        vec![],
    );
    assert!(!Rc::ptr_eq(&descriptor(&d, 0), &descriptor(&f, 0)));
    let mut declaration = member("fixed", MKind::Der, literal(8));
    declaration.menv = Some(env_a.clone());
    let fixed = record_type(vec![declaration]);
    let g = bind(&eng, &fixed, &scope, "g", vec![]);
    let h = bind(&eng, &fixed, &scope.with_menv(Some(env_b)), "h", vec![]);
    assert!(Rc::ptr_eq(&descriptor(&g, 0), &descriptor(&h, 0)));
    assert!(
        matches!(&*descriptor(&h, 0), Compute::Derived { menv: Some(e), .. } if Rc::ptr_eq(e, &env_a))
    );
}

#[test]
fn shared_reference_defaults_and_derivations_keep_each_navigation_scope() {
    let eng = engine(true);
    let members = [MKind::Dflt, MKind::Der]
        .into_iter()
        .enumerate()
        .map(|(i, kind)| {
            let mut m = member(&format!("r{i}"), kind, Rc::new(Expr::Ctx("$this".into())));
            m.ty = Some(ty(RTk::Ref(ty(RTk::Any))));
            m
        })
        .collect();
    let rt = record_type(members);
    let scope = Scope::new("root", None);
    let value = |result: R<Value>| match result {
        Ok(value) => value,
        Err(Fail::Eval(error)) => panic!("navigation evaluation: {}", error.msg),
        Err(Fail::Defer) => panic!("navigation unexpectedly deferred"),
        Err(Fail::Taint) => panic!("navigation tainted: {:?}", eng.env.diagnostics_vec()),
    };
    let parent_type = record_type(
        ["a", "b"]
            .into_iter()
            .map(|name| {
                let mut m = member(name, MKind::Req, literal(0));
                m.ty = Some(rt.clone());
                m
            })
            .collect(),
    );
    // Canonical navigation traverses bound runtime containers, not raw JSON.
    // Binding the parent also supplies each child's real enclosing instance.
    let Value::Rec(parent) = value(
        eng.bind(
            Value::JObj(Rc::new(
                ["a", "b"]
                    .into_iter()
                    .map(|name| (name.into(), Value::JObj(Rc::new(vec![]))))
                    .collect(),
            )),
            &parent_type,
            &[Seg::Name(scope.root_name.clone())],
            None,
            &scope,
        ),
    ) else {
        panic!("bound parent record");
    };
    eng.env.set_root("root", Value::Rec(parent.clone()));
    let Value::Rec(a) = value(eng.force_slot(&parent, "a")) else {
        panic!("bound child a");
    };
    let Value::Rec(b) = value(eng.force_slot(&parent, "b")) else {
        panic!("bound child b");
    };
    for i in 0..2 {
        assert!(Rc::ptr_eq(&descriptor(&a, i), &descriptor(&b, i)));
        for (inst, expected_text) in [(&a, "root.a"), (&b, "root.b")] {
            let expected = inst.borrow().path.clone();
            assert_eq!(path_str(&expected, None), expected_text);
            let actual = value(eng.force_slot(inst, &format!("r{i}")));
            let Value::Ref(path) = actual else {
                panic!("expected reference for {expected_text}.r{i}, got {actual:?}");
            };
            assert_eq!(path_str(&path, None), expected_text);
            assert!(path == expected);
            assert!(
                matches!(value(eng.resolve_canonical(&path)), Value::Rec(target) if Rc::ptr_eq(&target, inst)),
                "reference must resolve to its own child instance"
            );
        }
    }
    assert!(eng.env.diagnostics_vec().is_empty());
}

#[test]
fn slot_cow_preserves_snapshots_and_dissociates_a_lone_cached_descriptor() {
    let eng = engine(true);
    let rt = record_type(vec![member("value", MKind::Dflt, literal(7))]);
    let scope = Scope::new("root", None);
    let a = bind(&eng, &rt, &scope, "a", vec![]);
    let b = bind(&eng, &rt, &scope, "b", vec![]);
    let snapshot = descriptor(&a, 0);
    {
        let mut borrowed = a.borrow_mut();
        let Compute::Default {
            expr,
            op,
            root_name,
            menv,
            ..
        } = borrowed.slots[0].1.computation_mut().unwrap()
        else {
            panic!("default");
        };
        *expr = literal(99);
        *op = None;
        *root_name = "changed".into();
        *menv = Some(Env::new());
    }
    assert!(!Rc::ptr_eq(&snapshot, &descriptor(&a, 0)));
    let c = bind(&eng, &rt, &scope, "c", vec![]);
    assert!(Rc::ptr_eq(&snapshot, &descriptor(&c, 0)));
    assert_int(eng.force_slot(&a, "value"), 99);
    assert_int(eng.force_slot(&b, "value"), 7);
    assert_int(eng.force_slot(&c, "value"), 7);
    let sole_rt = record_type(vec![member("value", MKind::Dflt, literal(8))]);
    let sole = bind(&eng, &sole_rt, &scope, "sole", vec![]);
    let weak = Rc::downgrade(sole.borrow().slots[0].1.compute.as_ref().unwrap());
    {
        let mut borrowed = sole.borrow_mut();
        let Compute::Default { expr, op, .. } = borrowed.slots[0].1.computation_mut().unwrap()
        else {
            panic!("default");
        };
        *expr = literal(44);
        *op = None;
    }
    assert!(
        weak.upgrade().is_none(),
        "make_mut dissociates cached weak identity"
    );
    let next = bind(&eng, &sole_rt, &scope, "next", vec![]);
    assert_int(eng.force_slot(&sole, "value"), 44);
    assert_int(eng.force_slot(&next, "value"), 8);
}

#[test]
fn declaration_index_and_reentrant_schema_snapshot_boundaries_are_preserved() {
    let eng = engine(true);
    let scope = Scope::new("root", None);
    let duplicate = record_type(vec![
        member("same", MKind::Dflt, literal(1)),
        member("same", MKind::Dflt, literal(2)),
    ]);
    let a = bind(&eng, &duplicate, &scope, "a", vec![]);
    let b = bind(&eng, &duplicate, &scope, "b", vec![]);
    assert!(!Rc::ptr_eq(&descriptor(&a, 0), &descriptor(&a, 1)));
    for i in 0..2 {
        assert!(Rc::ptr_eq(&descriptor(&a, i), &descriptor(&b, i)));
    }
    let rt = record_type(vec![
        member("trigger", MKind::Req, literal(0)),
        member("value", MKind::Dflt, literal(7)),
    ]);
    let weak_engine = Rc::downgrade(&eng);
    let weak_type = Rc::downgrade(&rt);
    let inner = Rc::new(RefCell::new(None));
    let returned = inner.clone();
    let captured_scope = scope.clone();
    *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
        let eng = weak_engine.upgrade().unwrap();
        let rt = weak_type.upgrade().unwrap();
        let RTk::Rec(rec) = &rt.k else {
            panic!("record type");
        };
        Rc::make_mut(&mut rec.members.borrow_mut())[1].dflt = Some(literal(9));
        *returned.borrow_mut() = Some(bind(
            &eng,
            &rt,
            &captured_scope,
            "inner",
            vec![("trigger".into(), Value::Int(Num::from(1)))],
        ));
        None
    }));
    let outer = bind(&eng, &rt, &scope, "outer", vec![]);
    *eng.env.tagger.borrow_mut() = None;
    let inner = inner.borrow_mut().take().unwrap();
    assert!(!Rc::ptr_eq(&descriptor(&outer, 1), &descriptor(&inner, 1)));
    assert_int(eng.force_slot(&outer, "value"), 7);
    assert_int(eng.force_slot(&inner, "value"), 9);
    assert_eq!(eng.env.diagnostics_vec().len(), 1);
}

#[test]
fn weak_plan_cache_does_not_root_shared_descriptor_environment_cycles() {
    let eng = engine(true);
    let rt = record_type(vec![member("value", MKind::Dflt, literal(7))]);
    let context = Env::new();
    let scope = Scope::new("root", Some(context.clone()));
    let a = bind(&eng, &rt, &scope, "a", vec![]);
    let b = bind(&eng, &rt, &scope, "b", vec![]);
    context.registry_push(a.clone());
    context.registry_push(b.clone());
    let snapshot = descriptor(&a, 0);
    assert!(Rc::ptr_eq(&snapshot, &descriptor(&b, 0)));
    let weak_context = Rc::downgrade(&context);
    let weak_a = Rc::downgrade(&a);
    let weak_b = Rc::downgrade(&b);
    eng.env.registry_clear();
    drop((a, b, scope, context));
    collect_cycles();
    assert!(weak_a.upgrade().is_some() && weak_b.upgrade().is_some());
    assert_eq!(weak_context.upgrade().unwrap().registry_snapshot().len(), 2);
    drop(snapshot);
    collect_cycles();
    assert!(weak_a.upgrade().is_none() && weak_b.upgrade().is_none());
    assert!(weak_context.upgrade().is_none());
    assert!(
        eng.programs.borrow().is_some(),
        "Programs remain alive with weak cache only"
    );
}

#[test]
fn last_descriptor_snapshot_releases_native_capture_after_programs_drop() {
    struct Witness {
        drops: Rc<Cell<usize>>,
        observer: Weak<Engine>,
    }
    impl Drop for Witness {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
            let eng = self.observer.upgrade().unwrap();
            assert_int(
                eng.bind(
                    Value::Int(Num::from(5)),
                    &ty(RTk::Any),
                    &[],
                    None,
                    &Scope::new("other", None),
                ),
                5,
            );
            collect_cycles();
        }
    }
    let observer = engine(false);
    let eng = engine(true);
    let drops = Rc::new(Cell::new(0));
    let witness = Witness {
        drops: drops.clone(),
        observer: Rc::downgrade(&observer),
    };
    let native = Value::native(move |_| {
        let _ = &witness;
        Ok(Value::Null)
    });
    let rt = record_type(vec![member(
        "native",
        MKind::Dflt,
        Rc::new(Expr::Lit(native)),
    )]);
    let scope = Scope::new("root", None);
    let a = bind(&eng, &rt, &scope, "a", vec![]);
    let b = bind(&eng, &rt, &scope, "b", vec![]);
    let snapshot = descriptor(&a, 0);
    assert!(Rc::ptr_eq(&snapshot, &descriptor(&b, 0)));
    let weak = Rc::downgrade(&snapshot);
    eng.env.registry_clear();
    drop((a, b, rt));
    let programs = eng.programs.borrow_mut().take();
    drop(programs);
    collect_cycles();
    assert_eq!(
        drops.get(),
        0,
        "external descriptor snapshot still owns expression/program"
    );
    drop(snapshot);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        drops.get(),
        1,
        "Weak descriptor allocation does not retain native capture"
    );
}

#[test]
fn non_program_bindings_keep_independent_default_and_derived_descriptors() {
    let eng = engine(false);
    let scope = Scope::new("root", None);
    let rt = record_type(vec![
        member("a", MKind::Dflt, literal(1)),
        member("b", MKind::Der, literal(2)),
    ]);
    let a = bind(&eng, &rt, &scope, "a", vec![]);
    let b = bind(&eng, &rt, &scope, "b", vec![]);
    for i in 0..2 {
        assert!(!Rc::ptr_eq(&descriptor(&a, i), &descriptor(&b, i)));
    }
    assert_int(eng.force_slot(&a, "a"), 1);
    assert_int(eng.force_slot(&b, "b"), 2);
}
