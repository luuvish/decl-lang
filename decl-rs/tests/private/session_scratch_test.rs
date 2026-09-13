//! Rust-only graph ownership and scratch work-bound checks.
use super::*;
use crate::engine::Inst;
use crate::qengine::graph::ReadSet;
use crate::semantics::{record_instance, ty, Compute, RecInst, Slot};
use std::collections::BTreeSet;

fn instance(path: &str) -> Inst {
    record_instance(RecInst {
        type_name: None,
        rt: ty(RTk::Any),
        path: Rc::new(vec![Seg::Name(path.into())]),
        ps: RefCell::new(None),
        parent: None,
        slots: Vec::new(),
        entry_order: Vec::new(),
        extras: Vec::new(),
        menv: None,
    })
}

fn empty_run() -> (Session, Run) {
    let session = Session::new(None);
    let run = session.run(Mode::Full);
    assert!(run.load_diags.is_empty());
    assert!(run.checks.is_empty());
    assert!(run.diags.is_empty());
    assert!(run.eng.is_some());
    (session, run)
}

fn read(eng: &Engine, owner: &str, dependency: &str) {
    eng.replace_query_reads(owner, [eng.query_id(dependency)].into_iter().collect());
}

fn registry_ids(env: &Env) -> Vec<usize> {
    env.registry_snapshot()
        .iter()
        .map(|inst| Rc::as_ptr(inst) as usize)
        .collect()
}

#[test]
fn temporary_slot_drop_observes_all_read_owners_already_removed() {
    struct ObserveReadsOnDrop {
        engine: std::rc::Weak<Engine>,
        observed: Rc<Cell<Option<usize>>>,
    }
    impl Drop for ObserveReadsOnDrop {
        fn drop(&mut self) {
            if let Some(engine) = self.engine.upgrade() {
                // This public API must be usable here: the old cleanup released
                // its read-map borrow before it started dropping slot values.
                self.observed.set(Some(engine.dependency_queries()));
            }
        }
    }

    for demand_fallback in [false, true] {
        let (session, run) = empty_run();
        let eng = run.eng.as_ref().unwrap();
        let observed = Rc::new(Cell::new(None));
        session.scratch(&run, |current, env| {
            let observer = ObserveReadsOnDrop {
                engine: Rc::downgrade(current),
                observed: observed.clone(),
            };
            let temporary = instance("_");
            temporary.borrow_mut().slots.push((
                "callback".into(),
                Slot {
                    kind: MKind::Der,
                    hidden: false,
                    state: SlotState::Unforced,
                    value: Value::Undef,
                    compute: Some(Rc::new(Compute::Bridge(Rc::new(move || {
                        let _ = &observer;
                        Ok(Value::Null)
                    })))),
                },
            ));
            // The query index becomes the instance's last strong owner. Its
            // removal drops the Bridge capture during graph cleanup itself.
            current.register_query_slot("_.callback", temporary, "callback".into());
            read(current, "_.callback", "ordinary.dependency");
            read(current, "assert:_.other", "ordinary.dependency");
            if demand_fallback {
                env.set_root("future", Value::Null);
                read(current, "root:future", "ordinary.dependency");
            }
        });
        assert_eq!(observed.get(), Some(0));
        assert_eq!(eng.dependency_queries(), 0);
        assert!(eng.query_slot("_.callback").is_none());
    }
}

#[test]
fn underscore_snapshots_filter_current_membership_and_preserve_old_owners() {
    let (session, run) = empty_run();
    let eng = run.eng.as_ref().unwrap();
    let existing = instance("_");
    eng.env.set_root("_", Value::Rec(existing.clone()));
    eng.env.registry_push(existing.clone());
    eng.register_query_slot("_.kept", existing.clone(), "before".into());
    read(eng, "_.kept", "stable.before");
    read(eng, "assert:_", "stable.assertion");
    // A shared pool can contain another retained Engine's scratch identities.
    let old = Engine::bare_with_queries(Env::new(), eng.query_pool());
    let old_inst = instance("_");
    old.register_query_slot("_.old_run", old_inst.clone(), "old".into());
    read(&old, "assert:_.old_run", "old.dependency");
    let old_dependencies = old.query_dependencies();
    let before_registry = registry_ids(&eng.env);
    let replacement = instance("_");
    session.scratch(&run, |current, env| {
        current.register_query_slot("_.kept", replacement.clone(), "after".into());
        read(current, "_.kept", "transient.after");
        read(current, "assert:_", "transient.assertion");
        current.register_query_slot("_[0].new", replacement.clone(), "new".into());
        read(current, "assert:_[0]", "transient.new");
        // Raw slot and assertion-owner predicates are intentionally different.
        current.register_query_slot("assert:_.slot", replacement.clone(), "raw".into());
        read(current, "assert:_.slot", "transient.read");
        for owner in ["_other.x", "root:_", "assert:assert:_"] {
            read(current, owner, "stable.outside");
        }
        env.registry_push(replacement.clone());
    });
    let (slot_inst, member) = eng.query_slot("_.kept").unwrap();
    assert!(Rc::ptr_eq(&slot_inst, &existing));
    assert_eq!(member, "before");
    assert_eq!(
        eng.query_dependencies_for("_.kept"),
        Some(vec!["stable.before".into()])
    );
    assert_eq!(
        eng.query_dependencies_for("assert:_"),
        Some(vec!["stable.assertion".into()])
    );
    assert!(eng.query_slot("_[0].new").is_none());
    assert!(eng.query_dependencies_for("assert:_[0]").is_none());
    assert!(eng.query_slot("assert:_.slot").is_some());
    assert!(eng.query_dependencies_for("assert:_.slot").is_none());
    for owner in ["_other.x", "root:_", "assert:assert:_"] {
        assert_eq!(
            eng.query_dependencies_for(owner),
            Some(vec!["stable.outside".into()])
        );
    }
    assert!(eng.query_slot("_.old_run").is_none());
    assert!(eng.query_dependencies_for("assert:_.old_run").is_none());
    assert_eq!(old.query_dependencies(), old_dependencies);
    assert!(Rc::ptr_eq(
        &old.query_slot("_.old_run").unwrap().0,
        &old_inst
    ));
    assert_eq!(registry_ids(&eng.env), before_registry);
    assert!(matches!(eng.env.root("_"), Some(Value::Rec(root)) if Rc::ptr_eq(&root, &existing)));
}

#[test]
fn demanded_root_error_cleanup_preserves_neighbors_and_existing_underscore() {
    let (session, run) = empty_run();
    let eng = run.eng.as_ref().unwrap();
    let old = instance("before_public_mutation");
    let underscore = instance("_");
    let neighbor = instance("future_neighbor");
    for inst in [&old, &underscore, &neighbor] {
        eng.env.registry_push(inst.clone());
    }
    eng.env.set_root("_", Value::Rec(underscore.clone()));
    eng.register_query_slot("_.kept", underscore.clone(), "before".into());
    read(eng, "assert:_", "stable.before");
    let outcome: Result<(), &str> = session.scratch(&run, |current, env| {
        old.borrow_mut().path = Rc::new(vec![Seg::Name("future".into())]);
        env.set_root("future", Value::Rec(old.clone()));
        current.failed_inputs.borrow_mut().insert("future".into());
        for key in ["future.x", "future[0]", "_.new", "future_neighbor.x"] {
            current.register_query_slot(key, old.clone(), "x".into());
            read(current, key, "dependency");
        }
        read(current, "root:future", "dependency");
        read(current, "assert:future", "dependency");
        read(current, "assert:future_neighbor", "dependency");
        env.registry_push(instance("_"));
        env.registry_push(instance("future"));
        env.report(Diag::error("scratch failure", "_".into(), None));
        current
            .computing
            .borrow_mut()
            .push(current.query_id("_.failed"));
        Err("ordinary failure")
    });
    assert_eq!(outcome, Err("ordinary failure"));
    assert!(eng.env.root("future").is_none());
    assert!(!eng.failed_inputs.borrow().contains("future"));
    for key in ["future.x", "future[0]", "_.new"] {
        assert!(eng.query_slot(key).is_none());
        assert!(eng.query_dependencies_for(key).is_none());
    }
    for key in ["root:future", "assert:future"] {
        assert!(eng.query_dependencies_for(key).is_none());
    }
    assert!(eng.query_slot("future_neighbor.x").is_some());
    assert!(eng
        .query_dependencies_for("assert:future_neighbor")
        .is_some());
    assert!(Rc::ptr_eq(
        &eng.query_slot("_.kept").unwrap().0,
        &underscore
    ));
    assert_eq!(
        eng.query_dependencies_for("assert:_"),
        Some(vec!["stable.before".into()])
    );
    assert_eq!(
        registry_ids(&eng.env),
        vec![
            Rc::as_ptr(&underscore) as usize,
            Rc::as_ptr(&neighbor) as usize
        ]
    );
    assert_eq!(eng.env.diag_len(), 0);
    assert!(eng.computing.borrow().is_empty());
}

#[test]
fn scalar_scratch_does_not_borrow_preexisting_registry_instances() {
    let (session, run) = empty_run();
    let eng = run.eng.as_ref().unwrap();
    let existing = instance("_");
    for _ in 0..4096 {
        eng.env.registry_push(existing.clone());
    }
    // A borrow of an old instance remains valid across the no-demand cleanup:
    // the entire prefix is skipped, rather than visited and path-formatted.
    let borrowed = existing.borrow_mut();
    session.scratch(&run, |_, env| {
        env.registry_push(instance("_"));
    });
    drop(borrowed);
    assert_eq!(eng.env.registry_len(), 4096);
}

#[test]
fn registry_tail_retention_preserves_order_and_public_replacement_boundary() {
    let env = Env::new();
    let existing = instance("_");
    for _ in 0..4096 {
        env.registry_push(existing.clone());
    }
    let kept = [instance("first"), instance("second"), instance("third")];
    for inst in &kept {
        env.registry_push(instance("_"));
        env.registry_push(inst.clone());
    }
    let mut visits = 0;
    env.registry_retain_from(4096, |inst| {
        visits += 1;
        !matches!(inst.borrow().path.first(), Some(Seg::Name(n)) if &**n == "_")
    });
    assert_eq!(visits, 6);
    assert_eq!(env.registry_len(), 4099);
    assert_eq!(
        &registry_ids(&env)[4096..],
        kept.iter()
            .map(|inst| Rc::as_ptr(inst) as usize)
            .collect::<Vec<_>>()
    );
    let (session, run) = empty_run();
    let eng = run.eng.as_ref().unwrap();
    eng.env.registry_push(existing.clone());
    let replaced = instance("_");
    session.scratch(&run, |_, env| {
        *env.registry.borrow_mut() = Rc::new(RefCell::new(vec![replaced.clone()]));
    });
    assert_eq!(registry_ids(&eng.env), vec![Rc::as_ptr(&replaced) as usize]);
}

#[test]
fn ordinary_graph_size_does_not_expand_the_scratch_subset() {
    let (session, run) = empty_run();
    let eng = run.eng.as_ref().unwrap();
    let stable = instance("stable");
    for index in 0..4096 {
        let key = format!("stable.rows[{index}].value");
        eng.register_query_slot(&key, stable.clone(), "value".into());
        eng.replace_query_reads(&key, ReadSet::default());
    }
    assert!(eng.query_pool().scratch_ids().is_empty());
    let before_slots = eng.indexed_slots();
    let before_reads = eng.dependency_queries();
    session.scratch(&run, |_, _| ());
    assert_eq!(eng.indexed_slots(), before_slots);
    assert_eq!(eng.dependency_queries(), before_reads);
    assert!(eng.query_pool().scratch_ids().is_empty());
    assert_eq!(
        eng.query_slot_keys()
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        before_slots
    );
}
