//! Rust-specific descriptor ownership and public mutation contracts.
use super::*;
use crate::engine::Engine;
use crate::qengine::programs::Programs;

fn record() -> Rc<RefCell<RecInst>> {
    record_instance(RecInst {
        type_name: None,
        rt: ty(RTk::Any),
        path: Rc::new(vec![Seg::Name("root".into())]),
        ps: RefCell::new(None),
        parent: None,
        slots: Vec::new(),
        entry_order: Vec::new().into(),
        extras: Vec::new(),
        menv: None,
    })
}

fn slot(compute: Option<Rc<Compute>>) -> Slot {
    Slot {
        kind: MKind::Req,
        hidden: false,
        state: SlotState::Ok,
        value: Value::Null,
        compute,
    }
}

fn check(raw: Value) -> Compute {
    Compute::Check {
        raw,
        types: Rc::new(vec![]),
        name: "old".into(),
        root_name: "root".into(),
        menv: None,
    }
}

#[test]
fn descriptor_snapshot_and_mutation_preserve_prior_capture() {
    let mut slot = slot(Some(Rc::new(check(Value::Int(Num::from(7))))));
    let original = slot.computation_snapshot().unwrap();
    let snapshot = slot.computation_snapshot().unwrap();
    assert!(Rc::ptr_eq(&original, &snapshot));
    let Compute::Check {
        name,
        root_name,
        raw,
        ..
    } = slot.computation_mut().unwrap()
    else {
        panic!("check descriptor");
    };
    *name = "new".into();
    Rc::make_mut(root_name).make_ascii_uppercase();
    *raw = Value::Int(Num::from(9));
    let Compute::Check {
        name,
        root_name,
        raw,
        ..
    } = &*original
    else {
        panic!("captured check descriptor");
    };
    assert_eq!(name.as_ref(), "old");
    assert_eq!(root_name.as_ref(), "root");
    assert!(matches!(raw, Value::Int(n) if n.to_i64() == Some(7)));
    assert!(!Rc::ptr_eq(&original, slot.compute.as_ref().unwrap()));
    assert!(matches!(
        slot.computation(),
        Some(Compute::Check { name, root_name, .. })
            if name.as_ref() == "new" && root_name.as_ref() == "ROOT"
    ));
    slot.set_computation(None);
    assert!(slot.computation().is_none());
    assert_eq!(
        slot.state,
        SlotState::Ok,
        "replacement does not reset cached state"
    );
    assert!(matches!(slot.value, Value::Null));
    slot.set_computation(Some(check(Value::Absent)));
    assert!(matches!(
        slot.computation(),
        Some(Compute::Check {
            raw: Value::Absent,
            ..
        })
    ));
}

#[test]
fn shared_descriptor_captures_are_traced_once_and_external_snapshot_is_a_root() {
    let record = record();
    let weak = Rc::downgrade(&record);
    let snapshot = Rc::new(check(Value::Rec(record.clone())));
    // One descriptor owns one record edge, despite two slots owning it. The
    // external snapshot is a third descriptor owner and must root the cycle.
    record.borrow_mut().slots.extend([
        ("a".into(), slot(Some(snapshot.clone()))),
        ("b".into(), slot(Some(snapshot.clone()))),
    ]);
    drop(record);
    collect_cycles();
    {
        let live = weak
            .upgrade()
            .expect("external descriptor preserves captured record");
        assert_eq!(live.borrow().slots.len(), 2);
        let borrowed = live.borrow_mut();
        collect_cycles();
        assert_eq!(
            borrowed.slots.len(),
            2,
            "failed collector borrow is conservative"
        );
    }
    drop(snapshot);
    collect_cycles();
    assert!(
        weak.upgrade().is_none(),
        "abandoned shared-descriptor cycle is released"
    );
}

fn member(name: &str, kind: MKind) -> Member {
    Member {
        kind,
        name: name.into(),
        hidden: false,
        ty: Some(ty(RTk::Prim("int".into()))),
        conj: None,
        dflt: (kind == MKind::Dflt).then(|| Rc::new(Expr::Lit(Value::Int(Num::from(7))))),
        expr: (kind == MKind::Der).then(|| Rc::new(Expr::Lit(Value::Int(Num::from(8))))),
        menv: None,
    }
}

fn record_type(members: Vec<Member>) -> RT {
    let rec = rec_type(false);
    *rec.members.borrow_mut() = Rc::new(members);
    ty(RTk::Rec(rec))
}

fn bind_native(
    eng: &Engine,
    rt: &RT,
    sc: &Scope,
    at: &str,
    entries: Vec<(String, Value)>,
) -> Rc<RefCell<RecInst>> {
    let Value::Rec(inst) = eng
        .bind(
            Value::JObj(Rc::new(entries)),
            rt,
            &[Seg::Name(sc.root_name.clone()), Seg::Name(at.into())],
            None,
            sc,
        )
        .ok()
        .expect("native record binds")
    else {
        panic!("record result");
    };
    inst
}

fn captured_names(compute: &Compute) -> (&Rc<str>, &Rc<str>) {
    match compute {
        Compute::Check {
            name, root_name, ..
        }
        | Compute::Default {
            name, root_name, ..
        }
        | Compute::Derived {
            name, root_name, ..
        } => (name, root_name),
        Compute::Bridge(_) => panic!("bound member descriptor"),
    }
}

fn assert_int(value: R<Value>, expected: i64) {
    assert!(matches!(value, Ok(Value::Int(n)) if n.to_i64() == Some(expected)));
}

#[test]
fn scope_root_sharing_keeps_captured_scopes_independent_of_cow_mutation() {
    let mut scope = Scope::new("root", None);
    let plain = scope.clone();
    let with_locals = scope.with_locals(Locals::new());
    let with_inst = scope.with_inst(Some(record()));
    let with_menv = scope.with_menv(Some(Env::new()));
    let pre = PreValV {
        expr: Rc::new(Expr::Lit(Value::Null)),
        scope: scope.clone(),
    };
    let closure = Closure {
        params: vec![],
        body: Rc::new(Expr::Lit(Value::Null)),
        scope: scope.clone(),
    };
    let snapshots = [
        &plain,
        &with_locals,
        &with_inst,
        &with_menv,
        &pre.scope,
        &closure.scope,
    ];
    for captured in snapshots {
        assert!(Rc::ptr_eq(&scope.root_name, &captured.root_name));
    }
    Rc::make_mut(&mut scope.root_name).make_ascii_uppercase();
    assert_eq!(scope.root_name.as_ref(), "ROOT");
    for captured in snapshots {
        assert_eq!(captured.root_name.as_ref(), "root");
        assert!(!Rc::ptr_eq(&scope.root_name, &captured.root_name));
    }
}

#[test]
fn compiled_names_are_shared_but_replaced_schema_keeps_bound_descriptors() {
    let eng = Engine::bare(Env::new());
    let programs = Rc::new(Programs::default());
    *eng.programs.borrow_mut() = Some(programs.clone());
    let rt = record_type(vec![
        member("provided", MKind::Req),
        member("fallback", MKind::Dflt),
        member("derived", MKind::Der),
    ]);
    let scope = Scope::new("root", None);
    let first = bind_native(
        &eng,
        &rt,
        &scope,
        "first",
        vec![("provided".into(), Value::Int(Num::from(1)))],
    );
    let second = bind_native(
        &eng,
        &rt,
        &scope,
        "second",
        vec![("provided".into(), Value::Int(Num::from(2)))],
    );
    let schema = programs.schema(&rt);
    assert_eq!(programs.compiled_schemas.get(), 1);
    {
        let a = first.borrow();
        let b = second.borrow();
        for (index, ((an, av), (bn, bv))) in a.slots.iter().zip(&b.slots).enumerate() {
            let ac = av.compute.as_ref().unwrap();
            let bc = bv.compute.as_ref().unwrap();
            let (acn, acr) = captured_names(ac);
            let (bcn, bcr) = captured_names(bc);
            assert!(Rc::ptr_eq(an, &schema.members[index].name));
            assert!(Rc::ptr_eq(an, bn));
            assert!(Rc::ptr_eq(an, acn));
            assert!(Rc::ptr_eq(bn, bcn));
            assert!(Rc::ptr_eq(acr, &scope.root_name));
            assert!(Rc::ptr_eq(bcr, &scope.root_name));
            assert_eq!(
                Rc::ptr_eq(ac, bc),
                index != 0,
                "only Default and unsupplied Derived share matching plan/context metadata"
            );
        }
    }
    drop(schema);
    let RTk::Rec(rec) = &rt.k else {
        panic!("record type");
    };
    {
        let mut members = rec.members.borrow_mut();
        let members = Rc::make_mut(&mut members);
        members[0].name = "renamed".into();
        members[0].ty = Some(ty(RTk::Prim("string".into())));
        members[1].dflt = Some(Rc::new(Expr::Lit(Value::Int(Num::from(9)))));
        members[2].expr = Some(Rc::new(Expr::Lit(Value::Int(Num::from(10)))));
    }
    let third = bind_native(
        &eng,
        &rt,
        &scope,
        "third",
        vec![("renamed".into(), Value::Str("next".into()))],
    );
    assert_eq!(programs.compiled_schemas.get(), 2);
    assert!(first.borrow().slot("renamed").is_none());
    assert!(third.borrow().slot("provided").is_none());
    for (inst, expected) in [(&first, 1), (&second, 2)] {
        assert_int(eng.force_slot(inst, "provided"), expected);
        assert_int(eng.force_slot(inst, "fallback"), 7);
        assert_int(eng.force_slot(inst, "derived"), 8);
    }
    assert!(matches!(
        eng.force_slot(&third, "renamed"),
        Ok(Value::Str(s)) if s.as_ref() == "next"
    ));
    assert_int(eng.force_slot(&third, "fallback"), 9);
    assert_int(eng.force_slot(&third, "derived"), 10);
    assert!(eng.env.diagnostics_vec().is_empty());
}

#[test]
fn compiled_duplicate_member_names_preserve_slots_and_first_name_lookup() {
    let eng = Engine::bare(Env::new());
    let programs = Rc::new(Programs::default());
    *eng.programs.borrow_mut() = Some(programs.clone());
    let rt = record_type(vec![
        member("same", MKind::Req),
        member("same", MKind::Dflt),
        member("last", MKind::Opt),
    ]);
    let inst = bind_native(
        &eng,
        &rt,
        &Scope::new("root", None),
        "duplicates",
        vec![
            ("same".into(), Value::Int(Num::from(4))),
            ("same".into(), Value::Int(Num::from(9))),
        ],
    );
    let schema = programs.schema(&rt);
    {
        let b = inst.borrow();
        assert_eq!(b.entry_order(), ["same", "same"]);
        assert_eq!(
            b.slots
                .iter()
                .map(|(n, s)| (n.as_ref(), s.kind))
                .collect::<Vec<_>>(),
            [
                ("same", MKind::Req),
                ("same", MKind::Dflt),
                ("last", MKind::Opt)
            ]
        );
        for (index, (name, slot)) in b.slots.iter().enumerate() {
            assert!(Rc::ptr_eq(name, &schema.members[index].name));
            if index < 2 {
                assert!(matches!(
                    slot.computation(),
                    Some(Compute::Check { raw: Value::Int(n), .. }) if n.to_i64() == Some(9)
                ));
            }
        }
    }
    assert_int(eng.force_slot(&inst, "same"), 9);
    let b = inst.borrow();
    assert_eq!(b.slots[0].1.state, SlotState::Ok);
    assert_eq!(b.slots[1].1.state, SlotState::Unforced);
    assert_eq!(b.slots[2].1.state, SlotState::Absent);
    assert_eq!(b.slot("same").unwrap().kind, MKind::Req);
    assert!(eng.env.diagnostics_vec().is_empty());
}

#[test]
fn reentrant_schema_replacement_does_not_change_outer_binding_names() {
    let eng = Engine::bare(Env::new());
    let programs = Rc::new(Programs::default());
    *eng.programs.borrow_mut() = Some(programs.clone());
    let rt = record_type(vec![
        member("trigger", MKind::Req),
        member("stable", MKind::Req),
    ]);
    let weak_engine = Rc::downgrade(&eng);
    let weak_type = Rc::downgrade(&rt);
    let inner = Rc::new(RefCell::new(None));
    let inner_result = inner.clone();
    let called = Rc::new(Cell::new(false));
    let callback_called = called.clone();
    *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
        assert!(
            !callback_called.replace(true),
            "only the outer missing member reports"
        );
        let eng = weak_engine.upgrade().unwrap();
        let rt = weak_type.upgrade().unwrap();
        let RTk::Rec(rec) = &rt.k else {
            panic!("record type");
        };
        {
            let mut members = rec.members.borrow_mut();
            let members = Rc::make_mut(&mut members);
            members[0].name = "inner_trigger".into();
            members[1].name = "inner_stable".into();
        }
        *inner_result.borrow_mut() = Some(bind_native(
            &eng,
            &rt,
            &Scope::new("root", None),
            "inner",
            vec![
                ("inner_trigger".into(), Value::Int(Num::from(6))),
                ("inner_stable".into(), Value::Int(Num::from(7))),
            ],
        ));
        None
    }));
    let outer = bind_native(
        &eng,
        &rt,
        &Scope::new("root", None),
        "outer",
        vec![("stable".into(), Value::Int(Num::from(5)))],
    );
    *eng.env.tagger.borrow_mut() = None;
    assert!(called.get());
    assert_eq!(programs.compiled_schemas.get(), 2);
    let inner = inner.borrow_mut().take().expect("reentrant bind completed");
    assert_eq!(
        outer
            .borrow()
            .slots
            .iter()
            .map(|(n, _)| n.as_ref())
            .collect::<Vec<_>>(),
        ["trigger", "stable"]
    );
    assert_eq!(
        inner
            .borrow()
            .slots
            .iter()
            .map(|(n, _)| n.as_ref())
            .collect::<Vec<_>>(),
        ["inner_trigger", "inner_stable"]
    );
    assert_int(eng.force_slot(&outer, "stable"), 5);
    assert_int(eng.force_slot(&inner, "inner_trigger"), 6);
    assert_int(eng.force_slot(&inner, "inner_stable"), 7);
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E4002"));
    assert_eq!(diagnostics[0].path, "root.outer.trigger");
}
