//! Native attribution must preserve conservative preparation and owner lifetimes.
use super::*;

fn instance() -> Inst {
    record_instance(RecInst {
        type_name: None,
        rt: ty(RTk::Any),
        path: Rc::new(vec![Seg::Name("record".into())]),
        ps: RefCell::new(None),
        parent: None,
        slots: vec![(
            "x".into(),
            Slot {
                kind: MKind::Der,
                hidden: false,
                state: SlotState::Ok,
                value: Value::Int(Num::Small(7)),
                compute: Some(Rc::new(Compute::Bridge(Rc::new(|| {
                    Ok(Value::Int(Num::Small(7)))
                })))),
            },
        )],
        entry_order: Vec::new().into(),
        extras: Vec::new(),
        menv: None,
    })
}

fn phases_are_disjoint(phases: &[EditPhase]) {
    assert!(phases.iter().all(|p| p.recorded && p.completed));
    for pair in phases.windows(2) {
        assert_eq!(pair[0].end.wall_ns, pair[1].start.wall_ns);
        assert_eq!(pair[0].end.cpu_ns, pair[1].start.cpu_ns);
        assert_eq!(pair[0].end.valid, pair[1].start.valid);
    }
    for phase in phases {
        if phase.valid {
            assert_eq!(phase.wall_ns, phase.end.wall_ns - phase.start.wall_ns);
            assert_eq!(phase.cpu_ns, phase.end.cpu_ns - phase.start.cpu_ns);
        } else {
            assert_eq!((phase.wall_ns, phase.cpu_ns), (0, 0));
        }
    }
    if phases.iter().all(|p| p.valid) {
        assert_eq!(
            phases.iter().map(|p| p.wall_ns).sum::<u64>(),
            phases.last().unwrap().end.wall_ns - phases[0].start.wall_ns
        );
        assert_eq!(
            phases.iter().map(|p| p.cpu_ns).sum::<u64>(),
            phases.last().unwrap().end.cpu_ns - phases[0].start.cpu_ns
        );
    }
}

#[test]
fn preparation_attribution_keeps_unknown_owner_fallback_and_pending_lifetimes() {
    let eng = Engine::bare(Env::new());
    let edits = Edits::default();
    let record = instance();
    let weak = Rc::downgrade(&record);
    eng.env.registry_push(record.clone());
    eng.env.set_root("record", Value::Rec(record.clone()));
    for (key, deps) in [
        (
            "q1",
            vec!["edge:a", "snapshot:b", "round:c", "value:missing"],
        ),
        ("q2", vec!["record.x"]),
        ("q3", vec!["q1"]),
    ] {
        eng.replace_query_reads(key, deps.into_iter().map(|s| eng.query_id(s)).collect());
    }

    edits.begin(&eng, &[], &HashMap::new());
    let first = edits.diagnostics();
    assert!(first.preparation_completed);
    assert!(!first.finish_completed);
    assert_eq!((first.sequence, first.revision), (1, 1));
    phases_are_disjoint(&first.preparation);
    let work = first.preparation_work;
    assert_eq!(
        (work.registry_records, work.pooled_records, work.saved_roots),
        (1, 1, 1)
    );
    assert_eq!(work.forced_after_seeding, [0, 1, 1, 1, 1]);
    assert_eq!(
        (work.read_queries, work.read_edges, work.reverse_keys),
        (3, 6, 6)
    );
    assert_eq!(work.special_edges, [1, 1, 1, 1]);
    assert_eq!(work.reverse_degree_buckets, [6, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(work.reverse_max_degree, 1);
    assert_eq!(
        (
            work.registered_slots_scanned,
            work.saved_slot_writes,
            work.saved_values
        ),
        (1, 1, 2)
    );
    assert_eq!(
        (
            work.value_dependency_keys,
            work.value_owners_examined,
            work.value_owners_found
        ),
        (1, 1, 0)
    );
    assert!(work.missing_owner_fallback);
    assert_eq!(work.fallback_enqueues, 2);
    assert_eq!(work.invalid_queries, 5);
    assert_eq!(edits.prepared_queries.get(), 5);
    for key in ["root:record", "record.x", "q1", "q2", "q3"] {
        assert!(edits.invalid.borrow().contains(key));
    }
    // Hash iteration can change queue order, but this conservation law does not.
    assert_eq!(work.queue_pops, work.invalid_queries + work.duplicate_pops);
    assert_eq!(
        work.queue_pops,
        work.initial_queue
            + work.fallback_enqueues
            + work.owner_descendant_enqueues
            + work.producer_descendant_enqueues
            + work.reader_enqueues
    );
    assert_eq!(
        (
            work.pending_captures,
            work.capture_memo_entries,
            work.reset_slots
        ),
        (2, 2, 1)
    );
    assert_eq!(work.pending_capture_kinds, [1, 0, 0, 1]);
    assert_eq!(
        record.borrow().slot("x").unwrap().state,
        SlotState::Unforced
    );

    // Reading or serializing the diagnostic snapshot adds no record owner.
    let owners = Rc::strong_count(&record);
    let snapshot = edits.diagnostics();
    let json = serde_json::to_value(snapshot).unwrap();
    assert_eq!(json["preparation_work"]["invalid_queries"], 5);
    assert_eq!(Rc::strong_count(&record), owners);
    drop(record);
    edits.finish(&eng);
    let finished = edits.diagnostics();
    assert!(finished.finish_completed && finished.finish_was_active);
    assert_eq!((finished.sequence, finished.finish_sequence), (1, 1));
    phases_are_disjoint(&finished.finish);
    assert_eq!(finished.finish_work.slot_entries, [1, 0]);
    assert!(
        weak.upgrade().is_none(),
        "neither stored diagnostic copy retains a record"
    );
    assert!(
        !first.finish_completed,
        "old fixed snapshots do not change on finish"
    );
    assert!(!snapshot.finish_completed);
    let aggregate = finished.finish_aggregate;
    assert_eq!(
        (
            aggregate.calls,
            aggregate.active_calls,
            aggregate.partial_calls,
            aggregate.superseded_calls
        ),
        (1, 1, 0, 0)
    );
    for (total, phase) in aggregate.phases.iter().zip(&finished.finish) {
        assert_eq!(total.recorded_calls, 1);
        assert_eq!((total.wall_ns, total.cpu_ns), (phase.wall_ns, phase.cpu_ns));
    }
    // Later reference rounds can finish while inactive. Their latest snapshot
    // must not erase the first call's active cleanup attribution.
    edits.finish(&eng);
    let later = edits.diagnostics();
    assert!(!later.finish_was_active);
    assert_eq!(
        (
            later.finish_aggregate.calls,
            later.finish_aggregate.active_calls
        ),
        (2, 1)
    );
    for i in 0..6 {
        let old = aggregate.phases[i];
        let latest = later.finish[i];
        let sum = later.finish_aggregate.phases[i];
        assert_eq!(
            sum.recorded_calls,
            old.recorded_calls + u64::from(latest.recorded)
        );
        assert_eq!(sum.wall_ns, old.wall_ns + latest.wall_ns);
        assert_eq!(sum.cpu_ns, old.cpu_ns + latest.cpu_ns);
    }
    assert_eq!(aggregate.calls, 1, "earlier copies stay immutable");
    edits.begin(&eng, &[], &HashMap::new());
    let next = edits.diagnostics();
    assert_eq!(next.sequence, 2);
    assert_eq!(next.finish_aggregate.calls, 0);
    assert!(next.finish_aggregate.valid);
    assert!(next
        .finish_aggregate
        .phases
        .iter()
        .all(|p| p.recorded_calls == 0 && p.wall_ns == 0 && p.cpu_ns == 0));
    edits.finish(&eng);
}

#[test]
fn producer_queue_keeps_string_seed_lifo_and_cross_pool_diamond_readers() {
    use crate::qengine::graph::QueryPool;

    let eng = Engine::bare(Env::new());
    let edits = Edits::default();
    let left = instance();
    let right = instance();
    let computations = Rc::new(Cell::new(0));
    for (record, result) in [(&left, 11), (&right, 22)] {
        let calls = computations.clone();
        let mut record = record.borrow_mut();
        let slot = record.slot_mut("x").unwrap();
        slot.value = Value::Int(Num::Small(result));
        slot.compute = Some(Rc::new(Compute::Bridge(Rc::new(move || {
            calls.set(calls.get() + 1);
            Ok(Value::Int(Num::Small(result)))
        }))));
    }
    // Native roots may expose different records at the same canonical path.
    // Leave them outside the registry: collecting each producer registers its
    // child slot, and the last producer visited determines the indexed owner.
    eng.env.set_root("left", Value::Rec(left.clone()));
    eng.env.set_root("right", Value::Rec(right.clone()));
    assert_eq!(eng.env.registry_len(), 0);
    for key in ["root:left", "root:right"] {
        eng.replace_query_reads(key, [eng.query_id("edge:changed")].into_iter().collect());
    }
    let foreign = QueryPool::default();
    eng.replace_query_reads_id(
        &foreign.intern("diamond"),
        [foreign.intern("root:left"), foreign.intern("root:right")]
            .into_iter()
            .collect(),
    );
    eng.replace_query_reads("tail", [eng.query_id("diamond")].into_iter().collect());

    // Preserve the original String-set seed contract without assuming a fixed
    // hash iteration order. Its first entry is the last seed popped by LIFO.
    let mut string_seeds = FxHashSet::default();
    for key in eng.reads.borrow().keys() {
        if matches!(key.as_str(), "root:left" | "root:right") {
            string_seeds.insert(key.as_str().to_owned());
        }
    }
    let last_producer = string_seeds.iter().next().unwrap();
    let (winner, untouched, expected) = if last_producer == "root:left" {
        (&left, &right, 11)
    } else {
        (&right, &left, 22)
    };

    edits.begin(&eng, &[], &HashMap::new());
    let work = edits.diagnostics().preparation_work;
    assert_eq!(work.forced_after_seeding, [0, 2, 2, 2, 2]);
    assert_eq!(work.initial_queue, 2);
    assert_eq!(work.producer_collects, 2);
    assert_eq!(work.producer_descendant_enqueues, 1);
    assert_eq!(work.reader_enqueues, 3);
    assert_eq!((work.queue_pops, work.duplicate_pops), (6, 1));
    assert_eq!(work.invalid_queries, 5);
    assert_eq!(work.reset_slots, 1);
    for key in ["root:left", "root:right", "record.x", "diamond", "tail"] {
        assert!(edits.invalid.borrow().contains(key));
    }
    let (indexed, name) = eng.query_slot("record.x").unwrap();
    assert_eq!(name, "x");
    assert!(Rc::ptr_eq(&indexed, winner));
    assert_eq!(
        winner.borrow().slot("x").unwrap().state,
        SlotState::Unforced
    );
    assert!(matches!(
        winner.borrow().slot("x").unwrap().value,
        Value::Undef
    ));
    assert_eq!(untouched.borrow().slot("x").unwrap().state, SlotState::Ok);
    assert_eq!(
        computations.get(),
        0,
        "preparation does not execute descriptors"
    );
    assert!(matches!(
        eng.force_slot(winner, "x").ok().unwrap(),
        Value::Int(Num::Small(value)) if value == expected
    ));
    assert_eq!(computations.get(), 1);
    assert_eq!(untouched.borrow().slot("x").unwrap().state, SlotState::Ok);
    edits.finish(&eng);
}

struct OnDrop(Option<Box<dyn FnOnce()>>);

impl Drop for OnDrop {
    fn drop(&mut self) {
        if let Some(run) = self.0.take() {
            run();
        }
    }
}

#[test]
fn inactive_finish_allows_native_drop_observation_without_fabricated_preparation() {
    let eng = Engine::bare(Env::new());
    let edits = Rc::new(Edits::default());
    let seen = Rc::new(Cell::new(false));
    let observed = seen.clone();
    let weak_edits = Rc::downgrade(&edits);
    let weak_engine = Rc::downgrade(&eng);
    let capture = OnDrop(Some(Box::new(move || {
        let edits = weak_edits.upgrade().unwrap();
        let report = edits.diagnostics();
        assert_eq!(report.sequence, 0);
        assert_eq!(report.finish_sequence, 1);
        assert!(!report.finish_completed);
        // The existing prune callback remains free to replace public roots
        // and reenter an independent edit on the same Engine.
        let eng = weak_engine.upgrade().unwrap();
        eng.env.set_root("from_drop", Value::Bool(true));
        let other = Edits::default();
        other.finish(&eng);
        assert!(other.diagnostics().finish_completed);
        observed.set(true);
    })));
    let native: NatFn = Rc::new(Box::new(move |_| {
        let _ = &capture;
        Ok(Value::Null)
    }));
    let weak_native = Rc::downgrade(&native);
    edits
        .roots
        .borrow_mut()
        .insert("expired".into(), RootInput::Doc(Value::Nat(native)));
    edits.finish(&eng);
    assert!(seen.get());
    assert!(weak_native.upgrade().is_none());
    let first = edits.diagnostics();
    assert!(first.finish_completed);
    assert!(!first.finish_was_active);
    assert!(!first.preparation_completed);
    assert_eq!(first.sequence, 0);
    assert!(first.preparation.iter().all(|p| !p.recorded));
    phases_are_disjoint(&first.finish[..1]);
    assert!(first.finish[1..].iter().all(|p| !p.recorded));
    assert_eq!(first.finish_work.root_entries[..2], [1, 0]);
    edits.finish(&eng);
    let next = edits.diagnostics();
    assert_eq!((next.sequence, next.finish_sequence), (0, 2));
    assert_eq!(first.finish_sequence, 1);
    assert_eq!(
        (
            next.finish_aggregate.calls,
            next.finish_aggregate.active_calls
        ),
        (2, 0)
    );
    assert_eq!(next.finish_aggregate.phases[0].recorded_calls, 2);
    assert!(next.finish_aggregate.phases[1..]
        .iter()
        .all(|p| p.recorded_calls == 0));
}

#[test]
fn failed_preparation_records_partial_interval_and_releases_diagnostic_state() {
    let eng = Engine::bare(Env::new());
    let edits = Edits::default();
    let record = instance();
    eng.env.registry_push(record.clone());
    // A native caller holding a mutable public record borrow already makes
    // the original root census panic. Attribution must neither mask that
    // panic nor leave a diagnostic borrow behind.
    let held = record.borrow_mut();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        edits.begin(&eng, &[], &HashMap::new());
    }));
    assert!(failed.is_err());
    drop(held);
    let partial = edits.diagnostics();
    assert_eq!(partial.sequence, 1);
    assert!(!partial.preparation_completed);
    assert!(partial.preparation[0].recorded);
    assert!(!partial.preparation[0].completed);
    assert!(partial.preparation[1..].iter().all(|p| !p.recorded));
    edits.begin(&eng, &[], &HashMap::new());
    let complete = edits.diagnostics();
    assert_eq!(complete.sequence, 2);
    assert!(complete.preparation_completed);
    phases_are_disjoint(&complete.preparation);
    assert!(!partial.preparation_completed);
    edits.finish(&eng);
    let held = edits.roots.borrow_mut();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| edits.finish(&eng)));
    assert!(failed.is_err());
    drop(held);
    let partial = edits.diagnostics();
    assert_eq!(partial.finish_aggregate.partial_calls, 1);
    assert!(!partial.finish_aggregate.valid);
    assert!(!partial.finish_completed);
    assert!(partial.finish[0].recorded && !partial.finish[0].completed);
    edits.begin(&eng, &[], &HashMap::new());
    assert_eq!(edits.diagnostics().finish_aggregate.partial_calls, 0);
    assert!(edits.diagnostics().finish_aggregate.valid);
    edits.finish(&eng);
}

#[test]
fn finish_aggregate_marks_native_reentry_without_double_counting_outer_time() {
    let eng = Engine::bare(Env::new());
    let edits = Rc::new(Edits::default());
    edits.begin(&eng, &[], &HashMap::new());
    let observed = Rc::new(Cell::new(None));
    let capture_observed = observed.clone();
    let weak_edits = Rc::downgrade(&edits);
    let weak_engine = Rc::downgrade(&eng);
    let capture = OnDrop(Some(Box::new(move || {
        let edits = weak_edits.upgrade().unwrap();
        // The public active flag lets an embedding enter an inactive finish
        // while a previous active finish releases an old native value.
        edits.active.set(false);
        edits.finish(&weak_engine.upgrade().unwrap());
        capture_observed.set(Some(edits.diagnostics()));
    })));
    let native: NatFn = Rc::new(Box::new(move |_| {
        let _ = &capture;
        Ok(Value::Null)
    }));
    let weak_native = Rc::downgrade(&native);
    edits
        .root_values
        .borrow_mut()
        .insert("old".into(), Value::Nat(native));
    edits.finish(&eng);
    let nested = observed.get().unwrap();
    let final_report = edits.diagnostics();
    assert!(weak_native.upgrade().is_none());
    assert_eq!(
        (final_report.sequence, final_report.finish_sequence),
        (1, 2)
    );
    assert!(
        !final_report.finish_was_active,
        "newest call survives the outer return"
    );
    let aggregate = final_report.finish_aggregate;
    assert_eq!(
        (
            aggregate.calls,
            aggregate.active_calls,
            aggregate.partial_calls,
            aggregate.superseded_calls
        ),
        (2, 1, 0, 1)
    );
    assert!(
        !aggregate.valid,
        "the omitted overlapping scope is explicit"
    );
    for (total, inner) in aggregate.phases.iter().zip(&nested.finish_aggregate.phases) {
        assert_eq!(total.recorded_calls, inner.recorded_calls);
        assert_eq!((total.wall_ns, total.cpu_ns), (inner.wall_ns, inner.cpu_ns));
    }
}

#[test]
fn superseded_finish_does_not_write_into_a_new_preparation_aggregate() {
    let eng = Engine::bare(Env::new());
    let edits = Rc::new(Edits::default());
    edits.begin(&eng, &[], &HashMap::new());
    let weak_edits = Rc::downgrade(&edits);
    let weak_engine = Rc::downgrade(&eng);
    let capture = OnDrop(Some(Box::new(move || {
        weak_edits
            .upgrade()
            .unwrap()
            .begin(&weak_engine.upgrade().unwrap(), &[], &HashMap::new());
    })));
    let native: NatFn = Rc::new(Box::new(move |_| {
        let _ = &capture;
        Ok(Value::Null)
    }));
    edits
        .roots
        .borrow_mut()
        .insert("expired".into(), RootInput::Doc(Value::Nat(native)));
    edits.finish(&eng);
    let next = edits.diagnostics();
    assert_eq!(next.sequence, 2);
    assert!(next.preparation_completed);
    assert!(!next.finish_completed);
    assert_eq!(next.finish_aggregate.calls, 0);
    assert!(next.finish.iter().all(|p| !p.recorded));
    edits.finish(&eng);
    assert_eq!(edits.diagnostics().finish_aggregate.calls, 1);
}
