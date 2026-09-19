//! Native cache membership and destructor reentry at Session prune boundaries.
use super::*;
use crate::session::{BindSource, EditKind, Mode, Op, Session};

fn instance(name: &str) -> Inst {
    record_instance(RecInst {
        type_name: None,
        rt: ty(RTk::Any),
        path: Rc::new(vec![Seg::Name(name.into())]),
        ps: RefCell::new(None),
        parent: None,
        slots: Vec::new(),
        entry_order: Vec::new().into(),
        extras: Vec::new(),
        menv: None,
    })
}

fn remember(edits: &Edits, inst: &Inst, value: Value) {
    edits.inputs.borrow_mut().insert(
        Rc::as_ptr(inst) as usize,
        (Rc::downgrade(inst), vec![("value".into(), value)]),
    );
}

struct OnDrop(Option<Box<dyn FnOnce()>>);

impl Drop for OnDrop {
    fn drop(&mut self) {
        if let Some(run) = self.0.take() {
            run();
        }
    }
}

fn drop_value(run: impl FnOnce() + 'static) -> Value {
    let capture = OnDrop(Some(Box::new(run)));
    Value::native(move |_| {
        let _ = &capture;
        Ok(Value::Null)
    })
}

#[test]
fn prune_uses_current_registry_membership_not_retained_engine_ownership() {
    let eng = Engine::bare(Env::new());
    let old = Engine::bare_with_queries(Env::new(), eng.query_pool());
    let edits = Edits::default();
    let current = instance("current");
    let foreign = instance("foreign");
    let replacement = instance("replacement");
    old.env.registry_push(foreign.clone());
    old.register_query_slot("foreign.value", foreign.clone(), "value".into());
    old.replace_query_reads(
        "foreign.value",
        [old.query_id("old.dependency")].into_iter().collect(),
    );
    let old_reads = old.query_dependencies();
    eng.env.registry_push(current.clone());
    eng.env.registry_push(current.clone());
    remember(&edits, &current, Value::Bool(true));
    let foreign_input: Rc<str> = Rc::from("old input description");
    let weak_input = Rc::downgrade(&foreign_input);
    remember(&edits, &foreign, Value::Str(foreign_input.into()));
    let gone = instance("gone");
    remember(&edits, &gone, Value::Null);
    drop(gone);

    // Pruning needs only addresses, never a borrow of record bodies.
    let current_mut = current.borrow_mut();
    edits.prune(&eng);
    drop(current_mut);
    assert_eq!(edits.cached_inputs(), 1);
    assert!(weak_input.upgrade().is_none());
    assert!(Rc::ptr_eq(
        &old.query_slot("foreign.value").unwrap().0,
        &foreign
    ));
    assert_eq!(old.query_dependencies(), old_reads);

    // The public registry handle itself can be replaced between prunes.
    *eng.env.registry.borrow_mut() = Rc::new(RefCell::new(vec![replacement.clone()]));
    remember(&edits, &replacement, Value::Bool(false));
    edits.prune(&eng);
    assert_eq!(edits.cached_inputs(), 1);
    assert!(edits
        .inputs
        .borrow()
        .contains_key(&(Rc::as_ptr(&replacement) as usize)));
    assert!(!edits
        .inputs
        .borrow()
        .contains_key(&(Rc::as_ptr(&current) as usize)));
}

#[test]
fn input_drop_can_replace_registry_after_membership_snapshot() {
    let eng = Engine::bare(Env::new());
    let edits = Edits::default();
    let keep = instance("keep");
    let expired = instance("expired");
    eng.env.registry_push(keep.clone());
    remember(&edits, &keep, Value::Bool(true));
    let observed = Rc::new(Cell::new(false));
    let on_drop = observed.clone();
    let weak_engine = Rc::downgrade(&eng);
    remember(
        &edits,
        &expired,
        drop_value(move || {
            let eng = weak_engine.upgrade().unwrap();
            let registry = eng.env.registry.borrow().clone();
            registry.borrow_mut().clear();
            *eng.env.registry.borrow_mut() = Rc::new(RefCell::new(Vec::new()));
            eng.env.set_root("from_input_drop", Value::Bool(true));
            // Independent native reentry can use the same current Engine.
            let other = Edits::default();
            other.prune(&eng);
            assert!(matches!(
                eng.env.root("from_input_drop"),
                Some(Value::Bool(true))
            ));
            on_drop.set(true);
        }),
    );
    edits
        .roots
        .borrow_mut()
        .insert("from_input_drop".into(), RootInput::Doc(Value::Bool(true)));
    drop(expired);

    edits.prune(&eng);
    assert!(observed.get());
    assert_eq!(eng.env.registry_len(), 0);
    assert_eq!(
        edits.cached_inputs(),
        1,
        "membership precedes cached-value drops"
    );
    assert!(edits.roots.borrow().contains_key("from_input_drop"));
    edits.prune(&eng);
    assert_eq!(
        edits.cached_inputs(),
        0,
        "the next prune sees the replacement"
    );
}

#[test]
fn root_cache_drop_can_replace_public_roots_without_a_held_borrow() {
    let eng = Engine::bare(Env::new());
    let edits = Edits::default();
    let observed = Rc::new(Cell::new(false));
    let on_drop = observed.clone();
    let weak_engine = Rc::downgrade(&eng);
    edits.roots.borrow_mut().insert(
        "expired".into(),
        RootInput::Doc(drop_value(move || {
            let eng = weak_engine.upgrade().unwrap();
            let roots = eng.env.roots.borrow().clone();
            roots.borrow_mut().clear();
            *eng.env.roots.borrow_mut() = Rc::new(RefCell::new(vec![
                ("".into(), Value::Undef),
                ("한글".into(), Value::Absent),
                ("duplicate".into(), Value::Null),
                ("duplicate".into(), Value::Bool(false)),
            ]));
            assert!(root_exists(&eng, ""));
            on_drop.set(true);
        })),
    );
    edits.prune(&eng);
    assert!(observed.get());
    assert!(edits.roots.borrow().is_empty());
    for name in ["", "한글", "duplicate", "missing"] {
        edits
            .roots
            .borrow_mut()
            .insert(name.into(), RootInput::Doc(Value::Null));
    }
    edits.prune(&eng);
    assert_eq!(edits.roots.borrow().len(), 3);
    assert!(!edits.roots.borrow().contains_key("missing"));
    assert_eq!(eng.env.root_names(), ["", "한글", "duplicate", "duplicate"]);
}

#[test]
fn finish_rechecks_roots_after_removed_slot_capture_mutates_them() {
    let eng = Engine::bare(Env::new());
    let edits = Edits::default();
    let root_drops = Rc::new(Cell::new(0));
    let observed = root_drops.clone();
    edits.roots.borrow_mut().insert(
        "survivor".into(),
        RootInput::Doc(drop_value(move || observed.set(observed.get() + 1))),
    );
    eng.env.set_root("survivor", Value::Null);
    eng.replace_query_reads(
        "root:survivor",
        [eng.query_id("dependency")].into_iter().collect(),
    );
    let weak_engine = Rc::downgrade(&eng);
    let capture = OnDrop(Some(Box::new(move || {
        let eng = weak_engine.upgrade().unwrap();
        assert!(eng.env.root("survivor").is_some());
        eng.env.remove_root("survivor");
    })));
    let removed = instance("removed");
    removed.borrow_mut().slots.push((
        "value".into(),
        Slot {
            kind: MKind::Der,
            hidden: false,
            state: SlotState::Unforced,
            value: Value::Undef,
            compute: Some(Rc::new(Compute::Bridge(Rc::new(move || {
                let _ = &capture;
                Ok(Value::Null)
            })))),
        },
    ));
    eng.register_query_slot("removed.value", removed, "value".into());
    edits.active.set(true);
    edits.finish(&eng);
    assert!(!edits.active.get());
    assert_eq!(root_drops.get(), 1);
    assert!(edits.roots.borrow().is_empty());
    assert!(eng.query_slot("removed.value").is_none());
    assert!(eng.query_reads("root:survivor").is_none());
}

#[test]
fn session_equal_changed_restore_keeps_only_current_input_descriptions() {
    let entry = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/private/prune_session.decl");
    let overlay = HashMap::from([(
        entry.clone(),
        "type Item = { n: int, doubled = n * 2 }\nfunc make(n: int): Item = { n: n }\ninput item: Item\nexport output result: int = item.doubled\n".into(),
    )]);
    let mut session = Session::with_overlay(entry.to_str(), Some(&overlay));
    session
        .apply(Op::Bind {
            name: "item".into(),
            src: BindSource::Inline {
                text: "{\"n\":1}".into(),
            },
        })
        .unwrap();
    let first = session.run(Mode::Full);
    assert!(first.diags.is_empty());
    let initial = first
        .eng
        .as_ref()
        .unwrap()
        .edits
        .borrow()
        .as_ref()
        .unwrap()
        .cached_inputs();
    assert_eq!(initial, 1);
    // Retain earlier public Runs while current edits and scratch are pruned.
    let mut retained = vec![first];
    for n in [1, 2, 1] {
        session
            .apply(Op::Edit {
                kind: EditKind::Update,
                path: "item.n".into(),
                expr: Some(n.to_string()),
            })
            .unwrap();
        let run = session.run(Mode::Full);
        assert!(run.load_diags.is_empty());
        assert!(run.checks.is_empty());
        assert!(run.diags.is_empty());
        let eng = run.eng.as_ref().unwrap();
        let edits = eng.edits.borrow().clone().unwrap();
        assert_eq!(edits.cached_inputs(), initial);
        let result = eng.env.root("result").unwrap();
        assert_eq!(eng.serialize(&result, "result", false), (n * 2).to_string());
        let scratch = session.evaluate_expr("make(9).n").unwrap();
        assert!(scratch.error.is_none());
        assert_eq!(scratch.value.as_deref(), Some("9"));
        edits.prune(eng);
        assert_eq!(edits.cached_inputs(), initial);
        assert!(edits.unchanged(
            &match eng.env.root("item").unwrap() {
                Value::Rec(inst) => inst,
                _ => panic!("bound Item"),
            },
            &[("n".into(), Value::Int(Num::Small(n)))],
        ));
        retained.push(run);
    }
    assert_eq!(retained.len(), 4);
}

#[test]
fn first_input_index_borrows_payloads_and_keeps_duplicate_first_occurrences() {
    let first: Rc<str> = Rc::from("first payload");
    let last: Rc<str> = Rc::from("last payload");
    let first_weak = Rc::downgrade(&first);
    let last_weak = Rc::downgrade(&last);
    let mut entries: Vec<_> = (0..64)
        .map(|i| (format!("key_{i}"), Value::Bool(true)))
        .collect();
    entries[0] = ("".into(), Value::Str(first.into()));
    entries[1] = ("한글".into(), Value::Null);
    entries.push(("".into(), Value::Str(last.into())));
    entries.push(("한글".into(), Value::Bool(false)));
    let lookup = FirstInputs::new(&entries, entries.len());
    assert_eq!(lookup.index.as_ref().unwrap().len(), 64);
    assert!(std::ptr::eq(lookup.get("").unwrap(), &entries[0].1));
    assert!(matches!(lookup.get("한글"), Some(Value::Null)));
    assert!(lookup.get("missing").is_none());
    assert_eq!(first_weak.strong_count(), 1);
    assert_eq!(last_weak.strong_count(), 1);

    // A wide single search and bounded small search work keep the slice path.
    assert!(FirstInputs::new(&entries, 1).index.is_none());
    assert!(FirstInputs::new(&entries[..8], 100).index.is_none());
    assert!(FirstInputs::new(&entries[..9], 7).index.is_none());
    assert!(FirstInputs::new(&entries[..9], 8).index.is_some());
    drop(lookup);
    assert_eq!(first_weak.strong_count(), 1);
    assert_eq!(last_weak.strong_count(), 1);
    drop(entries);
    assert!(first_weak.upgrade().is_none());
    assert!(last_weak.upgrade().is_none());
}

#[test]
fn captured_text_memo_uses_content_keys_without_copying_or_retaining_losing_text() {
    let eng = Engine::bare(Env::new());
    let chars: Rc<str> = Rc::from("same-한글-key");
    let weak = Rc::downgrade(&chars);
    let original = Value::Str(SharedText::from_rc(chars.clone()));
    let mut memo = FxHashMap::default();
    let matches = capture(&original, &eng, &mut memo);
    let Some(CaptureKey::String(key)) = memo.keys().next() else {
        panic!("string capture key");
    };
    assert!(
        Rc::ptr_eq(key, &chars),
        "the cache retains the original character owner"
    );

    let fresh_chars: Rc<str> = Rc::from("same-한글-key");
    assert!(!Rc::ptr_eq(&fresh_chars, &chars));
    let fresh_weak = Rc::downgrade(&fresh_chars);
    let fresh = Value::Str(SharedText::from_rc(fresh_chars));
    let equal_matches = capture(&fresh, &eng, &mut memo);
    assert!(
        Rc::ptr_eq(&matches, &equal_matches),
        "equal text reuses its captured matcher"
    );
    assert_eq!(memo.len(), 1);
    assert!(matches(&fresh, &eng));
    drop(fresh);
    assert!(
        fresh_weak.upgrade().is_none(),
        "a cache hit retains no losing character owner"
    );

    let mut changed = original.clone();
    let Value::Str(text) = &mut changed else {
        unreachable!();
    };
    text.make_mut().make_ascii_uppercase();
    assert!(!matches(&changed, &eng));
    assert!(matches(&original, &eng));
    drop(original);
    drop(chars);
    drop(memo);
    drop(matches);
    assert!(
        weak.upgrade().is_some(),
        "the retained matcher still owns its snapshot"
    );
    drop(equal_matches);
    assert!(weak.upgrade().is_none());
}

fn cached_slot(value: Value) -> Slot {
    Slot {
        kind: MKind::Req,
        hidden: false,
        state: SlotState::Ok,
        value,
        compute: None,
    }
}

fn raw_leaf(value: Value) -> Value {
    Value::JObj(Rc::new(vec![("leaf".into(), value)]))
}

#[test]
fn wide_edit_diff_preserves_first_duplicates_and_missing_undefined_distinction() {
    for map in [false, true] {
        let eng = Engine::bare(Env::new());
        let mut members = Vec::new();
        let mut before = Vec::new();
        for i in 0..16 {
            let child = instance(&format!("child_{i}"));
            child
                .borrow_mut()
                .slots
                .push(("leaf".into(), cached_slot(Value::Bool(true))));
            members.push((format!("key_{i}"), Value::Rec(child)));
            before.push((format!("key_{i}"), raw_leaf(Value::Bool(true))));
        }
        before.push(("key_0".into(), raw_leaf(Value::Bool(false))));
        let value = if map {
            Value::Map(Rc::new(RefCell::new(MapV {
                path: PrefixPath::from(vec![Seg::Name("bound".into())]),
                entries: members.into_iter().collect(),
            })))
        } else {
            let record = instance("bound");
            record.borrow_mut().slots = members
                .into_iter()
                .map(|(name, value)| (Rc::from(name), cached_slot(value)))
                .collect();
            Value::Rec(record)
        };
        let mut next = before.clone();
        next.last_mut().unwrap().1 = Value::Null;
        let mut forced = FxHashSet::default();
        let mut known = FxHashSet::default();
        Edits::diff(
            &eng,
            &Value::JObj(Rc::new(before.clone())),
            &Value::JObj(Rc::new(next.clone())),
            &value,
            "root:source",
            &mut forced,
            &mut known,
        );
        assert_eq!(forced, FxHashSet::from_iter(["root:source".into()]));
        assert_eq!(known, forced, "later duplicates do not invalidate children");

        next[0].1 = raw_leaf(Value::Bool(false));
        forced.clear();
        known.clear();
        Edits::diff(
            &eng,
            &Value::JObj(Rc::new(before.clone())),
            &Value::JObj(Rc::new(next)),
            &value,
            "root:source",
            &mut forced,
            &mut known,
        );
        let mut expected = FxHashSet::from_iter(["root:source".into(), "child_0.leaf".into()]);
        if !map {
            expected.insert("bound.key_0".into());
        }
        assert_eq!(forced, expected);
        assert!(eng.query_slot("child_0.leaf").is_some());
        assert!(eng.query_slot("child_1.leaf").is_none());

        // A record's supplied/absent distinction survives Undef fallback; the
        // map comparison intentionally treats missing and raw Undef alike.
        before[2].1 = Value::Undef;
        let mut next = before.clone();
        next.remove(2);
        forced.clear();
        known.clear();
        Edits::diff(
            &eng,
            &Value::JObj(Rc::new(before)),
            &Value::JObj(Rc::new(next)),
            &value,
            "root:source",
            &mut forced,
            &mut known,
        );
        let mut expected = FxHashSet::from_iter(["root:source".into()]);
        if !map {
            expected.insert("bound.key_2".into());
        }
        assert_eq!(forced, expected);
    }
}

#[test]
fn wide_record_reuse_keeps_first_lookup_and_snapshot_across_native_drop() {
    let eng = Engine::bare(Env::new());
    let edits = Edits::default();
    let record = instance("record");
    let mut before: Vec<_> = (0..16)
        .map(|i| (format!("key_{i}"), Value::Bool(true)))
        .collect();
    before.push(("key_0".into(), Value::Bool(false)));
    record.borrow_mut().entry_order = Rc::new(before.iter().map(|(k, _)| k.clone()).collect());
    edits.inputs.borrow_mut().insert(
        Rc::as_ptr(&record) as usize,
        (Rc::downgrade(&record), before.clone()),
    );
    assert!(!edits.unchanged(&record, &before));
    let mut equalized = before.clone();
    equalized.last_mut().unwrap().1 = Value::Bool(true);
    assert!(edits.unchanged(&record, &equalized));
    let mut reordered = equalized.clone();
    reordered.swap(2, 3);
    assert!(!edits.unchanged(&record, &reordered));

    let mut next = equalized;
    next[1].1 = Value::Bool(false);
    let external = Rc::new(RefCell::new(Rc::new(next.clone())));
    let raw = external.borrow().clone();
    let observed = Rc::new(Cell::new(false));
    let on_drop = observed.clone();
    let input = external.clone();
    let weak_engine = Rc::downgrade(&eng);
    let capture = OnDrop(Some(Box::new(move || {
        Rc::make_mut(&mut input.borrow_mut())[15].1 = Value::Bool(false);
        let eng = weak_engine.upgrade().unwrap();
        eng.env.set_root("during_old_slot_drop", Value::Bool(true));
        Edits::default().prune(&eng);
        on_drop.set(true);
    })));
    let mut old_slots = Vec::new();
    for i in 0..16 {
        let value = Value::Bool(i != 0);
        let mut old = cached_slot(value.clone());
        old.compute = Some(Rc::new(Compute::Bridge(Rc::new(move || Ok(value.clone())))));
        old_slots.push((Rc::from(format!("key_{i}")), old));
        record.borrow_mut().slots.push((
            Rc::from(format!("key_{i}")),
            Slot {
                state: SlotState::Unforced,
                compute: Some(Rc::new(Compute::Bridge(Rc::new(|| Ok(Value::Null))))),
                ..cached_slot(Value::Undef)
            },
        ));
    }
    old_slots[1].1.compute = Some(Rc::new(Compute::Bridge(Rc::new(move || {
        let _ = &capture;
        Ok(Value::Bool(true))
    }))));
    let duplicate_compute = old_slots[0].1.compute.clone().unwrap();
    let future_compute = old_slots[15].1.compute.clone().unwrap();
    edits.active.set(true);
    edits.bound(&eng, &record, raw.as_slice(), old_slots);
    assert!(observed.get());
    assert!(matches!(external.borrow()[15].1, Value::Bool(false)));
    assert!(matches!(raw[15].1, Value::Bool(true)));
    let bound = record.borrow();
    let duplicate = bound.slot("key_0").unwrap();
    assert!(matches!(duplicate.value, Value::Bool(false)));
    assert!(Rc::ptr_eq(
        duplicate.compute.as_ref().unwrap(),
        &duplicate_compute
    ));
    assert_eq!(bound.slot("key_1").unwrap().state, SlotState::Unforced);
    let future = bound.slot("key_15").unwrap();
    assert_eq!(future.state, SlotState::Ok);
    assert!(Rc::ptr_eq(
        future.compute.as_ref().unwrap(),
        &future_compute
    ));
    drop(bound);
    drop(raw);
    drop(external);
    assert!(
        edits.unchanged(&record, &next),
        "the cache still owns its snapshot"
    );
    assert!(matches!(
        eng.env.root("during_old_slot_drop"),
        Some(Value::Bool(true))
    ));
}
