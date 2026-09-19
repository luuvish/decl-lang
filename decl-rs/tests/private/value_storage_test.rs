//! Rust-native ownership boundaries of compact quantity, pattern and range values.
use super::*;
use crate::engine::Engine;
use std::rc::Weak;

#[test]
fn shared_text_path_bridges_preserve_character_identity_and_isolate_cow() {
    for spelling in ["", "lower_key", "한글[lower]😀"] {
        let chars: Rc<str> = Rc::from(spelling);
        let weak = Rc::downgrade(&chars);
        let parent = record();
        parent.borrow_mut().path = Rc::new(vec![Seg::Name("root".into())]);
        let child = record();
        child.borrow_mut().path = Rc::new(vec![
            Seg::Name("root".into()),
            Seg::Name("items".into()),
            Seg::Key(chars.clone()),
        ]);
        child.borrow_mut().parent = Some(parent.clone());
        let eng = Engine::bare(Env::new());
        let scope = Scope::new("root", None).with_inst(Some(child.clone()));
        let original = eng.context_value("$key", &scope).ok().unwrap();
        let Value::Str(text) = &original else {
            panic!("a map element key is text");
        };
        assert!(Rc::ptr_eq(text.as_rc(), &chars));
        let stable = original.clone();
        let mut changed = original.clone();
        let expression = Rc::new(Expr::Index {
            x: Rc::new(Expr::Lit(Value::Segs(Rc::new(vec![Seg::Name(
                "missing".into(),
            )])))),
            i: Rc::new(Expr::Lit(stable.clone())),
        });
        let path = eng.eval_place(&expression, &scope).ok().unwrap().unwrap();
        let Some(Seg::Key(key)) = path.last() else {
            panic!("string navigation retains a map key");
        };
        assert!(
            Rc::ptr_eq(key, &chars),
            "navigation must not copy characters"
        );
        let Value::Str(text) = &mut changed else {
            unreachable!();
        };
        text.make_mut().make_ascii_uppercase();
        assert!(!Rc::ptr_eq(text.as_rc(), &chars));
        assert_eq!(text.as_ref(), spelling.to_ascii_uppercase());
        assert!(value_eq(&original, &stable));
        assert!(matches!(&stable, Value::Str(s) if s.as_ref() == spelling));
        assert_eq!(key.as_ref(), spelling);

        // The path is independently mutable even while Values retain its old text.
        let mut exported = key.clone();
        Rc::make_mut(&mut exported).make_ascii_uppercase();
        assert!(!Rc::ptr_eq(&exported, &chars));
        drop(exported);
        drop(chars);
        drop(scope);
        drop(child);
        drop(parent);
        drop(expression);
        drop(original);
        drop(stable);
        drop(eng);
        assert!(
            weak.upgrade().is_some(),
            "the returned path still owns the key"
        );
        drop(path);
        assert!(
            weak.upgrade().is_none(),
            "the changed Value owns a separate key"
        );
        assert!(matches!(changed, Value::Str(s) if s.as_ref() == spelling.to_ascii_uppercase()));
    }
}

#[test]
fn shared_text_weak_only_cow_and_consuming_conversions_keep_owner_boundaries() {
    for spelling in ["", "ascii", "한글-ascii😀"] {
        let chars: Rc<str> = Rc::from(spelling);
        let old_weak = Rc::downgrade(&chars);
        let mut text = SharedText::from_rc(chars);
        assert_eq!(old_weak.strong_count(), 1);
        text.make_mut().make_ascii_uppercase();
        assert!(
            old_weak.upgrade().is_none(),
            "weak-only character identity dissociates"
        );
        let stable = text.clone();
        let weak = Rc::downgrade(text.as_rc());
        assert_eq!(weak.strong_count(), 1, "clones share one thin descriptor");
        let moved = text.into_rc();
        assert!(Rc::ptr_eq(&moved, stable.as_rc()));
        assert_eq!(weak.strong_count(), 2);
        let moved_stable = stable.into_rc();
        assert!(Rc::ptr_eq(&moved, &moved_stable));
        assert_eq!(
            weak.strong_count(),
            2,
            "unique conversion moves its character owner"
        );
        let mut table = HashMap::new();
        table.insert(SharedText::from_rc(moved.clone()), 7);
        assert_eq!(table.get(spelling.to_ascii_uppercase().as_str()), Some(&7));
        assert_eq!(
            table.insert(SharedText::from(spelling.to_ascii_uppercase()), 9),
            Some(7)
        );
        assert_eq!(table.len(), 1, "text keys compare and hash by contents");
        drop(table);
        drop(moved);
        assert!(weak.upgrade().is_some());
        drop(moved_stable);
        assert!(weak.upgrade().is_none());
    }
}

struct NativeCapture {
    owner: Rc<RefCell<RecInst>>,
    calls: Rc<Cell<usize>>,
    drops: Rc<RefCell<Vec<(bool, usize)>>>,
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        let borrowed = self.owner.try_borrow_mut().is_err();
        let reclaimed = collect_cycles();
        self.drops.borrow_mut().push((borrowed, reclaimed));
    }
}

#[test]
fn native_function_clones_share_one_capture_until_the_last_callable_owner() {
    collect_cycles();
    let owner = record();
    owner.borrow_mut().parent = Some(owner.clone());
    let weak_owner = Rc::downgrade(&owner);
    let calls = Rc::new(Cell::new(0));
    let drops = Rc::new(RefCell::new(Vec::new()));
    let capture = NativeCapture {
        owner: owner.clone(),
        calls: calls.clone(),
        drops: drops.clone(),
    };
    let original = Value::native(move |args| {
        let _ = &capture;
        capture.calls.set(capture.calls.get() + 1);
        assert!(args.is_empty());
        Ok(Value::Rec(capture.owner.clone()))
    });
    let stable = original.clone();
    let other = original.clone();
    let Value::Nat(f) = &original else {
        unreachable!();
    };
    let weak_callable = Rc::downgrade(f);
    assert_eq!(
        Rc::strong_count(&owner),
        3,
        "self edge, caller, one environment"
    );
    let Value::Nat(g) = &stable else {
        unreachable!();
    };
    assert!(Rc::ptr_eq(f, g));
    let returned = g(&[]).ok().unwrap();
    assert!(matches!(&returned, Value::Rec(r) if Rc::ptr_eq(r, &owner)));
    assert_eq!(calls.get(), 1);
    drop(returned);
    drop(owner);
    drop(original);
    drop(stable);
    assert!(drops.borrow().is_empty());
    assert_eq!(collect_cycles(), 0, "the last callable retains its capture");
    drop(other);
    assert!(weak_callable.upgrade().is_none());
    assert_eq!(
        calls.get(),
        1,
        "destruction never invokes the native function"
    );
    assert_eq!(*drops.borrow(), vec![(false, 0)]);
    assert!(
        collect_cycles() >= 1,
        "the capture's last owner has now been released"
    );
    assert!(weak_owner.upgrade().is_none());
}

#[test]
fn quantity_clones_share_dimensions_but_keep_magnitudes_independent() {
    let original = Value::quantity("Length*Time^-1", -0.0);
    let mut changed = original.clone();
    let (Value::Q(before), Value::Q(after)) = (&original, &changed) else {
        panic!("quantity variants");
    };
    assert!(Rc::ptr_eq(&before.dim, &after.dim));
    assert!(!std::ptr::eq(&**before, &**after));
    let weak_original_dimension = Rc::downgrade(&before.dim);
    changed.as_quantity_mut().unwrap().value = 7.5;
    assert_eq!(
        original.as_quantity().unwrap().1.to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(changed.as_quantity(), Some(("Length*Time^-1", 7.5)));

    changed.quantity_dimension_mut().unwrap().push_str("*Mass");
    assert_eq!(original.as_quantity().unwrap().0, "Length*Time^-1");
    assert_eq!(changed.as_quantity(), Some(("Length*Time^-1*Mass", 7.5)));
    let (Value::Q(before), Value::Q(after)) = (&original, &changed) else {
        panic!("quantity variants");
    };
    assert!(!Rc::ptr_eq(&before.dim, &after.dim));
    drop(original);
    assert!(weak_original_dimension.upgrade().is_none());
    assert_eq!(changed.as_quantity(), Some(("Length*Time^-1*Mass", 7.5)));
}

#[test]
fn unique_quantity_dimension_mutation_handles_weak_owners_and_nan() {
    let nan = f64::from_bits(0x7ff8_0000_0000_1234);
    let mut value = Value::quantity("차원", nan);
    let Value::Q(quantity) = &value else {
        panic!("quantity");
    };
    let weak = Rc::downgrade(&quantity.dim);
    assert_eq!(Rc::strong_count(&quantity.dim), 1);
    value.quantity_dimension_mut().unwrap().push_str("^-1");
    assert!(
        weak.upgrade().is_none(),
        "COW dissociates the old weak identity"
    );
    let (dimension, magnitude) = value.as_quantity().unwrap();
    assert_eq!(dimension, "차원^-1");
    assert_eq!(magnitude.to_bits(), nan.to_bits());
    assert!(!value_eq(&value, &value));
    assert!(!value_eq(&value, &value.clone()));

    let mut other = Value::Null;
    assert!(other.as_quantity().is_none());
    assert!(other.as_quantity_mut().is_none());
    assert!(other.quantity_dimension_mut().is_none());
    assert!(other.as_pattern().is_none());
    assert!(other.as_range().is_none());
    assert!(other.as_range_mut().is_none());
}

#[test]
fn pattern_clones_share_text_without_reflexive_equality_or_shared_replacement() {
    let original = Value::pattern("한글[0-9]+😀");
    let stable = original.clone();
    let mut replaced = original.clone();
    let (Value::Pat(a), Value::Pat(b), Value::Pat(c)) = (&original, &stable, &replaced) else {
        panic!("patterns");
    };
    assert!(Rc::ptr_eq(a.as_rc(), b.as_rc()) && Rc::ptr_eq(b.as_rc(), c.as_rc()));
    let weak = Rc::downgrade(a.as_rc());
    assert!(!value_eq(&original, &original));
    assert!(!value_eq(&original, &replaced));
    replaced = Value::pattern("replacement");
    assert_eq!(stable.as_pattern(), Some("한글[0-9]+😀"));
    assert_eq!(replaced.as_pattern(), Some("replacement"));
    drop(original);
    assert!(weak.upgrade().is_some());
    drop(stable);
    assert!(weak.upgrade().is_none());
    assert_eq!(replaced.as_pattern(), Some("replacement"));
}

#[test]
fn range_clone_owns_distinct_nested_payloads_and_mutations() {
    let original = Value::range(
        Value::quantity("Time", -0.0),
        Value::range(Value::Int(7.into()), Value::Int(11.into()), false),
        true,
    );
    let mut changed = original.clone();
    assert!(!std::ptr::eq(
        original.as_range().unwrap(),
        changed.as_range().unwrap()
    ));
    assert!(!std::ptr::eq(
        original.as_range().unwrap().hi.as_range().unwrap(),
        changed.as_range().unwrap().hi.as_range().unwrap(),
    ));
    assert!(!value_eq(&original, &original));
    assert!(!value_eq(&original, &changed));
    {
        let range = changed.as_range_mut().unwrap();
        range.lo.quantity_dimension_mut().unwrap().push_str("^-1");
        let Value::Q(quantity) = &mut range.lo else {
            panic!("quantity lower bound");
        };
        quantity.value = 4.5;
        range.excl = false;
        let nested = range.hi.as_range_mut().unwrap();
        nested.lo = Value::Int(17.into());
        nested.excl = true;
    }
    let before = original.as_range().unwrap();
    assert!(before.excl);
    let (dimension, magnitude) = before.lo.as_quantity().unwrap();
    assert_eq!(dimension, "Time");
    assert_eq!(magnitude.to_bits(), (-0.0_f64).to_bits());
    let nested = before.hi.as_range().unwrap();
    assert!(value_eq(&nested.lo, &Value::Int(7.into())));
    assert!(value_eq(&nested.hi, &Value::Int(11.into())));
    assert!(!nested.excl);

    let after = changed.as_range().unwrap();
    assert!(!after.excl);
    assert_eq!(after.lo.as_quantity(), Some(("Time^-1", 4.5)));
    let nested = after.hi.as_range().unwrap();
    assert!(value_eq(&nested.lo, &Value::Int(17.into())));
    assert!(value_eq(&nested.hi, &Value::Int(11.into())));
    assert!(nested.excl);
}

fn record() -> Rc<RefCell<RecInst>> {
    record_instance(RecInst {
        type_name: None,
        rt: ty(RTk::Prim("int".into())),
        path: Rc::new(vec![]),
        ps: RefCell::new(None),
        parent: None,
        slots: vec![],
        entry_order: vec![].into(),
        extras: vec![],
        menv: None,
    })
}

#[test]
fn range_duplicate_child_edges_preserve_external_clone_then_reclaim_cycle() {
    collect_cycles();
    let holder = record();
    let child = Rc::new(RefCell::new(ArrV {
        items: vec![Value::Rec(holder.clone())],
        path: PrefixPath::default(),
    }));
    let range = Value::range(Value::Arr(child.clone()), Value::Arr(child.clone()), true);
    let retained = range.clone();
    assert_eq!(
        Rc::strong_count(&child),
        5,
        "four endpoint owners and the caller"
    );
    holder.borrow_mut().extras.push(("range".into(), range));
    let weak_holder = Rc::downgrade(&holder);
    let weak_child = Rc::downgrade(&child);
    drop(holder);
    drop(child);
    assert_eq!(
        collect_cycles(),
        0,
        "the external range owns both endpoints"
    );
    {
        let holder = weak_holder
            .upgrade()
            .expect("range child keeps its record alive");
        let holder = holder.borrow();
        let range = holder.extra("range").unwrap().as_range().unwrap();
        let (Value::Arr(lo), Value::Arr(hi)) = (&range.lo, &range.hi) else {
            panic!("array endpoints");
        };
        assert!(Rc::ptr_eq(lo, hi));
        assert_eq!(lo.borrow().items.len(), 1);
        assert!(range.excl);
    }
    drop(retained);
    assert!(collect_cycles() >= 2);
    assert!(weak_holder.upgrade().is_none() && weak_child.upgrade().is_none());
}

#[derive(Debug, PartialEq)]
struct DropEvent {
    label: &'static str,
    owner_alive: bool,
    owner_borrowed: bool,
    reentrant_result: usize,
}

struct EndpointDrop {
    label: &'static str,
    owner: Weak<RefCell<RecInst>>,
    events: Rc<RefCell<Vec<DropEvent>>>,
}

impl Drop for EndpointDrop {
    fn drop(&mut self) {
        let owner = self.owner.upgrade();
        self.events.borrow_mut().push(DropEvent {
            label: self.label,
            owner_alive: owner.is_some(),
            owner_borrowed: owner.as_ref().is_some_and(|r| r.try_borrow().is_err()),
            reentrant_result: collect_cycles(),
        });
    }
}

fn native_endpoint(
    label: &'static str,
    owner: &Rc<RefCell<RecInst>>,
    events: &Rc<RefCell<Vec<DropEvent>>>,
) -> Value {
    let capture = EndpointDrop {
        label,
        owner: Rc::downgrade(owner),
        events: events.clone(),
    };
    Value::native(move |_| {
        let _ = &capture;
        panic!("dropping a range must not execute its native endpoints")
    })
}

#[test]
fn range_native_endpoints_drop_in_order_with_reentrant_collection_suppressed() {
    collect_cycles();
    let holder = record();
    holder.borrow_mut().parent = Some(holder.clone());
    let weak = Rc::downgrade(&holder);
    let events = Rc::new(RefCell::new(vec![]));
    let original = Value::range(
        native_endpoint("lo", &holder, &events),
        native_endpoint("hi", &holder, &events),
        false,
    );
    let copy = original.clone();
    drop(original);
    assert!(
        events.borrow().is_empty(),
        "the clone retains both native owners"
    );
    holder.borrow_mut().extras.push(("range".into(), copy));
    drop(holder);
    assert!(collect_cycles() >= 1);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        *events.borrow(),
        vec![
            DropEvent {
                label: "lo",
                owner_alive: true,
                owner_borrowed: true,
                reentrant_result: 0,
            },
            DropEvent {
                label: "hi",
                owner_alive: true,
                owner_borrowed: true,
                reentrant_result: 0,
            },
        ]
    );
    assert_eq!(collect_cycles(), 0);
    assert_eq!(events.borrow().len(), 2);
}
