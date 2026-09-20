//! Rust-native snapshot, reset-callback and fallback ownership boundaries.
use super::*;
use crate::parse::parse_source;
use crate::pipeline::run_pipeline;

#[test]
fn shared_map_rebase_and_cycle_copy_preserve_keys_without_sharing_value_storage() {
    {
        let engine = fixture();
        let cache = RoundCache::default();
        let previous = cache.advance(&engine, &HashMap::new()).unwrap();
        let mut pool = MapShapePool::default();
        let unchanged = map_value(vec![("stable".into(), Value::Int(7.into()))]);
        let Value::Map(map) = &unchanged else {
            unreachable!()
        };
        assert!(pool.seal(&mut map.borrow_mut().entries));
        assert_eq!(
            identity(&cache.value(&unchanged, &engine)),
            identity(&unchanged)
        );
        for changed in [0, 1, 2] {
            let reference = Rc::new(vec![Seg::Name("result".into()), Seg::Name("left".into())]);
            engine
                .round_refs
                .borrow_mut()
                .insert(Rc::as_ptr(&reference) as usize, reference.clone());
            let keys = ["z", "a", "한글😀"];
            let entries = keys
                .iter()
                .enumerate()
                .map(|(i, key)| {
                    (
                        (*key).into(),
                        if i == changed {
                            Value::Ref(reference.clone())
                        } else {
                            Value::Int((i as i64 + 7).into())
                        },
                    )
                })
                .collect();
            let input = map_value(entries);
            let Value::Map(original) = &input else {
                unreachable!()
            };
            assert!(pool.seal(&mut original.borrow_mut().entries));
            let Value::Map(rebased) = cache.value(&input, &engine) else {
                panic!("rebased map")
            };
            assert!(!Rc::ptr_eq(original, &rebased));
            assert!(rebased.borrow().entries.is_shared());
            assert!(std::ptr::eq(
                original.borrow().entries.get_index(0).unwrap().0,
                rebased.borrow().entries.get_index(0).unwrap().0
            ));
            assert_eq!(
                rebased
                    .borrow()
                    .entries
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                keys
            );
            assert_eq!(original.borrow().path, rebased.borrow().path);
            for (i, key) in keys.iter().enumerate() {
                let result = rebased.borrow();
                let value = result.get(key).unwrap();
                if i == changed {
                    let Value::Ref(path) = value else {
                        panic!("rebased reference")
                    };
                    assert!(!Rc::ptr_eq(path, &reference));
                    assert!(Rc::ptr_eq(
                        &engine.snap_refs.borrow()[&(Rc::as_ptr(path) as usize)].1,
                        &previous
                    ));
                } else {
                    assert!(value_eq(value, &Value::Int((i as i64 + 7).into())));
                }
            }
            assert_eq!(
                identity(&cache.value(&input, &engine)),
                identity(&Value::Map(rebased.clone()))
            );
            rebased
                .borrow_mut()
                .set(keys[changed].into(), Value::Bool(true));
            assert!(
                matches!(original.borrow().get(keys[changed]), Some(Value::Ref(path)) if Rc::ptr_eq(path, &reference))
            );
        }
    }

    let engine = fixture();
    let cache = RoundCache::default();
    let frozen = Engine::bare_with_queries(engine.env.clone(), engine.query_pool());
    let live = snapshot_cycle(&engine, Value::Int(7.into()));
    let Value::Arr(live_array) = live.borrow().slot("left").unwrap().value.clone() else {
        panic!("live array")
    };
    let Value::Map(live_map) = live_array.borrow().items[1].clone() else {
        panic!("live map")
    };
    assert!(MapShapePool::default().seal(&mut live_map.borrow_mut().entries));
    let mut memo = CopyMemo::default();
    let copied = record(
        cache
            .copy(&Value::Rec(live.clone()), &engine, &frozen, &mut memo)
            .unwrap(),
    );
    let Value::Arr(array) = copied.borrow().slot("left").unwrap().value.clone() else {
        panic!("copied array")
    };
    let Value::Map(map) = array.borrow().items[1].clone() else {
        panic!("copied map")
    };
    assert!(!Rc::ptr_eq(&live_map, &map));
    assert!(map.borrow().entries.is_shared());
    assert!(std::ptr::eq(
        live_map.borrow().entries.get_index(0).unwrap().0,
        map.borrow().entries.get_index(0).unwrap().0
    ));
    assert!(matches!(&array.borrow().items[2], Value::Map(alias) if Rc::ptr_eq(alias, &map)));
    assert!(
        matches!(map.borrow().get("root"), Some(Value::Rec(back)) if Rc::ptr_eq(back, &copied))
    );
    assert!(
        matches!(map.borrow().get("array"), Some(Value::Arr(back)) if Rc::ptr_eq(back, &array))
    );
    assert_eq!(
        (memo.records.len(), memo.arrays.len(), memo.maps.len()),
        (1, 1, 1)
    );
    map.borrow_mut().set("array".into(), Value::Int(19.into()));
    assert!(map.borrow().entries.is_shared());
    assert!(
        matches!(live_map.borrow().get("array"), Some(Value::Arr(back)) if Rc::ptr_eq(back, &live_array))
    );
    map.borrow_mut().set("new".into(), Value::Bool(true));
    assert!(!map.borrow().entries.is_shared() && live_map.borrow().entries.is_shared());
    assert!(!live_map.borrow().has("new"));
    let weak_record = Rc::downgrade(&copied);
    let weak_array = Rc::downgrade(&array);
    let weak_map = Rc::downgrade(&map);
    drop((memo, copied, array, map));
    collect_cycles();
    assert!(
        weak_record.upgrade().is_none()
            && weak_array.upgrade().is_none()
            && weak_map.upgrade().is_none()
    );
}

#[test]
fn failed_shared_map_copy_does_not_publish_partial_values_or_retire_live_native_owners() {
    struct OnDrop {
        observer: std::rc::Weak<Engine>,
        calls: Rc<Cell<usize>>,
    }
    impl Drop for OnDrop {
        fn drop(&mut self) {
            let observer = self.observer.upgrade().unwrap();
            assert!(observer.reads.try_borrow_mut().is_ok());
            observer
                .env
                .set_root("from_shared_map_drop", Value::Bool(true));
            self.calls.set(self.calls.get() + 1);
        }
    }
    let engine = fixture();
    let cache = RoundCache::default();
    let frozen = Engine::bare_with_queries(engine.env.clone(), engine.query_pool());
    let observer = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let capture = OnDrop {
        observer: Rc::downgrade(&observer),
        calls: calls.clone(),
    };
    let native: NatFn = Rc::new(Box::new(move |_| {
        let _ = &capture;
        panic!("snapshot copying must not execute native payloads")
    }));
    let native_weak = Rc::downgrade(&native);
    let root = record(engine.env.root("result").unwrap());
    let Value::Map(map) = map_value(Vec::new()) else {
        unreachable!()
    };
    map.borrow_mut().entries.extend([
        ("self".into(), Value::Map(map.clone())),
        ("record".into(), Value::Rec(root.clone())),
        (
            "native".into(),
            Value::JObj(Rc::new(vec![("payload".into(), Value::Nat(native))])),
        ),
        ("reject".into(), Value::pattern("not copyable")),
    ]);
    root.borrow_mut().slot_mut("left").unwrap().value = Value::Map(map.clone());
    assert!(MapShapePool::default().seal(&mut map.borrow_mut().entries));
    let mut memo = CopyMemo::default();
    assert!(cache
        .copy(&Value::Map(map.clone()), &engine, &frozen, &mut memo)
        .is_none());
    let destination = memo.maps.get(&(Rc::as_ptr(&map) as usize)).unwrap();
    assert!(destination.borrow().entries.is_empty());
    assert!(
        !destination.borrow().entries.is_shared(),
        "success storage is installed only after every child copies"
    );
    let weak_destination = Rc::downgrade(destination);
    let partial_records: Vec<_> = memo.records.values().map(Rc::downgrade).collect();
    assert!(!partial_records.is_empty());
    drop(memo);
    collect_cycles();
    assert!(weak_destination.upgrade().is_none());
    assert!(partial_records
        .iter()
        .all(|value| value.upgrade().is_none()));
    assert_eq!(calls.get(), 0);
    assert_eq!(
        native_weak.strong_count(),
        1,
        "only the unchanged original opaque input retains the native function"
    );
    assert!(map.borrow().entries.is_shared());
    assert!(matches!(map.borrow().get("self"), Some(Value::Map(back)) if Rc::ptr_eq(back, &map)));
    map.borrow_mut().entries.clear();
    assert_eq!(calls.get(), 1);
    assert!(native_weak.upgrade().is_none());
    assert!(matches!(
        observer.env.root("from_shared_map_drop"),
        Some(Value::Bool(true))
    ));
}

fn fixture() -> Rc<Engine> {
    let parsed = parse_source(
        "type Child = { x: int }\n\
         type Parent = { left: Child, right: Child }\n\
         export output result: Parent = { left: { x: 7 }, right: { x: 11 } }\n",
    );
    let pipeline = run_pipeline(&parsed.decls);
    assert!(pipeline.diags.is_empty());
    pipeline.eng
}

fn record(value: Value) -> Inst {
    match value {
        Value::Rec(record) => record,
        _ => panic!("expected record"),
    }
}

fn child(parent: &Inst, name: &str) -> Inst {
    record(parent.borrow().slot(name).expect("member").value.clone())
}

fn frozen_root(engine: &Engine) -> Inst {
    let roots = engine.frozen_roots.borrow();
    record(
        roots
            .as_ref()
            .expect("frozen roots")
            .iter()
            .find(|(name, _)| name == "result")
            .expect("result root")
            .1
            .clone(),
    )
}

#[test]
fn advance_checks_current_paths_after_native_mutation_leaves_a_cached_query_spelling() {
    let engine = fixture();
    let root = record(engine.env.root("result").unwrap());
    let left = child(&root, "left");
    assert_eq!(Engine::slot_key(&left, "x"), "result.left.x");
    left.borrow_mut().path = root.borrow().path.clone();
    assert_eq!(Engine::slot_key(&left, "x"), "result.left.x");

    // The actual paths now collide even though the cached query spellings do
    // not. Classification must reject before freezing or resetting anything.
    let cache = RoundCache::default();
    assert!(cache.advance(&engine, &HashMap::new()).is_none());
    assert_eq!(cache.reused_rounds.get(), 0);
    assert!(engine.prev.borrow().is_none());
    assert_eq!(left.borrow().slot("x").unwrap().state, SlotState::Ok);
}

#[test]
fn inverted_edges_borrow_sorted_sources_and_deduplicate_canonical_targets() {
    let key_target = vec![
        Seg::Name("result".into()),
        Seg::Key("a\"\\\n".into()),
        Seg::Idx(2),
    ];
    // A non-dot-spellable member and a map key share the same canonical text.
    let member_target = vec![
        Seg::Name("result".into()),
        Seg::Name("a\"\\\n".into()),
        Seg::Idx(2),
    ];
    let target_text = r#"result["a\"\\\n"][2]"#;
    let mut edge = Edge::new();
    edge.insert("z-source".into(), (Vec::new(), vec![key_target.clone()]));
    edge.insert(
        "a-source".into(),
        (
            Vec::new(),
            vec![key_target.clone(), member_target, key_target],
        ),
    );
    let mut edges = HashMap::new();
    edges.insert("Child|links".into(), edge);
    edges.insert("Empty|links".into(), Edge::new());

    let inverted = invert_edges(&edges);
    assert_eq!(inverted.len(), 2);
    assert!(inverted["Empty|links"].is_empty());
    let sources = &inverted["Child|links"][target_text];
    assert_eq!(inverted["Child|links"].len(), 1);
    assert_eq!(
        sources.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ["a-source", "z-source"]
    );
    for source in sources {
        let original = edges["Child|links"]
            .get_key_value(source.as_str())
            .unwrap()
            .0;
        assert!(std::ptr::eq(*source, original));
    }
    let original_key = edges.get_key_value("Child|links").unwrap().0;
    let borrowed_key = inverted.get_key_value("Child|links").unwrap().0;
    assert!(std::ptr::eq(*borrowed_key, original_key.as_str()));
}

#[test]
fn advance_releases_borrowed_edge_snapshot_before_reset_and_on_rejection() {
    let engine = fixture();
    let root = record(engine.env.root("result").unwrap());
    let left = child(&root, "left");
    let reader = Engine::slot_key(&left, "x");
    let target = vec![Seg::Name("result".into()), Seg::Name("left".into())];
    let mut edge = Edge::new();
    edge.insert("source".into(), (Vec::new(), vec![target.clone(), target]));
    let mut old_edges = HashMap::new();
    old_edges.insert("Child|links".into(), edge);
    engine.take_snapshot(old_edges.clone());
    let mut next_edges = old_edges;
    next_edges
        .get_mut("Child|links")
        .unwrap()
        .get_mut("source")
        .unwrap()
        .1
        .pop();
    engine.track.set(true);
    engine.step(&reader, || {
        engine.record("edge:Child|links|result.left".into())
    });
    let called = Rc::new(Cell::new(false));
    let observed = called.clone();
    let weak = Rc::downgrade(&engine);
    engine.round_resets.borrow_mut().push(Rc::new(move || {
        let engine = weak.upgrade().unwrap();
        assert!(engine
            .snap
            .try_borrow_mut()
            .expect("snapshot borrow released")
            .is_none());
        assert!(engine.reads.try_borrow_mut().is_ok());
        observed.set(true);
    }));
    let cache = RoundCache::default();
    let snapshot = cache
        .advance(&engine, &next_edges)
        .expect("duplicate-only edge change");
    assert!(called.get());
    assert_eq!(cache.invalidated_slots.get(), 0);
    assert_eq!(left.borrow().slot("x").unwrap().state, SlotState::Ok);
    assert!(value_eq(
        &child(&frozen_root(&snapshot), "left")
            .borrow()
            .slot("x")
            .unwrap()
            .value,
        &Value::Int(7.into())
    ));

    // Early classification fallback must release the additional Ref too.
    engine.take_snapshot(next_edges.clone());
    engine.step(&reader, || engine.record("edge:malformed".into()));
    called.set(false);
    assert!(cache.advance(&engine, &next_edges).is_none());
    assert!(!called.get());
    assert!(engine
        .snap
        .try_borrow_mut()
        .expect("fallback releases snapshot borrow")
        .is_some());
}

fn assert_snapshot(engine: &Engine, expected_x: i64) {
    let root = frozen_root(engine);
    let left = child(&root, "left");
    let right = child(&root, "right");
    assert!(Rc::ptr_eq(&left, &right), "frozen aliases stay shared");
    assert!(Rc::ptr_eq(
        left.borrow().parent.as_ref().expect("parent"),
        &root
    ));
    assert!(value_eq(
        &left.borrow().slot("x").expect("x").value,
        &Value::Int(expected_x.into())
    ));
    for record in engine.frozen_registry.borrow().as_ref().expect("registry") {
        for (_, slot) in &record.borrow().slots {
            assert!(slot.compute.is_none(), "frozen copies have no producers");
        }
    }
}

#[test]
fn advance_preserves_frozen_aliases_and_roots_removed_before_reset_collection() {
    let engine = fixture();
    let cache = RoundCache::default();
    let live_root = record(engine.env.root("result").expect("live result"));
    let live_left = child(&live_root, "left");
    live_root.borrow_mut().slot_mut("right").unwrap().value = Value::Rec(live_left.clone());
    let weak_root = Rc::downgrade(&live_root);
    let weak_left = Rc::downgrade(&live_left);
    let weak_env = Rc::downgrade(&engine.env);

    let first = cache
        .advance(&engine, &HashMap::new())
        .expect("first advance");
    assert_snapshot(&first, 7);
    assert!(!Rc::ptr_eq(&frozen_root(&first), &live_root));
    assert!(!Rc::ptr_eq(
        &child(&frozen_root(&first), "left"),
        &live_left
    ));
    live_left.borrow_mut().slot_mut("x").unwrap().value = Value::Int(19.into());
    assert_snapshot(&first, 7);
    drop(live_left);
    drop(live_root);

    // Dirty the whole root, so the live registry and root table are empty by
    // the reset callback. No callback capture owns a live record.
    engine.track.set(true);
    engine.step("root:result", || engine.record("round:nested".into()));
    let order = Rc::new(RefCell::new(Vec::new()));
    let observed = order.clone();
    let weak_engine = Rc::downgrade(&engine);
    let reset_root = weak_root.clone();
    let reset_left = weak_left.clone();
    engine.round_resets.borrow_mut().push(Rc::new(move || {
        let engine = weak_engine.upgrade().expect("live engine");
        assert_eq!(engine.env.registry_len(), 0);
        assert!(engine.env.root("result").is_none());
        // Collect before upgrading any weak record handle: the advance-local
        // registry, not this callback, must preserve the removed live layer.
        collect_cycles();
        assert!(reset_root.upgrade().is_some());
        assert!(reset_left.upgrade().is_some());
        let prior = engine.prev.borrow().clone().expect("previous snapshot");
        assert_snapshot(&prior, 7);
        observed.borrow_mut().push(1);
    }));
    let observed = order.clone();
    let reset_root = weak_root.clone();
    engine.round_resets.borrow_mut().push(Rc::new(move || {
        assert!(reset_root.upgrade().is_some());
        observed.borrow_mut().push(2);
    }));

    let second = cache
        .advance(&engine, &HashMap::new())
        .expect("second advance");
    assert_eq!(*order.borrow(), [1, 2]);
    assert_eq!(cache.reused_rounds.get(), 2);
    assert_snapshot(&first, 7);
    assert_snapshot(&second, 19);
    collect_cycles();
    assert!(
        weak_root.upgrade().is_none(),
        "removed original root is collectible"
    );
    assert!(
        weak_left.upgrade().is_none(),
        "removed original child is collectible"
    );
    drop(engine);
    collect_cycles();
    assert!(
        weak_env.upgrade().is_some(),
        "returned snapshots retain their universe"
    );
    assert_snapshot(&first, 7);
    assert_snapshot(&second, 19);
    drop(second);
    drop(first);
    drop(cache);
    collect_cycles();
    assert!(weak_env.upgrade().is_none());
}

fn plain_fixture() -> Rc<Engine> {
    let parsed = parse_source(
        "type Child = { x: int }\n\
         type Parent = { left: Child, right: Child }\n\
         export output result: Parent = { left: { x: 7 }, right: { x: 11 } }\n\
         export output table: { [string]: int[] } = { \"a\": [1, 2], \"b\": [3] }\n",
    );
    let pipeline = run_pipeline(&parsed.decls);
    assert!(pipeline.diags.is_empty());
    pipeline.eng
}

fn frozen_value(engine: &Engine, name: &str) -> Option<Value> {
    let roots = engine.frozen_roots.borrow();
    roots
        .as_ref()
        .expect("frozen roots")
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
}

#[test]
fn snapshot_shares_a_plain_root_and_leaves_out_one_that_is_bound_again() {
    // Nothing is dirty: the snapshot holds the plain root itself and a copy of
    // the record root.
    let engine = plain_fixture();
    let cache = RoundCache::default();
    let live = engine.env.root("table").expect("live table");
    let kept = cache.advance(&engine, &HashMap::new()).expect("advance");
    assert_eq!(
        identity(&frozen_value(&kept, "table").expect("frozen table")),
        identity(&live)
    );
    assert!(!Rc::ptr_eq(
        &frozen_root(&kept),
        &record(engine.env.root("result").unwrap())
    ));
    drop(kept);
    drop(live);

    // The plain root is bound again: no answer can name it, so the snapshot
    // leaves it out and the removed value has no owner left.
    let engine = plain_fixture();
    let cache = RoundCache::default();
    let Value::Map(table) = engine.env.root("table").expect("live table") else {
        panic!("map root")
    };
    let weak = Rc::downgrade(&table);
    drop(table);
    engine.track.set(true);
    engine.step("root:table", || engine.record("round:nested".into()));
    let snapshot = cache.advance(&engine, &HashMap::new()).expect("advance");
    assert!(frozen_value(&snapshot, "table").is_none());
    assert!(snapshot.root("table").is_none());
    assert!(engine.env.root("table").is_none());
    assert!(
        weak.upgrade().is_none(),
        "the removed plain root is released"
    );
    assert_eq!(snapshot.frozen_registry.borrow().as_ref().unwrap().len(), 3);
    let root = frozen_root(&snapshot);
    assert!(value_eq(
        &child(&root, "left").borrow().slot("x").unwrap().value,
        &Value::Int(7.into())
    ));
}

#[test]
fn snapshot_keeps_a_rebound_root_that_holds_or_heads_a_record() {
    // A record root that is bound again stays in the snapshot.
    let engine = plain_fixture();
    let cache = RoundCache::default();
    engine.track.set(true);
    engine.step("root:result", || engine.record("round:nested".into()));
    engine.step("root:table", || engine.record("round:nested".into()));
    let snapshot = cache.advance(&engine, &HashMap::new()).expect("advance");
    assert!(frozen_value(&snapshot, "table").is_none());
    assert!(value_eq(
        &child(&frozen_root(&snapshot), "right")
            .borrow()
            .slot("x")
            .unwrap()
            .value,
        &Value::Int(11.into())
    ));

    // A plain root that heads a registered record's path stays as well: an
    // answer may name that record.
    let engine = plain_fixture();
    let cache = RoundCache::default();
    let left = child(&record(engine.env.root("result").unwrap()), "left");
    left.borrow_mut().path = Rc::new(vec![Seg::Name("table".into()), Seg::Key("a".into())]);
    engine.track.set(true);
    engine.step("root:table", || engine.record("round:nested".into()));
    let snapshot = cache.advance(&engine, &HashMap::new()).expect("advance");
    assert!(frozen_value(&snapshot, "table").is_some());
}

#[test]
fn readers_index_lists_each_reader_of_a_dependency_once() {
    let mut readers = Readers::with_capacity(3, 5).expect("index");
    let names = ["r.a", "r.b", "root:r"];
    let ids: Vec<u32> = names.iter().map(|name| readers.reader(name)).collect();
    readers.insert("x", ids[0]);
    readers.insert("y", ids[0]);
    readers.insert("x", ids[1]);
    readers.insert("x", ids[2]);
    readers.insert("round:nested", ids[2]);
    let mut of_x: Vec<&str> = readers.of("x").collect();
    of_x.sort_unstable();
    assert_eq!(of_x, ["r.a", "r.b", "root:r"]);
    assert_eq!(readers.of("y").collect::<Vec<_>>(), ["r.a"]);
    assert_eq!(readers.of("round:nested").collect::<Vec<_>>(), ["root:r"]);
    assert_eq!(readers.of("z").count(), 0);
    assert_eq!(
        Readers::with_capacity(0, 0).expect("empty").of("x").count(),
        0
    );
    // The links index with 32 bits; a larger graph falls back before allocating.
    assert!(Readers::with_capacity(u32::MAX as usize + 1, 0).is_none());
    assert!(Readers::with_capacity(0, u32::MAX as usize).is_none());
}

#[test]
fn advance_keeps_fallbacks_before_mutation_and_does_not_invoke_resets() {
    for case in 0..4 {
        let engine = fixture();
        let cache = RoundCache::default();
        let root = record(engine.env.root("result").unwrap());
        let left = child(&root, "left");
        match case {
            0 => left.borrow_mut().path = root.borrow().path.clone(),
            1 => root.borrow_mut().slot_mut("left").unwrap().state = SlotState::Unforced,
            2 => root.borrow_mut().slot_mut("left").unwrap().value = Value::pattern("a"),
            3 => engine.env.report(Diag::error(
                "existing error",
                "result".into(),
                Some("E5000"),
            )),
            _ => unreachable!(),
        }
        let called = Rc::new(Cell::new(false));
        let observed = called.clone();
        engine
            .round_resets
            .borrow_mut()
            .push(Rc::new(move || observed.set(true)));
        let phase = engine.phase.get();
        let registry = engine.env.registry_snapshot();
        let tagger = engine.env.tagger.borrow().clone().expect("existing tagger");
        assert!(
            cache.advance(&engine, &HashMap::new()).is_none(),
            "case {case}"
        );
        assert_eq!(engine.phase.get(), phase);
        assert!(Rc::ptr_eq(
            &record(engine.env.root("result").unwrap()),
            &root
        ));
        let current = engine.env.registry_snapshot();
        assert_eq!(current.len(), registry.len());
        assert!(current.iter().zip(&registry).all(|(a, b)| Rc::ptr_eq(a, b)));
        assert!(Rc::ptr_eq(
            engine.env.tagger.borrow().as_ref().unwrap(),
            &tagger
        ));
        assert!(engine.prev.borrow().is_none());
        assert_eq!(cache.reused_rounds.get(), 0);
        assert!(!called.get());
    }
}

fn array_value(items: Vec<Value>) -> Value {
    Value::Arr(Rc::new(RefCell::new(ArrV {
        items,
        path: PrefixPath::from_segments(&[Seg::Name("result".into())]),
    })))
}

fn map_value(entries: Vec<(String, Value)>) -> Value {
    Value::Map(Rc::new(RefCell::new(MapV {
        entries: entries.into_iter().collect(),
        path: PrefixPath::from_segments(&[Seg::Name("result".into())]),
    })))
}

fn snapshot_cycle(engine: &Engine, payload: Value) -> Inst {
    let root = record(engine.env.root("result").unwrap());
    let array = array_value(vec![payload]);
    let map = map_value(vec![
        ("root".into(), Value::Rec(root.clone())),
        ("array".into(), array.clone()),
    ]);
    let Value::Arr(items) = &array else {
        unreachable!()
    };
    items.borrow_mut().items.extend([map.clone(), map]);
    root.borrow_mut().slot_mut("left").unwrap().value = array.clone();
    root.borrow_mut().slot_mut("right").unwrap().value = array;
    root
}

#[test]
fn copy_memo_preserves_mixed_cycles_aliases_and_reference_owners() {
    let engine = fixture();
    let cache = RoundCache::default();
    let frozen = Engine::bare_with_queries(engine.env.clone(), engine.query_pool());
    let owner = Engine::bare(Env::new());
    let reference = Rc::new(vec![Seg::Name("result".into())]);
    let ordinary = Rc::new(vec![Seg::Name("ordinary".into())]);
    engine
        .round_refs
        .borrow_mut()
        .insert(Rc::as_ptr(&reference) as usize, reference.clone());
    engine.snap_refs.borrow_mut().insert(
        Rc::as_ptr(&reference) as usize,
        (reference.clone(), owner.clone()),
    );
    let live = snapshot_cycle(&engine, Value::Ref(reference.clone()));
    let Value::Arr(live_array) = live.borrow().slot("left").unwrap().value.clone() else {
        panic!("array")
    };
    live_array
        .borrow_mut()
        .items
        .extend([Value::Ref(reference.clone()), Value::Ref(ordinary.clone())]);
    let Value::Map(live_map) = live_array.borrow().items[1].clone() else {
        panic!("map")
    };
    let mut memo = CopyMemo::default();
    let copied = record(
        cache
            .copy(&Value::Rec(live.clone()), &engine, &frozen, &mut memo)
            .unwrap(),
    );
    let Value::Arr(array) = copied.borrow().slot("left").unwrap().value.clone() else {
        panic!("copied array")
    };
    let Value::Map(map) = array.borrow().items[1].clone() else {
        panic!("copied map")
    };
    let Value::Ref(path) = array.borrow().items[0].clone() else {
        panic!("copied reference")
    };
    assert!(!Rc::ptr_eq(&live, &copied));
    assert!(!Rc::ptr_eq(&live_array, &array));
    assert!(!Rc::ptr_eq(&live_map, &map));
    assert!(!Rc::ptr_eq(&reference, &path));
    for (source, expected) in [
        (Value::Rec(live.clone()), Value::Rec(copied.clone())),
        (Value::Arr(live_array.clone()), Value::Arr(array.clone())),
        (Value::Map(live_map.clone()), Value::Map(map.clone())),
        (Value::Ref(reference), Value::Ref(path.clone())),
    ] {
        let again = cache.copy(&source, &engine, &frozen, &mut memo).unwrap();
        assert_eq!(identity(&again), identity(&expected));
    }
    assert!(matches!(&copied.borrow().slot("right").unwrap().value,
        Value::Arr(alias) if Rc::ptr_eq(alias, &array)));
    assert!(matches!(&array.borrow().items[2],
        Value::Map(alias) if Rc::ptr_eq(alias, &map)));
    assert!(matches!(&array.borrow().items[3],
        Value::Ref(alias) if Rc::ptr_eq(alias, &path)));
    assert!(matches!(&array.borrow().items[4],
        Value::Ref(alias) if Rc::ptr_eq(alias, &ordinary)));
    assert!(matches!(map.borrow().get("root"),
        Some(Value::Rec(back)) if Rc::ptr_eq(back, &copied)));
    assert!(matches!(map.borrow().get("array"),
        Some(Value::Arr(back)) if Rc::ptr_eq(back, &array)));
    assert!(copied
        .borrow()
        .slots
        .iter()
        .all(|(_, slot)| slot.compute.is_none()));
    assert!(Rc::ptr_eq(
        &frozen
            .snap_refs
            .borrow()
            .get(&(Rc::as_ptr(&path) as usize))
            .unwrap()
            .1,
        &owner,
    ));
    assert!(frozen
        .inverse_refs
        .borrow()
        .contains_key(&(Rc::as_ptr(&path) as usize)));
    assert_eq!(
        (
            memo.records.len(),
            memo.arrays.len(),
            memo.maps.len(),
            memo.refs.len()
        ),
        (1, 1, 1, 1)
    );

    let weak_root = Rc::downgrade(&copied);
    let weak_array = Rc::downgrade(&array);
    let weak_map = Rc::downgrade(&map);
    drop(memo);
    live_array.borrow_mut().items[0] = Value::Int(91.into());
    live_map.borrow_mut().entries.swap_remove("array");
    collect_cycles();
    assert!(matches!(&array.borrow().items[0], Value::Ref(alias) if Rc::ptr_eq(alias, &path)));
    assert!(map.borrow().get("array").is_some());
    drop((copied, array, map));
    collect_cycles();
    assert!(weak_root.upgrade().is_none());
    assert!(weak_array.upgrade().is_none());
    assert!(weak_map.upgrade().is_none());
}

#[test]
fn failed_copy_releases_partial_cycles_without_dropping_original_native_payload() {
    struct ObserveDrop {
        engine: std::rc::Weak<Engine>,
        calls: Rc<Cell<usize>>,
    }
    impl Drop for ObserveDrop {
        fn drop(&mut self) {
            self.calls.set(self.calls.get() + 1);
            let engine = self.engine.upgrade().unwrap();
            assert!(engine.reads.try_borrow_mut().is_ok());
            engine.env.set_root("from_copy_drop", Value::Bool(true));
        }
    }

    let engine = fixture();
    let cache = RoundCache::default();
    let frozen = Engine::bare_with_queries(engine.env.clone(), engine.query_pool());
    let observer = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let capture = ObserveDrop {
        engine: Rc::downgrade(&observer),
        calls: calls.clone(),
    };
    let native: NatFn = Rc::new(Box::new(move |_| {
        let _ = &capture;
        panic!("copy must not execute native payloads")
    }));
    // JObj is an admitted opaque payload: its original owner must survive even
    // when copied containers are only partially reachable at a later failure.
    let live = snapshot_cycle(
        &engine,
        Value::JObj(Rc::new(vec![("native".into(), Value::Nat(native))])),
    );
    let input = array_value(vec![Value::Rec(live.clone()), Value::pattern("reject")]);
    let mut memo = CopyMemo::default();
    assert!(cache.copy(&input, &engine, &frozen, &mut memo).is_none());
    let copied = Rc::downgrade(memo.records.get(&address(&live)).unwrap());
    let arrays: Vec<_> = memo.arrays.values().map(Rc::downgrade).collect();
    let maps: Vec<_> = memo.maps.values().map(Rc::downgrade).collect();
    assert_eq!((memo.records.len(), arrays.len(), maps.len()), (1, 2, 1));
    drop(memo);
    collect_cycles();
    assert!(copied.upgrade().is_none());
    assert!(arrays.iter().all(|array| array.upgrade().is_none()));
    assert!(maps.iter().all(|map| map.upgrade().is_none()));
    assert_eq!(calls.get(), 0, "the original graph still owns the payload");
    assert!(Rc::ptr_eq(
        &record(engine.env.root("result").unwrap()),
        &live
    ));
    assert_eq!(live.borrow().slot("left").unwrap().state, SlotState::Ok);
    assert!(engine.prev.borrow().is_none());

    drop(input);
    drop(live);
    drop(frozen);
    drop(engine);
    collect_cycles();
    assert_eq!(calls.get(), 1);
    assert!(matches!(
        observer.env.root("from_copy_drop"),
        Some(Value::Bool(true))
    ));
}

#[test]
fn copy_memo_keeps_foreign_record_bypass_outside_its_owned_copies() {
    let engine = fixture();
    let cache = RoundCache::default();
    let previous = cache.advance(&engine, &HashMap::new()).unwrap();
    let foreign = frozen_root(&previous);
    let frozen = Engine::bare_with_queries(engine.env.clone(), engine.query_pool());
    let mut memo = CopyMemo::default();
    let copied = record(
        cache
            .copy(&Value::Rec(foreign.clone()), &engine, &frozen, &mut memo)
            .unwrap(),
    );
    assert!(Rc::ptr_eq(&copied, &foreign));
    assert!(memo.records.is_empty());
    assert!(memo.arrays.is_empty() && memo.maps.is_empty() && memo.refs.is_empty());
    assert!(value_eq(
        &child(&copied, "left").borrow().slot("x").unwrap().value,
        &Value::Int(7.into()),
    ));
}

#[test]
fn rebase_reuses_unchanged_empty_and_aliased_containers() {
    let engine = fixture();
    let cache = RoundCache::default();
    let _previous = cache.advance(&engine, &HashMap::new()).unwrap();
    let shared = array_value(vec![
        Value::quantity("dimension".repeat(32), 7.5),
        engine.env.root("result").unwrap(),
    ]);
    for input in [
        array_value(Vec::new()),
        map_value(Vec::new()),
        shared.clone(),
        map_value(vec![("z".into(), shared.clone()), ("a".into(), shared)]),
    ] {
        let result = cache.value(&input, &engine);
        assert_eq!(identity(&result), identity(&input));
        assert_eq!(identity(&cache.value(&input, &engine)), identity(&input));
    }
}

#[test]
fn rebase_retains_non_reflexive_scalar_replacement_behavior() {
    let engine = fixture();
    let cache = RoundCache::default();
    let _previous = cache.advance(&engine, &HashMap::new()).unwrap();
    for value in [
        Value::Absent,
        Value::Float(f64::NAN),
        Value::quantity("d", f64::NAN),
        Value::pattern("a".repeat(64)),
        Value::range(Value::Int(7.into()), Value::Int(11.into()), true),
    ] {
        assert!(!identical(&value, &value));
        for input in [
            array_value(vec![value.clone()]),
            map_value(vec![("key".into(), value.clone())]),
        ] {
            let result = cache.value(&input, &engine);
            assert_ne!(identity(&result), identity(&input));
            assert_eq!(identity(&cache.value(&input, &engine)), identity(&result));
        }
    }
}

#[test]
fn rebase_preserves_prefix_suffix_order_and_reference_aliases() {
    let engine = fixture();
    let cache = RoundCache::default();
    let previous = cache.advance(&engine, &HashMap::new()).unwrap();
    for changed in [0, 2, 4] {
        let path = Rc::new(vec![Seg::Name("result".into()), Seg::Name("left".into())]);
        engine
            .round_refs
            .borrow_mut()
            .insert(Rc::as_ptr(&path) as usize, path.clone());
        let mut values: Vec<_> = (0..5)
            .map(|i| Value::quantity(format!("dimension-{i}").repeat(16), i as f64))
            .collect();
        values[changed] = Value::Ref(path.clone());
        let input_array = array_value(values.clone());
        let input_map = map_value(
            values
                .into_iter()
                .enumerate()
                .map(|(i, v)| (format!("key-{}", 5 - i), v))
                .collect(),
        );
        let Value::Arr(array) = cache.value(&input_array, &engine) else {
            panic!("array")
        };
        let Value::Map(map) = cache.value(&input_map, &engine) else {
            panic!("map")
        };
        assert_ne!(identity(&Value::Arr(array.clone())), identity(&input_array));
        assert_ne!(identity(&Value::Map(map.clone())), identity(&input_map));
        assert_eq!(
            map.borrow().entries.keys().cloned().collect::<Vec<_>>(),
            ["key-5", "key-4", "key-3", "key-2", "key-1"]
        );
        for (i, value) in array.borrow().items.iter().enumerate() {
            let map = map.borrow();
            let other = map.entries.get_index(i).unwrap().1;
            if i == changed {
                let (Value::Ref(next), Value::Ref(alias)) = (value, other) else {
                    panic!("reference")
                };
                assert!(!Rc::ptr_eq(next, &path));
                assert!(Rc::ptr_eq(next, alias));
                assert_eq!(**next, *path);
                let owners = engine.snap_refs.borrow();
                assert!(Rc::ptr_eq(
                    &owners.get(&(Rc::as_ptr(next) as usize)).unwrap().1,
                    &previous
                ));
            } else {
                let expected = Value::quantity(format!("dimension-{i}").repeat(16), i as f64);
                assert!(value_eq(value, &expected));
                assert!(value_eq(other, &expected));
            }
        }
    }
}

#[test]
fn frozen_rebased_containers_keep_aliases_old_owners_and_cow_paths() {
    let engine = fixture();
    let cache = RoundCache::default();
    let original_root = record(engine.env.root("result").unwrap());
    let original_left = child(&original_root, "left");
    original_root.borrow_mut().slot_mut("right").unwrap().value = Value::Rec(original_left);
    let first = cache.advance(&engine, &HashMap::new()).unwrap();
    let frozen_order = frozen_root(&first).borrow().entry_order.clone();
    Rc::make_mut(&mut original_root.borrow_mut().entry_order).push("native-extra".into());
    assert_eq!(frozen_root(&first).borrow().entry_order, frozen_order);
    assert_ne!(original_root.borrow().entry_order, frozen_order);
    let reference = Rc::new(vec![Seg::Name("result".into()), Seg::Name("left".into())]);
    engine
        .round_refs
        .borrow_mut()
        .insert(Rc::as_ptr(&reference) as usize, reference.clone());
    let shared = array_value(vec![Value::Int(7.into()), Value::Ref(reference)]);
    engine.env.set_root(
        "result",
        map_value(vec![
            ("left".into(), shared.clone()),
            ("right".into(), shared),
        ]),
    );
    let second = cache.advance(&engine, &HashMap::new()).unwrap();
    let Value::Map(live) = engine.env.root("result").unwrap() else {
        panic!("live map")
    };
    let Value::Map(frozen) = second
        .frozen_roots
        .borrow()
        .as_ref()
        .unwrap()
        .iter()
        .find(|(name, _)| name == "result")
        .unwrap()
        .1
        .clone()
    else {
        panic!("frozen map")
    };
    let get_array = |map: &Rc<RefCell<MapV>>, key: &str| {
        let Value::Arr(array) = map.borrow().get(key).unwrap().clone() else {
            panic!("array")
        };
        array
    };
    let live_array = get_array(&live, "left");
    let frozen_array = get_array(&frozen, "left");
    assert!(Rc::ptr_eq(&live_array, &get_array(&live, "right")));
    assert!(Rc::ptr_eq(&frozen_array, &get_array(&frozen, "right")));
    assert!(!Rc::ptr_eq(&live_array, &frozen_array));
    let reference_of = |array: &Rc<RefCell<ArrV>>| {
        let Value::Ref(path) = array.borrow().items[1].clone() else {
            panic!("reference")
        };
        path
    };
    let live_ref = reference_of(&live_array);
    let frozen_ref = reference_of(&frozen_array);
    assert!(!Rc::ptr_eq(&live_ref, &frozen_ref));
    assert!(Rc::ptr_eq(
        &engine
            .snap_refs
            .borrow()
            .get(&(Rc::as_ptr(&live_ref) as usize))
            .unwrap()
            .1,
        &second
    ));
    assert!(Rc::ptr_eq(
        &second
            .snap_refs
            .borrow()
            .get(&(Rc::as_ptr(&frozen_ref) as usize))
            .unwrap()
            .1,
        &first
    ));
    assert!(std::ptr::eq(
        live_array.borrow().path.last().unwrap(),
        frozen_array.borrow().path.last().unwrap()
    ));
    {
        let mut live = live_array.borrow_mut();
        live.items[0] = Value::Int(99.into());
        live.path.push(Seg::Name("changed".into()));
    }
    live.borrow_mut().entries.swap_remove("right");
    collect_cycles();
    assert!(value_eq(
        &frozen_array.borrow().items[0],
        &Value::Int(7.into())
    ));
    assert_eq!(
        frozen_array.borrow().path.to_vec(),
        [Seg::Name("result".into())]
    );
    assert_eq!(frozen.borrow().entries.len(), 2);
    assert_snapshot(&first, 7);
}

#[test]
fn settled_rebase_release_preserves_live_aliases_and_reference_age_for_replay() {
    let engine = fixture();
    let cache = Rc::new(RoundCache::default());
    *engine.round_cache.borrow_mut() = Some(cache.clone());
    let previous = cache.advance(&engine, &HashMap::new()).unwrap();
    let root = record(engine.env.root("result").unwrap());
    child(&root, "left")
        .borrow_mut()
        .slot_mut("x")
        .unwrap()
        .value = Value::Int(19.into());
    let path = Rc::new(vec![
        Seg::Name("result".into()),
        Seg::Name("left".into()),
        Seg::Name("x".into()),
    ]);
    engine
        .round_refs
        .borrow_mut()
        .insert(Rc::as_ptr(&path) as usize, path.clone());
    let input = array_value(vec![Value::Ref(path.clone()), Value::Ref(path)]);
    let Value::Arr(original) = &input else {
        unreachable!()
    };
    let weak_original = Rc::downgrade(original);
    let Value::Arr(result) = cache.value(&input, &engine) else {
        panic!("array")
    };
    let Value::Ref(reference) = result.borrow().items[0].clone() else {
        panic!("reference")
    };
    let Value::Ref(alias) = result.borrow().items[1].clone() else {
        panic!("reference")
    };
    assert!(Rc::ptr_eq(&reference, &alias));
    assert!(!Rc::ptr_eq(original, &result));
    drop(input);
    assert!(
        weak_original.upgrade().is_some(),
        "the epoch memo owns the source"
    );
    assert!(!cache.rebased.borrow().is_empty());
    let clean_count = cache.clean.borrow().len();
    let owners = (
        engine.snap_refs.borrow().len(),
        engine.inverse_refs.borrow().len(),
        engine.round_refs.borrow().len(),
    );
    // This is the completion boundary used by Engine::evaluate_rounds.
    engine.settled.set(true);
    *engine.prev.borrow_mut() = None;
    cache.release_rebased();
    assert_eq!(cache.rebased.borrow().capacity(), 0);
    assert!(weak_original.upgrade().is_none());
    assert_eq!(cache.clean.borrow().len(), clean_count);
    assert_eq!(
        owners,
        (
            engine.snap_refs.borrow().len(),
            engine.inverse_refs.borrow().len(),
            engine.round_refs.borrow().len(),
        )
    );
    assert!(value_eq(
        &engine.deref(Value::Ref(reference.clone())).ok().unwrap(),
        &Value::Int(19.into())
    ));
    assert!(Rc::ptr_eq(
        &result,
        &match cache.value(&Value::Arr(result.clone()), &engine) {
            Value::Arr(value) => value,
            _ => panic!("array"),
        },
    ));
    assert!(
        cache.rebased.borrow().is_empty(),
        "settled reads bypass the memo"
    );
    // Edits::begin can make the engine unsettled before replay has rebound
    // every root. Old references must still resolve through their old owner.
    engine.settled.set(false);
    assert!(value_eq(
        &engine.deref(Value::Ref(reference.clone())).ok().unwrap(),
        &Value::Int(7.into())
    ));
    assert!(Rc::ptr_eq(
        &engine
            .snap_refs
            .borrow()
            .get(&(Rc::as_ptr(&reference) as usize))
            .unwrap()
            .1,
        &previous,
    ));
    collect_cycles();
    assert!(value_eq(
        &engine.deref(Value::Ref(alias)).ok().unwrap(),
        &Value::Int(7.into())
    ));
}

#[test]
fn settled_rebase_release_drops_native_captures_without_a_cache_borrow() {
    struct Reenter {
        engine: std::rc::Weak<Engine>,
        cache: std::rc::Weak<RoundCache>,
        calls: Rc<Cell<usize>>,
    }
    impl Drop for Reenter {
        fn drop(&mut self) {
            let engine = self.engine.upgrade().unwrap();
            let cache = self.cache.upgrade().unwrap();
            assert!(engine.settled.get());
            assert!(engine.prev.borrow().is_none());
            {
                let memo = cache
                    .rebased
                    .try_borrow_mut()
                    .expect("memo borrow ended before drop");
                assert_eq!(memo.capacity(), 0);
            }
            // A native capture can execute another evaluator and collect while
            // the completed evaluator and its returned values remain alive.
            let nested = fixture();
            let root = record(nested.env.root("result").unwrap());
            assert!(value_eq(
                &child(&root, "left").borrow().slot("x").unwrap().value,
                &Value::Int(7.into())
            ));
            drop((root, nested));
            collect_cycles();
            let current = record(engine.env.root("result").unwrap());
            assert!(value_eq(
                &engine
                    .force_slot(&child(&current, "left"), "x")
                    .ok()
                    .unwrap(),
                &Value::Int(7.into())
            ));
            assert!(cache.rebased.try_borrow_mut().is_ok());
            self.calls.set(self.calls.get() + 1);
        }
    }
    let engine = fixture();
    let cache = Rc::new(RoundCache::default());
    *engine.round_cache.borrow_mut() = Some(cache.clone());
    let previous = cache.advance(&engine, &HashMap::new()).unwrap();
    let calls = Rc::new(Cell::new(0));
    let capture = Reenter {
        engine: Rc::downgrade(&engine),
        cache: Rc::downgrade(&cache),
        calls: calls.clone(),
    };
    let input = array_value(vec![Value::native(move |_| {
        let _keep_capture = &capture;
        Ok(Value::Null)
    })]);
    let result = cache.value(&input, &engine);
    drop((input, result));
    assert_eq!(calls.get(), 0);
    engine.settled.set(true);
    *engine.prev.borrow_mut() = None;
    drop(previous);
    cache.release_rebased();
    assert_eq!(calls.get(), 1);
    cache.release_rebased();
    assert_eq!(calls.get(), 1, "release is idempotent");
}

#[cfg(feature = "runtime-diagnostics")]
mod advance_diagnostics {
    use super::*;
    use crate::evaluation_diagnostics::{take_evaluation_diagnostics, Span};

    fn assert_partition(span: &Span, names: &[&str]) {
        assert_eq!(
            span.stages.iter().map(|s| s.name).collect::<Vec<_>>(),
            names
        );
        let mut previous = &span.start;
        for stage in &span.stages {
            // Exact adjacent snapshots, including allocation and constructor
            // fields; no assumption of process-wide allocator quiescence.
            assert_eq!(
                serde_json::to_value(stage.start).unwrap(),
                serde_json::to_value(previous).unwrap()
            );
            assert_eq!(stage.wall_ns, stage.end.wall_ns - stage.start.wall_ns);
            assert_eq!(stage.cpu_ns, stage.end.cpu_ns - stage.start.cpu_ns);
            assert!(stage.end.wall_ns >= stage.start.wall_ns);
            previous = &stage.end;
        }
        assert_eq!(
            span.stages.iter().map(|s| s.wall_ns).sum::<u64>(),
            previous.wall_ns - span.start.wall_ns
        );
        assert_eq!(
            span.stages.iter().map(|s| s.cpu_ns).sum::<u64>(),
            previous.cpu_ns - span.start.cpu_ns
        );
    }

    struct RetiredCompute(Rc<Cell<usize>>);
    impl Drop for RetiredCompute {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
            let mut span = Span::new("retired_compute_test", 0);
            span.mark("native_drop");
            span.finish();
        }
    }

    #[test]
    fn advance_partition_includes_final_local_owner_destruction() {
        let parsed = parse_source("export output result: { x: int } = { x: 7 }\n");
        let pipeline = run_pipeline(&parsed.decls);
        assert!(pipeline.diags.is_empty());
        let engine = pipeline.eng.clone();
        drop(pipeline);
        let cache = RoundCache::default();
        let root = record(engine.env.root("result").unwrap());
        let weak_root = Rc::downgrade(&root);
        let weak_engine = Rc::downgrade(&engine);
        let weak_env = Rc::downgrade(&engine.env);
        let drops = Rc::new(Cell::new(0));
        let capture = RetiredCompute(drops.clone());
        root.borrow_mut()
            .slot_mut("x")
            .unwrap()
            .set_computation(Some(Compute::Bridge(Rc::new(move || {
                let _ = &capture;
                panic!("advance must not execute the producer")
            }))));
        drop(root);
        engine.track.set(true);
        engine.step("root:result", || engine.record("round:nested".into()));
        drop(take_evaluation_diagnostics());

        let snapshot = cache.advance(&engine, &HashMap::new()).unwrap();
        assert_eq!(drops.get(), 1);
        assert!(weak_root.upgrade().is_none());
        let spans = take_evaluation_diagnostics();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].kind, "retired_compute_test");
        assert_eq!(spans[1].kind, "advance");
        assert_eq!(spans[1].ordinal, 1);
        assert_partition(&spans[1], &["classify", "freeze", "revision_reset"]);
        let final_stage = &spans[1].stages[2];
        assert!(spans[0].start.wall_ns >= final_stage.start.wall_ns);
        assert!(spans[0].stages[0].end.wall_ns <= final_stage.end.wall_ns);
        let copied = frozen_root(&snapshot);
        assert!(value_eq(
            &copied.borrow().slot("x").unwrap().value,
            &Value::Int(7.into())
        ));
        drop(copied);
        drop(snapshot);
        drop(cache);
        drop(engine);
        collect_cycles();
        // Keep the diagnostic reports alive: they must retain no runtime owner.
        assert!(weak_engine.upgrade().is_none());
        assert!(weak_env.upgrade().is_none());
        assert_eq!(spans[1].kind, "advance");
    }

    #[test]
    fn advance_partition_preserves_classification_and_freeze_rejections() {
        for freeze_rejected in [false, true] {
            let engine = fixture();
            let cache = RoundCache::default();
            let root = record(engine.env.root("result").unwrap());
            if freeze_rejected {
                root.borrow_mut().slot_mut("left").unwrap().value = Value::pattern("a");
            } else {
                root.borrow_mut().slot_mut("left").unwrap().state = SlotState::Unforced;
            }
            let called = Rc::new(Cell::new(false));
            let observed = called.clone();
            engine
                .round_resets
                .borrow_mut()
                .push(Rc::new(move || observed.set(true)));
            drop(take_evaluation_diagnostics());

            assert!(cache.advance(&engine, &HashMap::new()).is_none());
            let spans = take_evaluation_diagnostics();
            assert_eq!(spans.len(), 1);
            assert_eq!(spans[0].kind, "advance_rejected");
            let names: &[&str] = if freeze_rejected {
                &["classify", "freeze"]
            } else {
                &["classify"]
            };
            assert_partition(&spans[0], names);
            assert!(!called.get());
            assert_eq!(cache.reused_rounds.get(), 0);
            assert!(engine.prev.borrow().is_none());
            assert!(Rc::ptr_eq(
                &record(engine.env.root("result").unwrap()),
                &root
            ));
        }
    }

    #[test]
    fn advance_partition_keeps_reentrant_reset_spans_separate() {
        let outer = fixture();
        let inner = fixture();
        let outer_cache = RoundCache::default();
        let inner_cache = Rc::new(RoundCache::default());
        let weak_inner = Rc::downgrade(&inner);
        let weak_cache = Rc::downgrade(&inner_cache);
        outer.round_resets.borrow_mut().push(Rc::new(move || {
            let inner = weak_inner.upgrade().unwrap();
            let cache = weak_cache.upgrade().unwrap();
            let _snapshot = cache.advance(&inner, &HashMap::new()).unwrap();
        }));
        drop(take_evaluation_diagnostics());

        let _snapshot = outer_cache.advance(&outer, &HashMap::new()).unwrap();
        let spans = take_evaluation_diagnostics();
        assert_eq!(spans.len(), 2);
        for span in &spans {
            assert_eq!(span.kind, "advance");
            assert_eq!(span.ordinal, 1, "ordinals are not unique attempt IDs");
            assert_partition(span, &["classify", "freeze", "revision_reset"]);
        }
        let outer_reset = &spans[1].stages[2];
        assert!(spans[0].start.wall_ns >= outer_reset.start.wall_ns);
        assert!(spans[0].stages[2].end.wall_ns <= outer_reset.end.wall_ns);
    }

    #[test]
    fn advance_partition_labels_reset_unwind_without_claiming_reuse() {
        let engine = fixture();
        let cache = RoundCache::default();
        engine.round_resets.borrow_mut().push(Rc::new(|| {
            panic!("native reset unwind witness");
        }));
        drop(take_evaluation_diagnostics());

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.advance(&engine, &HashMap::new())
        }));
        assert!(result.is_err());
        let spans = take_evaluation_diagnostics();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, "advance_unwound");
        assert_partition(&spans[0], &["classify", "freeze", "revision_reset"]);
        assert_eq!(cache.reused_rounds.get(), 0);
    }
}
