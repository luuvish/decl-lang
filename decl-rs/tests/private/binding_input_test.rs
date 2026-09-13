//! Rust-native input snapshots and ownership during collection and record binding.
use super::*;

fn array_type() -> RT {
    ty(RTk::Arr {
        elem: ty(RTk::Any),
        lo: None,
        hi: None,
    })
}

fn map_type() -> RT {
    ty(RTk::Map {
        key: ty(RTk::Prim("string".into())),
        val: ty(RTk::Any),
    })
}

fn callback(run: impl Fn() -> R<Value> + 'static) -> Value {
    Value::PreVal(Rc::new(PreValV {
        expr: Rc::new(Expr::Call {
            fun: Rc::new(Expr::Lit(Value::Nat(Rc::new(move |_| run())))),
            args: vec![],
        }),
        scope: Scope::new("root", None),
    }))
}

fn bind_at(eng: &Engine, raw: Value, rt: &RT) -> R<Value> {
    eng.bind(
        raw,
        rt,
        &[Seg::Name("root".into())],
        None,
        &Scope::new("root", None),
    )
}

#[test]
fn immutable_array_keeps_original_input_alive_across_cow_and_reentry() {
    let eng = Engine::bare(Env::new());
    let external = Rc::new(RefCell::new(Rc::new(vec![Value::Null, Value::Bool(true)])));
    let weak_external = Rc::downgrade(&external);
    let weak_engine = Rc::downgrade(&eng);
    let observed_original = Rc::new(RefCell::new(None));
    let observed = observed_original.clone();
    let first = callback(move || {
        let external = weak_external.upgrade().expect("external input handle");
        let mut input = external.borrow_mut();
        let original = Rc::downgrade(&input);
        let entries = Rc::make_mut(&mut input);
        entries[1] = Value::Bool(false);
        entries.push(Value::Bool(false));
        assert!(
            original.upgrade().is_some(),
            "binding retains the original Rc"
        );
        *observed.borrow_mut() = Some(original);
        drop(input);

        let eng = weak_engine.upgrade().expect("binding engine");
        let nested = bind_at(
            &eng,
            Value::JArr(Rc::new(vec![Value::Bool(false)])),
            &array_type(),
        )?;
        let Value::Arr(nested) = nested else {
            panic!("reentrant array");
        };
        assert_eq!(nested.borrow().items.len(), 1);
        assert!(matches!(nested.borrow().items[0], Value::Bool(false)));
        Ok(Value::Null)
    });
    Rc::make_mut(&mut external.borrow_mut())[0] = first;
    // End the holder borrow before native code can mutate the holder again.
    let raw = Value::JArr(external.borrow().clone());
    let result = bind_at(&eng, raw, &array_type())
        .ok()
        .expect("array binding");
    let Value::Arr(result) = result else {
        panic!("array result");
    };
    let result = result.borrow();
    assert_eq!(result.items.len(), 2);
    assert!(matches!(result.items[0], Value::Null));
    assert!(matches!(result.items[1], Value::Bool(true)));
    assert_eq!(external.borrow().len(), 3);
    assert!(matches!(external.borrow()[1], Value::Bool(false)));
    assert!(observed_original
        .borrow()
        .as_ref()
        .unwrap()
        .upgrade()
        .is_none());
}

#[test]
fn immutable_map_keeps_entry_snapshot_across_cow_mutation() {
    let eng = Engine::bare(Env::new());
    let external = Rc::new(RefCell::new(Rc::new(vec![
        ("first".to_string(), Value::Null),
        ("second".to_string(), Value::Bool(true)),
    ])));
    let weak_external = Rc::downgrade(&external);
    let observed_original = Rc::new(RefCell::new(None));
    let observed = observed_original.clone();
    let first = callback(move || {
        let external = weak_external.upgrade().expect("external map handle");
        let mut input = external.borrow_mut();
        let original = Rc::downgrade(&input);
        let entries = Rc::make_mut(&mut input);
        entries[1] = ("replacement".into(), Value::Bool(false));
        entries.push(("late".into(), Value::Bool(false)));
        assert!(
            original.upgrade().is_some(),
            "binding retains original entries"
        );
        *observed.borrow_mut() = Some(original);
        Ok(Value::Null)
    });
    Rc::make_mut(&mut external.borrow_mut())[0].1 = first;
    let raw = Value::JObj(external.borrow().clone());
    let result = bind_at(&eng, raw, &map_type()).ok().expect("map binding");
    let Value::Map(result) = result else {
        panic!("map result");
    };
    let result = result.borrow();
    assert_eq!(
        result
            .entries
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(matches!(result.get("first"), Some(Value::Null)));
    assert!(matches!(result.get("second"), Some(Value::Bool(true))));
    assert_eq!(external.borrow().len(), 3);
    assert_eq!(external.borrow()[1].0, "replacement");
    assert!(observed_original
        .borrow()
        .as_ref()
        .unwrap()
        .upgrade()
        .is_none());
}

#[test]
fn mutable_array_snapshots_siblings_before_callbacks_without_holding_a_borrow() {
    let eng = Engine::bare(Env::new());
    let input = Rc::new(RefCell::new(ArrV {
        path: Rc::new(PrefixPath::default()),
        items: vec![Value::Null, Value::Bool(true)],
    }));
    let weak_input = Rc::downgrade(&input);
    input.borrow_mut().items[0] = callback(move || {
        let input = weak_input.upgrade().expect("mutable array input");
        let mut input = input.borrow_mut();
        input.items[1] = Value::Bool(false);
        input.items.push(Value::Bool(false));
        Ok(Value::Null)
    });
    let result = bind_at(&eng, Value::Arr(input.clone()), &array_type())
        .ok()
        .expect("mutable input binding");
    let Value::Arr(result) = result else {
        panic!("array result");
    };
    assert_eq!(result.borrow().items.len(), 2);
    assert!(matches!(result.borrow().items[1], Value::Bool(true)));
    assert_eq!(input.borrow().items.len(), 3);
    assert!(matches!(input.borrow().items[1], Value::Bool(false)));
}

#[test]
fn mutable_map_snapshots_removed_entries_before_callbacks() {
    let eng = Engine::bare(Env::new());
    let input = Rc::new(RefCell::new(MapV {
        path: Rc::new(PrefixPath::default()),
        entries: [
            ("first".into(), Value::Null),
            ("second".into(), Value::Bool(true)),
        ]
        .into_iter()
        .collect(),
    }));
    let weak_input = Rc::downgrade(&input);
    input.borrow_mut().set(
        "first".into(),
        callback(move || {
            let input = weak_input.upgrade().expect("mutable map input");
            let mut input = input.borrow_mut();
            input.entries.shift_remove("second");
            input.set("late".into(), Value::Bool(false));
            Ok(Value::Null)
        }),
    );
    let result = bind_at(&eng, Value::Map(input.clone()), &map_type())
        .ok()
        .expect("mutable input binding");
    let Value::Map(result) = result else {
        panic!("map result");
    };
    let result = result.borrow();
    assert_eq!(
        result
            .entries
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(matches!(result.get("second"), Some(Value::Bool(true))));
    assert!(!input.borrow().has("second"));
    assert!(input.borrow().has("late"));
}

#[test]
fn immutable_early_exit_does_not_clone_or_execute_future_siblings() {
    for is_map in [false, true] {
        for defer in [false, true] {
            let eng = Engine::bare(Env::new());
            let future_calls = Rc::new(Cell::new(0));
            let calls = future_calls.clone();
            let future = callback(move || {
                calls.set(calls.get() + 1);
                Ok(Value::Bool(true))
            });
            let Value::PreVal(future_value) = &future else {
                panic!("deferred native value");
            };
            let weak_future = Rc::downgrade(future_value);
            let observed = Rc::new(RefCell::new(Vec::new()));
            let observations = observed.clone();
            let callback_future = weak_future.clone();
            let first = callback(move || {
                observations
                    .borrow_mut()
                    .push(callback_future.strong_count());
                if defer {
                    Err(Fail::Defer)
                } else {
                    err_code("stop before future sibling", "E5008")
                }
            });
            let (raw, rt) = if is_map {
                (
                    Value::JObj(Rc::new(vec![
                        ("first".into(), first),
                        ("future".into(), future),
                    ])),
                    map_type(),
                )
            } else {
                (Value::JArr(Rc::new(vec![first, future])), array_type())
            };
            let original = raw.clone();
            let result = bind_at(&eng, raw, &rt);
            if defer {
                assert!(matches!(result, Err(Fail::Defer)));
            } else {
                let Err(Fail::Eval(error)) = result else {
                    panic!("original failure propagated");
                };
                assert_eq!(error.code.as_deref(), Some("E5008"));
                assert_eq!(error.msg, "stop before future sibling");
            }
            assert_eq!(
                *observed.borrow(),
                [1],
                "only original raw owns the future value"
            );
            assert_eq!(future_calls.get(), 0);
            assert_eq!(weak_future.strong_count(), 1);
            drop(original);
            assert!(
                weak_future.upgrade().is_none(),
                "no iterator snapshot escapes"
            );
            assert!(eng.env.diagnostics_vec().is_empty());
        }
    }
}

#[test]
fn immutable_map_duplicate_keys_keep_callback_and_replacement_order() {
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut entries = vec![];
    for (index, key, value) in [(1, "same", true), (2, "middle", true), (3, "same", false)] {
        let calls = calls.clone();
        entries.push((
            key.into(),
            callback(move || {
                calls.borrow_mut().push(index);
                Ok(Value::Bool(value))
            }),
        ));
    }
    let result = bind_at(&eng, Value::JObj(Rc::new(entries)), &map_type())
        .ok()
        .expect("duplicate native entries binding");
    assert_eq!(*calls.borrow(), [1, 2, 3]);
    let Value::Map(result) = result else {
        panic!("map result");
    };
    let result = result.borrow();
    assert_eq!(
        result
            .entries
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["same", "middle"]
    );
    assert!(matches!(result.get("same"), Some(Value::Bool(false))));
    assert!(matches!(result.get("middle"), Some(Value::Bool(true))));
}

fn record_type(open: bool, required: &[&str]) -> RT {
    let rec = rec_type(open);
    *rec.members.borrow_mut() = Rc::new(
        required
            .iter()
            .map(|name| Member {
                kind: MKind::Req,
                name: (*name).into(),
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

#[test]
fn immutable_record_survives_tagger_cow_and_reentry_before_lazy_values_are_forced() {
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let lazy_calls = calls.clone();
    let lazy = callback(move || {
        lazy_calls.set(lazy_calls.get() + 1);
        Ok(Value::Bool(true))
    });
    let extra_calls = calls.clone();
    let extra = callback(move || {
        extra_calls.set(extra_calls.get() + 1);
        Ok(Value::Bool(true))
    });
    let Value::PreVal(lazy_value) = &lazy else {
        panic!("lazy native value");
    };
    let weak_lazy = Rc::downgrade(lazy_value);
    let Value::PreVal(extra_value) = &extra else {
        panic!("extra native value");
    };
    let weak_extra = Rc::downgrade(extra_value);
    let external = Rc::new(RefCell::new(Rc::new(vec![
        ("lazy".into(), lazy),
        ("extra.key".into(), extra),
    ])));
    let weak_external = Rc::downgrade(&external);
    let weak_engine = Rc::downgrade(&eng);
    let future = weak_lazy.clone();
    let original = Rc::new(RefCell::new(None));
    let observed_original = original.clone();
    let tagger_calls = Rc::new(Cell::new(0));
    let observed_calls = tagger_calls.clone();
    *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
        observed_calls.set(observed_calls.get() + 1);
        assert_eq!(
            future.strong_count(),
            1,
            "before the lazy slot exists, only raw entries own its value; supplied borrows it"
        );
        let external = weak_external.upgrade().expect("external record handle");
        let mut input = external.borrow_mut();
        let original = Rc::downgrade(&input);
        *Rc::make_mut(&mut input) = vec![("replacement".into(), Value::Bool(false))];
        assert!(
            original.upgrade().is_some(),
            "raw retains the borrowed entries"
        );
        *observed_original.borrow_mut() = Some(original);
        drop(input);

        let eng = weak_engine.upgrade().expect("binding engine");
        let inner = eng
            .bind(
                Value::JObj(Rc::new(vec![])),
                &record_type(false, &[]),
                &[Seg::Name("inner".into())],
                None,
                &Scope::new("inner", None),
            )
            .ok()
            .expect("valid record reentry during diagnostic delivery");
        assert!(matches!(inner, Value::Rec(_)));
        Some("native-record-tagger".into())
    }));
    let raw = Value::JObj(external.borrow().clone());
    let result = bind_at(&eng, raw, &record_type(true, &["missing", "lazy"]))
        .ok()
        .expect("record with a missing slot still binds its supplied values");
    *eng.env.tagger.borrow_mut() = None;
    let Value::Rec(inst) = result else {
        panic!("record result");
    };
    assert_eq!(tagger_calls.get(), 1);
    assert_eq!(calls.get(), 0, "record binding leaves native values lazy");
    assert_eq!(inst.borrow().entry_order, ["lazy", "extra.key"]);
    assert_eq!(
        inst.borrow().slot("missing").unwrap().state,
        SlotState::Invalid
    );
    assert_eq!(
        inst.borrow().slot("lazy").unwrap().state,
        SlotState::Unforced
    );
    assert_eq!(external.borrow()[0].0, "replacement");
    assert!(original.borrow().as_ref().unwrap().upgrade().is_none());
    drop(external);
    assert_eq!(
        weak_lazy.strong_count(),
        1,
        "the lazy descriptor owns its input"
    );
    assert_eq!(weak_extra.strong_count(), 1, "the extra owns its input");
    assert!(matches!(
        eng.force_slot(&inst, "lazy"),
        Ok(Value::Bool(true))
    ));
    let extra = inst.borrow().extra("extra.key").unwrap().clone();
    assert!(matches!(
        bind_at(&eng, extra, &ty(RTk::Any)),
        Ok(Value::Bool(true))
    ));
    assert_eq!(calls.get(), 2);
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E4002"));
    assert_eq!(diagnostics[0].path, "root.missing");
    assert_eq!(diagnostics[0].by.as_deref(), Some("native-record-tagger"));
    drop(inst);
    drop(eng);
    assert!(weak_lazy.upgrade().is_none());
    assert!(weak_extra.upgrade().is_none());
}

#[test]
fn immutable_record_preserves_duplicate_order_last_supply_and_each_unknown_diagnostic() {
    let eng = Engine::bare(Env::new());
    let input = Rc::new(vec![
        ("same".into(), Value::Bool(true)),
        ("a.b".into(), Value::Null),
        ("same".into(), Value::Bool(false)),
        ("한글".into(), Value::Null),
        ("a.b".into(), Value::Bool(true)),
    ]);
    let original = Rc::downgrade(&input);
    let result = bind_at(&eng, Value::JObj(input), &record_type(false, &["same"]))
        .ok()
        .expect("native duplicate record entries");
    let Value::Rec(inst) = result else {
        panic!("record result");
    };
    assert!(original.upgrade().is_none());
    assert_eq!(
        inst.borrow().entry_order,
        ["same", "a.b", "same", "한글", "a.b"]
    );
    assert!(inst.borrow().extras.is_empty());
    assert!(matches!(
        eng.force_slot(&inst, "same"),
        Ok(Value::Bool(false))
    ));
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 3);
    assert_eq!(
        diagnostics
            .iter()
            .map(|d| d.path.as_str())
            .collect::<Vec<_>>(),
        ["root[\"a.b\"]", "root[\"한글\"]", "root[\"a.b\"]"]
    );
    assert!(diagnostics
        .iter()
        .all(|d| d.code.as_deref() == Some("E4003")));
    assert_eq!(
        diagnostics[0].message,
        "undeclared member a.b on closed record"
    );
    assert_eq!(
        diagnostics[1].message,
        "undeclared member 한글 on closed record"
    );
    assert_eq!(diagnostics[2].message, diagnostics[0].message);
}

#[test]
fn mutable_record_input_is_snapshotted_before_tagger_mutates_its_map() {
    let eng = Engine::bare(Env::new());
    let input = Rc::new(RefCell::new(MapV {
        path: Rc::new(PrefixPath::default()),
        entries: [
            ("later".into(), Value::Bool(true)),
            ("extra.key".into(), Value::Bool(true)),
        ]
        .into_iter()
        .collect(),
    }));
    let weak_input = Rc::downgrade(&input);
    let calls = Rc::new(Cell::new(0));
    let observed_calls = calls.clone();
    *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
        observed_calls.set(observed_calls.get() + 1);
        let input = weak_input.upgrade().expect("mutable record input");
        let mut input = input.borrow_mut();
        input.entries.shift_remove("later");
        input.set("extra.key".into(), Value::Bool(false));
        input.set("late".into(), Value::Bool(false));
        None
    }));
    let result = bind_at(
        &eng,
        Value::Map(input.clone()),
        &record_type(true, &["missing", "later"]),
    )
    .ok()
    .expect("mutable map binds from an owned snapshot");
    *eng.env.tagger.borrow_mut() = None;
    let Value::Rec(inst) = result else {
        panic!("record result");
    };
    assert_eq!(calls.get(), 1);
    assert!(!input.borrow().has("later"));
    assert!(matches!(
        input.borrow().get("extra.key"),
        Some(Value::Bool(false))
    ));
    assert_eq!(inst.borrow().entry_order, ["later", "extra.key"]);
    assert!(matches!(
        inst.borrow().extra("extra.key"),
        Some(Value::Bool(true))
    ));
    assert!(inst.borrow().extra("late").is_none());
    drop(input);
    assert!(matches!(
        eng.force_slot(&inst, "later"),
        Ok(Value::Bool(true))
    ));
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E4002"));
    assert_eq!(diagnostics[0].path, "root.missing");
}

#[test]
fn immutable_record_edits_reuse_slots_and_keep_an_owned_input_snapshot() {
    let eng = Engine::bare(Env::new());
    let edits = Rc::new(Edits::default());
    *eng.edits.borrow_mut() = Some(edits.clone());
    let rt = record_type(false, &["changed", "stable"]);
    let before = vec![
        ("changed".into(), Value::Bool(true)),
        ("stable".into(), Value::Bool(true)),
    ];
    let input = Rc::new(before.clone());
    let original = Rc::downgrade(&input);
    let Value::Rec(inst) = bind_at(&eng, Value::JObj(input), &rt)
        .ok()
        .expect("initial record")
    else {
        panic!("record result");
    };
    assert!(
        original.upgrade().is_none(),
        "Edits owns entries independently of their outer Rc"
    );
    assert!(matches!(
        eng.force_slot(&inst, "changed"),
        Ok(Value::Bool(true))
    ));
    assert!(matches!(
        eng.force_slot(&inst, "stable"),
        Ok(Value::Bool(true))
    ));
    let changed_compute = inst
        .borrow()
        .slot("changed")
        .unwrap()
        .compute
        .clone()
        .unwrap();
    let stable_compute = inst
        .borrow()
        .slot("stable")
        .unwrap()
        .compute
        .clone()
        .unwrap();
    assert!(edits.unchanged(&inst, &before));

    edits.begin(&eng, &[], &HashMap::new());
    let Value::Rec(same) = bind_at(&eng, Value::JObj(Rc::new(before.clone())), &rt)
        .ok()
        .expect("unchanged record")
    else {
        panic!("record result");
    };
    assert!(Rc::ptr_eq(&same, &inst));
    assert_eq!(edits.retained_records.get(), 1);
    assert_eq!(eng.env.registry_len(), 1);
    assert_eq!(inst.borrow().slot("changed").unwrap().state, SlotState::Ok);
    assert!(Rc::ptr_eq(
        inst.borrow()
            .slot("changed")
            .unwrap()
            .compute
            .as_ref()
            .unwrap(),
        &changed_compute
    ));
    edits.finish(&eng);

    let next = vec![
        ("changed".into(), Value::Bool(false)),
        ("stable".into(), Value::Bool(true)),
    ];
    assert!(!edits.unchanged(&inst, &next));
    edits.begin(&eng, &[], &HashMap::new());
    let mut input = Rc::new(next.clone());
    let raw = Value::JObj(input.clone());
    let Value::Rec(rebound) = bind_at(&eng, raw, &rt).ok().expect("changed record") else {
        panic!("record result");
    };
    assert!(Rc::ptr_eq(&rebound, &inst));
    assert_eq!(
        inst.borrow().slot("changed").unwrap().state,
        SlotState::Unforced
    );
    assert!(!Rc::ptr_eq(
        inst.borrow()
            .slot("changed")
            .unwrap()
            .compute
            .as_ref()
            .unwrap(),
        &changed_compute
    ));
    assert_eq!(inst.borrow().slot("stable").unwrap().state, SlotState::Ok);
    assert!(Rc::ptr_eq(
        inst.borrow()
            .slot("stable")
            .unwrap()
            .compute
            .as_ref()
            .unwrap(),
        &stable_compute
    ));
    Rc::make_mut(&mut input)[0].1 = Value::Bool(true);
    assert!(
        edits.unchanged(&inst, &next),
        "stored entries survive caller mutation"
    );
    assert!(!edits.unchanged(&inst, input.as_slice()));
    drop(input);
    assert!(matches!(
        eng.force_slot(&inst, "changed"),
        Ok(Value::Bool(false))
    ));
    edits.finish(&eng);
    assert!(eng.env.diagnostics_vec().is_empty());
}

#[test]
fn native_duplicate_record_edits_keep_first_lookup_and_last_supply_distinct() {
    let eng = Engine::bare(Env::new());
    let edits = Rc::new(Edits::default());
    *eng.edits.borrow_mut() = Some(edits.clone());
    let rt = record_type(false, &["same"]);
    let before = vec![
        ("same".into(), Value::Bool(true)),
        ("same".into(), Value::Bool(false)),
    ];
    let Value::Rec(inst) = bind_at(&eng, Value::JObj(Rc::new(before.clone())), &rt)
        .ok()
        .expect("native duplicate record")
    else {
        panic!("record result");
    };
    assert!(matches!(
        eng.force_slot(&inst, "same"),
        Ok(Value::Bool(false))
    ));
    let original_compute = inst.borrow().slot("same").unwrap().compute.clone().unwrap();
    assert!(
        !edits.unchanged(&inst, &before),
        "reuse compares each native duplicate against the first stored occurrence"
    );
    edits.begin(&eng, &[], &HashMap::new());
    let Value::Rec(same) = bind_at(&eng, Value::JObj(Rc::new(before)), &rt)
        .ok()
        .expect("same duplicate entries rebound")
    else {
        panic!("record result");
    };
    assert!(Rc::ptr_eq(&same, &inst));
    assert_eq!(inst.borrow().slot("same").unwrap().state, SlotState::Ok);
    assert!(Rc::ptr_eq(
        inst.borrow()
            .slot("same")
            .unwrap()
            .compute
            .as_ref()
            .unwrap(),
        &original_compute
    ));
    edits.finish(&eng);

    edits.begin(&eng, &[], &HashMap::new());
    let next = vec![
        ("same".into(), Value::Bool(false)),
        ("same".into(), Value::Bool(false)),
    ];
    let Value::Rec(rebound) = bind_at(&eng, Value::JObj(Rc::new(next.clone())), &rt)
        .ok()
        .expect("changed first duplicate")
    else {
        panic!("record result");
    };
    assert!(Rc::ptr_eq(&rebound, &inst));
    assert_eq!(inst.borrow().entry_order, ["same", "same"]);
    assert_eq!(
        inst.borrow().slot("same").unwrap().state,
        SlotState::Unforced
    );
    assert!(!Rc::ptr_eq(
        inst.borrow()
            .slot("same")
            .unwrap()
            .compute
            .as_ref()
            .unwrap(),
        &original_compute
    ));
    assert!(matches!(
        eng.force_slot(&inst, "same"),
        Ok(Value::Bool(false))
    ));
    assert!(edits.unchanged(&inst, &next));
    edits.finish(&eng);
    assert!(eng.env.diagnostics_vec().is_empty());
}

#[test]
fn borrowed_record_values_remain_gc_roots_during_diagnostic_callbacks() {
    for mutable_input in [false, true] {
        let eng = Engine::bare(Env::new());
        let Value::Rec(child) = bind_at(
            &eng,
            Value::JObj(Rc::new(vec![("value".into(), Value::Bool(true))])),
            &record_type(true, &["value"]),
        )
        .ok()
        .expect("source child record") else {
            panic!("record result");
        };
        child
            .borrow_mut()
            .set_extra("self", Value::Rec(child.clone()));
        let weak_child = Rc::downgrade(&child);
        // The tracked child must be rooted by the binding input, not a prior
        // registry entry or an observer upgraded across the collection.
        eng.env.registry_clear();
        let entries = vec![("child".into(), Value::Rec(child))];
        let source = if mutable_input {
            Value::Map(Rc::new(RefCell::new(MapV {
                path: Rc::new(PrefixPath::default()),
                entries: entries.into_iter().collect(),
            })))
        } else {
            Value::JObj(Rc::new(entries))
        };
        let external = Rc::new(RefCell::new(source));
        let weak_external = Rc::downgrade(&external);
        let observed_child = weak_child.clone();
        let tagger_calls = Rc::new(Cell::new(0));
        let calls = tagger_calls.clone();
        *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
            calls.set(calls.get() + 1);
            let external = weak_external.upgrade().expect("external input handle");
            let mut source = external.borrow_mut();
            if let Value::Map(map) = &*source {
                map.borrow_mut().entries.clear();
            }
            *source = Value::Null;
            drop(source);

            // JObj is still owned by raw; a cleared Map leaves its eagerly
            // captured Cow entries as the only external owner of the child.
            collect_cycles();
            let child = observed_child
                .upgrade()
                .expect("input-rooted child survives GC");
            assert_eq!(
                child.borrow().slot("value").unwrap().state,
                SlotState::Unforced
            );
            assert!(matches!(child.borrow().extra("self"), Some(Value::Rec(_))));
            None
        }));
        let raw = external.borrow().clone();
        let Value::Rec(inst) = bind_at(&eng, raw, &record_type(false, &["missing", "child"]))
            .ok()
            .expect("record binding across explicit native collection")
        else {
            panic!("record result");
        };
        *eng.env.tagger.borrow_mut() = None;
        assert_eq!(tagger_calls.get(), 1);
        assert!(matches!(*external.borrow(), Value::Null));
        drop(external);
        let Value::Rec(child) = eng
            .force_slot(&inst, "child")
            .ok()
            .expect("retained child slot")
        else {
            panic!("record child");
        };
        assert!(Rc::ptr_eq(&child, &weak_child.upgrade().unwrap()));
        assert!(matches!(
            eng.force_slot(&child, "value"),
            Ok(Value::Bool(true))
        ));
        let diagnostics = eng.env.diagnostics_vec();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_deref(), Some("E4002"));
        assert_eq!(diagnostics[0].path, "root.missing");
        drop(child);
        drop(inst);
        drop(eng);
        collect_cycles();
        assert!(
            weak_child.upgrade().is_none(),
            "the input-rooted cycle is eventually released"
        );
    }
}

#[test]
fn duplicate_record_values_gain_ownership_only_when_the_selected_slot_is_created() {
    let eng = Engine::bare(Env::new());
    let forced = Rc::new(RefCell::new(Vec::new()));
    let first_forced = forced.clone();
    let first = callback(move || {
        first_forced.borrow_mut().push("first");
        Ok(Value::Bool(true))
    });
    let selected_forced = forced.clone();
    let selected = callback(move || {
        selected_forced.borrow_mut().push("selected");
        Ok(Value::Bool(false))
    });
    let Value::PreVal(first_value) = &first else {
        panic!("first native value");
    };
    let weak_first = Rc::downgrade(first_value);
    let Value::PreVal(selected_value) = &selected else {
        panic!("selected native value");
    };
    let weak_selected = Rc::downgrade(selected_value);
    let observed_first = weak_first.clone();
    let observed_selected = weak_selected.clone();
    let owners = Rc::new(RefCell::new(Vec::new()));
    let observations = owners.clone();
    *eng.env.tagger.borrow_mut() = Some(Rc::new(move || {
        observations.borrow_mut().push((
            observed_first.strong_count(),
            observed_selected.strong_count(),
        ));
        None
    }));
    let input = Value::JObj(Rc::new(vec![
        ("same".into(), first),
        ("same".into(), selected),
    ]));
    let Value::Rec(inst) = bind_at(
        &eng,
        input,
        &record_type(false, &["before", "same", "after"]),
    )
    .ok()
    .expect("duplicate native supplied values") else {
        panic!("record result");
    };
    *eng.env.tagger.borrow_mut() = None;
    assert_eq!(
        *owners.borrow(),
        [(1, 1), (1, 2)],
        "raw owns both entries; only the selected lazy descriptor gains an owner between diagnostics"
    );
    assert!(forced.borrow().is_empty());
    assert!(
        weak_first.upgrade().is_none(),
        "unselected native input does not escape"
    );
    assert_eq!(
        weak_selected.strong_count(),
        1,
        "selected descriptor owns its value after raw release"
    );
    assert_eq!(inst.borrow().entry_order, ["same", "same"]);
    assert!(matches!(
        eng.force_slot(&inst, "same"),
        Ok(Value::Bool(false))
    ));
    assert_eq!(*forced.borrow(), ["selected"]);
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 2);
    assert!(diagnostics
        .iter()
        .all(|d| d.code.as_deref() == Some("E4002")));
    assert_eq!(diagnostics[0].path, "root.before");
    assert_eq!(diagnostics[1].path, "root.after");
    drop(inst);
    drop(eng);
    assert!(weak_selected.upgrade().is_none());
}
