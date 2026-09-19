//! Rust-only capture guards and identity handoff through actual slot forcing.
use super::*;
use crate::parse::parse_source;
use crate::pipeline::run_pipeline;

#[test]
fn recorded_identity_respects_tracking_and_verification_depth() {
    let eng = Engine::bare(Env::new());
    eng.step("untracked", || {
        let entries = eng.query_pool().census().entries;
        assert!(eng.record_query("untracked.read").is_none());
        assert_eq!(eng.query_pool().census().entries, entries);
    });
    eng.track.set(true);
    let entries = eng.query_pool().census().entries;
    assert!(eng.record_query("without.parent").is_none());
    assert_eq!(eng.query_pool().census().entries, entries);
    eng.step("parent", || {
        eng.record("kept".into());
        let id = eng.record_query("new.read").unwrap();
        assert_eq!(id.as_str(), "new.read");
        assert_eq!(eng.record_query("new.read"), Some(id));
        eng.verify_reads(|| {
            let entries = eng.query_pool().census().entries;
            assert!(eng.record_query("verification.only").is_none());
            assert_eq!(eng.query_pool().census().entries, entries);
            eng.step("nested", || {
                assert_eq!(
                    eng.record_query("nested.read").unwrap().as_str(),
                    "nested.read"
                );
            });
        });
        assert_eq!(eng.query_stack(), ["parent"]);
    });
    assert_eq!(
        eng.query_dependencies_for("parent"),
        Some(vec!["kept".into(), "new.read".into()])
    );
    assert_eq!(
        eng.query_dependencies_for("nested"),
        Some(vec!["nested.read".into()])
    );
    assert!(eng.query_stack().is_empty());
}

#[test]
fn forcing_guards_preserve_parent_reads_and_producer_registration() {
    for state in [
        SlotState::Ok,
        SlotState::Absent,
        SlotState::Invalid,
        SlotState::Deferred,
        SlotState::Forcing,
        SlotState::Unforced,
    ] {
        let pipeline = run_pipeline(
            &parse_source("type T = { n: int = 1, twice: int = n + n }\nexport output t: T = {}\n")
                .decls,
        );
        let eng = pipeline.eng;
        let Value::Rec(inst) = eng.env.root("t").unwrap() else {
            panic!("record output");
        };
        *eng.round_cache.borrow_mut() = None;
        eng.track.set(true);
        eng.set_phase(1);
        eng.remove_query_slot("t.twice");
        inst.borrow_mut().slot_mut("twice").unwrap().state = state;
        let result = eng.step("caller", || {
            eng.record("unrelated".into());
            eng.force_slot(&inst, "twice")
        });
        assert_eq!(
            eng.query_dependencies_for("caller"),
            Some(vec!["t.twice".into(), "unrelated".into()])
        );
        assert!(eng.query_stack().is_empty());
        assert_eq!(
            eng.query_slot("t.twice").is_some(),
            state == SlotState::Unforced
        );
        match state {
            SlotState::Ok | SlotState::Unforced => {
                let value = result.ok().expect("successful force");
                assert_eq!(eng.serialize(&value, "t", false), "2");
            }
            SlotState::Absent => assert!(matches!(result, Ok(Value::Absent))),
            SlotState::Deferred => assert!(matches!(result, Err(Fail::Defer))),
            SlotState::Invalid | SlotState::Forcing => {
                assert!(matches!(result, Err(Fail::Taint)));
            }
        }
        if state == SlotState::Unforced {
            assert_eq!(
                eng.query_dependencies_for("t.twice"),
                Some(vec!["t.n".into()])
            );
        }
        if state == SlotState::Forcing {
            assert!(eng
                .env
                .diagnostics_vec()
                .iter()
                .any(|diag| diag.code.as_deref() == Some("E5007")));
        }
    }
}

fn diagnostic_record(name: &str, state: SlotState) -> Inst {
    record_instance(RecInst {
        type_name: None,
        rt: ty(RTk::Any),
        path: Rc::new(vec![
            Seg::Name("root".into()),
            Seg::Name("items".into()),
            Seg::Key("a.b".into()),
            Seg::Idx(2),
        ]),
        ps: RefCell::new(None),
        parent: None,
        slots: vec![(
            name.into(),
            Slot {
                kind: MKind::Der,
                hidden: false,
                state,
                value: Value::Undef,
                compute: None,
            },
        )],
        entry_order: vec![name.into()].into(),
        extras: vec![],
        menv: None,
    })
}

#[test]
fn cyclic_force_reports_canonical_member_path() {
    let eng = Engine::bare(Env::new());
    let inst = diagnostic_record("odd.member", SlotState::Forcing);
    let result = eng.force_slot(&inst, "odd.member");
    assert!(matches!(result, Err(Fail::Taint)));
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E5007"));
    assert_eq!(
        diagnostics[0].path,
        "root.items[\"a.b\"][2][\"odd.member\"]"
    );
    assert_eq!(
        inst.borrow().slot("odd.member").unwrap().state,
        SlotState::Invalid
    );
}

#[test]
fn force_path_capture_preserves_diagnostics_and_releases_replaced_paths() {
    // Public Bridge callbacks can replace or mutate a record's shared path.
    // Diagnostics retain the path at force entry, while no path snapshot may
    // survive a successful, deferred or failed force.
    for copy_on_write in [false, true] {
        for outcome in [SlotState::Ok, SlotState::Deferred, SlotState::Invalid] {
            let eng = Engine::bare(Env::new());
            eng.set_phase(1);
            let inst = diagnostic_record("odd.member", SlotState::Unforced);
            let old_path = Rc::downgrade(&inst.borrow().path);
            let old_root = {
                let b = inst.borrow();
                let Seg::Name(root) = &b.path[0] else {
                    panic!("root name");
                };
                Rc::downgrade(root)
            };
            let weak_inst = Rc::downgrade(&inst);
            inst.borrow_mut().slot_mut("odd.member").unwrap().compute =
                Some(Rc::new(Compute::Bridge(Rc::new(move || {
                    let inst = weak_inst.upgrade().expect("forcing instance is live");
                    let mut b = inst.borrow_mut();
                    if copy_on_write {
                        Rc::make_mut(&mut b.path)[0] = Seg::Name("changed".into());
                    } else {
                        b.path = Rc::new(vec![Seg::Name("changed".into())]);
                    }
                    drop(b);
                    match outcome {
                        SlotState::Ok => Ok(Value::Null),
                        SlotState::Deferred => Err(Fail::Defer),
                        SlotState::Invalid => err_code("bridge failure", "E5006"),
                        _ => unreachable!(),
                    }
                }))));

            let result = eng.force_slot(&inst, "odd.member");
            match outcome {
                SlotState::Ok => assert!(matches!(result, Ok(Value::Null))),
                SlotState::Deferred => assert!(matches!(result, Err(Fail::Defer))),
                SlotState::Invalid => assert!(matches!(result, Err(Fail::Taint))),
                _ => unreachable!(),
            }
            assert_eq!(inst.borrow().slot("odd.member").unwrap().state, outcome);
            assert!(old_path.upgrade().is_none(), "force released the old path");
            assert!(old_root.upgrade().is_none(), "force released old segments");
            assert_eq!(Rc::strong_count(&inst.borrow().path), 1);
            assert!(eng.query_stack().is_empty());
            let diagnostics = eng.env.diagnostics_vec();
            if outcome == SlotState::Invalid {
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0].code.as_deref(), Some("E5006"));
                assert_eq!(diagnostics[0].message, "bridge failure");
                assert_eq!(
                    diagnostics[0].path,
                    "root.items[\"a.b\"][2][\"odd.member\"]"
                );
            } else {
                assert!(diagnostics.is_empty());
            }
            assert_eq!(
                eng.deferred_slots.borrow().len(),
                usize::from(outcome == SlotState::Deferred)
            );
        }
    }
}

#[test]
fn force_path_capture_precedes_revision_dependency_callbacks() {
    let eng = Engine::bare(Env::new());
    let inst = diagnostic_record("odd.member", SlotState::Ok);
    let calls = Rc::new(Cell::new(0));
    let producer_calls = calls.clone();
    {
        let mut b = inst.borrow_mut();
        let slot = b.slot_mut("odd.member").unwrap();
        slot.value = Value::Null;
        slot.compute = Some(Rc::new(Compute::Bridge(Rc::new(move || {
            producer_calls.set(producer_calls.get() + 1);
            Ok(Value::Null)
        }))));
    }
    let key = Engine::slot_key(&inst, "odd.member");
    eng.register_query_slot(&key, inst.clone(), "odd.member".into());
    eng.replace_query_reads(&key, [eng.query_id("before-produce")].into_iter().collect());
    let revisions = Rc::new(Revisions::default());
    revisions.begin_queries(
        &eng,
        &[key.clone(), "before-produce".into()].into_iter().collect(),
        &[(key, Value::Null), ("before-produce".into(), Value::Null)]
            .into_iter()
            .collect(),
        &FxHashSet::default(),
        |_| Rc::new(|_, _| true),
    );
    revisions.resolve.set(Some(|eng, dependency| {
        assert_eq!(dependency, "before-produce");
        let (inst, _) = eng
            .query_slot("root.items[\"a.b\"][2].odd.member")
            .expect("pending producer");
        Rc::make_mut(&mut inst.borrow_mut().path)[0] = Seg::Name("changed".into());
        err_code("dependency verification failed", "E5006")
    }));
    *eng.revisions.borrow_mut() = Some(revisions);
    inst.borrow_mut().slot_mut("odd.member").unwrap().state = SlotState::Unforced;
    let old_path = Rc::downgrade(&inst.borrow().path);

    assert!(matches!(
        eng.force_slot(&inst, "odd.member"),
        Err(Fail::Taint)
    ));
    assert_eq!(
        calls.get(),
        0,
        "verification failed before the producer ran"
    );
    let diagnostics = eng.env.diagnostics_vec();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E5006"));
    assert_eq!(diagnostics[0].message, "dependency verification failed");
    assert_eq!(
        diagnostics[0].path,
        "root.items[\"a.b\"][2][\"odd.member\"]"
    );
    assert!(
        old_path.upgrade().is_none(),
        "verification capture released"
    );
    assert_eq!(Rc::strong_count(&inst.borrow().path), 1);
    assert!(eng.query_stack().is_empty());
}
