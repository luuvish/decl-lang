//! Rust-specific descriptor ownership and public mutation contracts.
use super::*;

fn record() -> Rc<RefCell<RecInst>> {
    record_instance(RecInst {
        type_name: None,
        rt: ty(RTk::Any),
        path: Rc::new(vec![Seg::Name("root".into())]),
        ps: RefCell::new(None),
        parent: None,
        slots: Vec::new(),
        entry_order: Vec::new(),
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
    let Compute::Check { name, raw, .. } = slot.computation_mut().unwrap() else {
        panic!("check descriptor");
    };
    *name = "new".into();
    *raw = Value::Int(Num::from(9));
    let Compute::Check { name, raw, .. } = &*original else {
        panic!("captured check descriptor");
    };
    assert_eq!(name, "old");
    assert!(matches!(raw, Value::Int(n) if n.to_i64() == Some(7)));
    assert!(!Rc::ptr_eq(&original, slot.compute.as_ref().unwrap()));
    assert!(matches!(slot.computation(), Some(Compute::Check { name, .. }) if name == "new"));
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
