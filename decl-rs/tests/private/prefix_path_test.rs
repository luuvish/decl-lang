use super::*;
use crate::semantics::{cmp_path, value_eq, ArrV, MapV, Value};
use std::cell::RefCell;

fn name(s: &str) -> Seg {
    Seg::Name(s.into())
}

#[test]
fn shared_prefixes_preserve_segments_and_outer_identity() {
    let mut pool = PrefixPathPool::default();
    let left = pool.retain(&[name("root"), name("items"), Seg::Idx(1)]);
    let right = pool.retain(&[name("root"), name("items"), Seg::Idx(2)]);
    let same = pool.retain(&[name("root"), name("items"), Seg::Idx(1)]);
    assert_eq!(left, same);
    assert!(!Rc::ptr_eq(&left, &same));
    let left_ids: Vec<_> = left.prefix_node_ids().collect();
    let right_ids: Vec<_> = right.prefix_node_ids().collect();
    assert_ne!(left_ids[0], right_ids[0]);
    assert_eq!(left_ids[1..], right_ids[1..]);
    assert_eq!(left_ids, same.prefix_node_ids().collect::<Vec<_>>());
    assert_eq!(
        left.to_vec(),
        vec![name("root"), name("items"), Seg::Idx(1)]
    );
    assert_eq!(left.format(Some("root")), "$.items[1]");
    let key = pool.retain(&[name("root"), Seg::Key("items".into()), Seg::Idx(1)]);
    assert_ne!(
        left, key,
        "Name and Key remain different canonical segments"
    );
}

#[test]
fn borrowed_iteration_and_copy_on_write_do_not_cache_flat_vectors() {
    let mut pool = PrefixPathPool::default();
    let mut path = pool.retain(&[name("root"), Seg::Idx(1)]);
    let snapshot = path.clone();
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();
    assert_eq!(
        path.iter().cloned().collect::<Vec<_>>(),
        vec![name("root"), Seg::Idx(1)]
    );
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(prefix_path_diagnostics().flat_exports, before.flat_exports);
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(
        prefix_path_diagnostics().iterator_spills,
        before.iterator_spills
    );
    Rc::make_mut(&mut path).push(Seg::Key("x".into()));
    assert_eq!(snapshot.len(), 2);
    assert_eq!(path.len(), 3);
    assert_eq!(Rc::make_mut(&mut path).pop(), Some(Seg::Key("x".into())));
    assert_eq!(path, snapshot);
    let mut flat = snapshot.to_vec();
    flat.clear();
    Rc::make_mut(&mut path).clear();
    assert!(path.is_empty());
    assert_eq!(snapshot.first(), Some(&name("root")));
    assert_eq!(snapshot.last(), Some(&Seg::Idx(1)));
    assert_eq!(snapshot.get(2), None);
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(
        prefix_path_diagnostics().flat_exports,
        before.flat_exports + 1
    );
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(
        prefix_path_diagnostics().flat_export_segments,
        before.flat_export_segments + 2
    );
}

#[test]
fn deep_iteration_is_ordered_and_deep_drop_is_iterative() {
    let path = PrefixPath::from((0..20_000).map(Seg::Idx).collect::<Vec<_>>());
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();
    let mut iter = path.iter();
    assert_eq!(iter.next(), Some(&Seg::Idx(0)));
    assert_eq!(iter.next_back(), Some(&Seg::Idx(19_999)));
    assert_eq!(iter.len(), 19_998);
    for i in 1..19_999 {
        assert_eq!(iter.next(), Some(&Seg::Idx(i)));
    }
    assert!(iter.next().is_none());
    assert!(iter.next_back().is_none());
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(
        prefix_path_diagnostics().iterator_spills,
        before.iterator_spills + 1
    );
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(prefix_path_diagnostics().flat_exports, before.flat_exports);
    drop(iter);
    drop(path);
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(
        prefix_path_diagnostics().nodes_live + 20_000,
        before.nodes_live
    );
}

#[test]
fn weak_pool_is_bounded_and_surviving_paths_outlive_eviction() {
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();
    let mut pool = PrefixPathPool::default();
    let survivor = pool.retain(&[Seg::Idx(0)]);
    for i in 1..=POOL_ENTRY_LIMIT {
        drop(pool.retain(&[Seg::Idx(i)]));
    }
    assert!(pool.stats().entries <= POOL_ENTRY_LIMIT);
    #[cfg(feature = "runtime-diagnostics")]
    assert!(prefix_path_diagnostics().pool_clears > before.pool_clears);
    assert_eq!(survivor.first(), Some(&Seg::Idx(0)));
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(prefix_path_diagnostics().nodes_live, before.nodes_live + 1);
    drop(pool);
    assert_eq!(survivor.last(), Some(&Seg::Idx(0)));
    drop(survivor);
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(prefix_path_diagnostics().nodes_live, before.nodes_live);
    #[cfg(feature = "runtime-diagnostics")]
    assert_eq!(prefix_path_diagnostics().handles_live, before.handles_live);
}

#[cfg(not(feature = "runtime-diagnostics"))]
#[test]
fn feature_empty_runtime_does_not_record_path_traffic() {
    let mut pool = PrefixPathPool::default();
    let path = pool.retain(&[name("root"), Seg::Idx(1)]);
    assert_eq!(path.to_vec(), vec![name("root"), Seg::Idx(1)]);
    drop(path);
    let counts = prefix_path_diagnostics();
    assert_eq!(counts.nodes_created, 0);
    assert_eq!(counts.handles_created, 0);
    assert_eq!(counts.flat_exports, 0);
    assert_eq!(counts.pool_misses, 0);
    assert_eq!(counts.retain_calls, 0);
    assert_eq!(counts.input_segments, 0);
    assert_eq!(counts.cursor_attempts, 0);
    assert_eq!(counts.cursor_hits, 0);
    assert_eq!(counts.reused_prefix_segments, 0);
    assert_eq!(counts.cursor_deep_fallbacks, 0);
}

#[test]
fn cursor_reuses_siblings_and_shorter_or_longer_prefixes() {
    let mut pool = PrefixPathPool::default();
    let first = pool.retain(&[name("root"), name("items"), Seg::Idx(1)]);
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();
    let sibling = pool.retain(&[name("root"), name("items"), Seg::Idx(2)]);
    let first_ids: Vec<_> = first.prefix_node_ids().collect();
    let sibling_ids: Vec<_> = sibling.prefix_node_ids().collect();
    assert_eq!(&first_ids[1..], &sibling_ids[1..]);
    assert_ne!(first_ids[0], sibling_ids[0]);
    let shorter = pool.retain(&[name("root")]);
    assert_eq!(shorter.prefix_node_ids().next(), first_ids.last().copied());
    let longer = pool.retain(&[name("root"), name("other"), Seg::Idx(3)]);
    assert_eq!(longer.prefix_node_ids().last(), first_ids.last().copied());
    #[cfg(feature = "runtime-diagnostics")]
    {
        let after = prefix_path_diagnostics();
        assert_eq!(after.retain_calls - before.retain_calls, 3);
        assert_eq!(after.input_segments - before.input_segments, 7);
        assert_eq!(after.cursor_attempts - before.cursor_attempts, 3);
        assert_eq!(after.cursor_hits - before.cursor_hits, 3);
        assert_eq!(
            after.reused_prefix_segments - before.reused_prefix_segments,
            4
        );
        assert_eq!(after.pool_hits - before.pool_hits, 0);
        assert_eq!(after.pool_misses - before.pool_misses, 3);
    }
}

#[test]
fn cursor_matches_canonical_segments_not_root_spelling_alone() {
    let mut pool = PrefixPathPool::default();
    let original = pool.retain(&[name("root"), name("field")]);
    let other_root = pool.retain(&[name("other"), name("field")]);
    assert_ne!(
        original.prefix_node_ids().last(),
        other_root.prefix_node_ids().last()
    );
    let original_again = pool.retain(&[name("root"), name("field")]);
    assert_eq!(original, original_again);
    let map_key = pool.retain(&[name("root"), Seg::Key("field".into())]);
    assert_eq!(
        original.prefix_node_ids().last(),
        map_key.prefix_node_ids().last()
    );
    assert_ne!(original, map_key);
    assert_eq!(
        map_key.to_vec(),
        vec![name("root"), Seg::Key("field".into())]
    );
}

#[test]
fn expired_cursor_falls_back_with_live_ancestor_and_keeps_outer_unique() {
    let mut pool = PrefixPathPool::default();
    let ancestor = pool.retain(&[name("root")]);
    let child = pool.retain(&[name("root"), Seg::Idx(1)]);
    drop(child);
    assert!(pool.cursor_tail.upgrade().is_none());
    assert_eq!(pool.stats().cursor_weak_entries, 1);
    assert_eq!(pool.stats().cursor_dead_entries, 1);
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();
    let mut next = pool.retain(&[name("root"), Seg::Idx(2)]);
    assert_eq!(
        next.prefix_node_ids().last(),
        ancestor.prefix_node_ids().next()
    );
    assert!(
        Rc::get_mut(&mut next).is_some(),
        "cursor must not weakly own the outer Rc"
    );
    #[cfg(feature = "runtime-diagnostics")]
    {
        let after = prefix_path_diagnostics();
        assert_eq!(after.cursor_attempts - before.cursor_attempts, 1);
        assert_eq!(after.cursor_hits - before.cursor_hits, 0);
        assert_eq!(
            after.reused_prefix_segments - before.reused_prefix_segments,
            0
        );
        assert_eq!(after.pool_hits - before.pool_hits, 1);
    }
    Rc::get_mut(&mut next).unwrap().push(name("changed"));
    let mut same_input = pool.retain(&[name("root"), Seg::Idx(2)]);
    assert_eq!(same_input.to_vec(), vec![name("root"), Seg::Idx(2)]);
    assert_eq!(next.len(), 3);
    assert!(Rc::get_mut(&mut same_input).is_some());
    assert!(!Rc::ptr_eq(&next, &same_input));
}

#[test]
fn cursor_survives_pool_clear_during_suffix_insertion() {
    let mut pool = PrefixPathPool::default();
    let prefix = pool.retain(&[name("root"), name("items")]);
    for i in 0..POOL_ENTRY_LIMIT - 5 {
        drop(pool.retain(&[Seg::Idx(i)]));
    }
    assert_eq!(pool.stats().entries, POOL_ENTRY_LIMIT - 3);
    let prefix_again = pool.retain(&[name("root"), name("items")]);
    let first = pool.retain(&[
        name("root"),
        name("items"),
        Seg::Idx(0),
        Seg::Idx(1),
        Seg::Idx(2),
        Seg::Idx(3),
    ]);
    assert_eq!(
        pool.stats().entries,
        1,
        "fourth suffix miss clears the full pool"
    );
    let sibling = pool.retain(&[
        name("root"),
        name("items"),
        Seg::Idx(0),
        Seg::Idx(1),
        Seg::Idx(2),
        Seg::Idx(4),
    ]);
    assert_eq!(pool.stats().entries, 2);
    assert_eq!(
        first.prefix_node_ids().skip(1).collect::<Vec<_>>(),
        sibling.prefix_node_ids().skip(1).collect::<Vec<_>>()
    );
    assert_eq!(prefix, prefix_again);
    drop(pool);
    assert_eq!(sibling.last(), Some(&Seg::Idx(4)));
}

#[test]
fn cursor_empty_and_deep_inputs_clear_the_weak_accelerator() {
    let mut pool = PrefixPathPool::default();
    let short = pool.retain(&[name("root")]);
    let deep_segments: Vec<_> = (0..INLINE_ITER_DEPTH + 1).map(Seg::Idx).collect();
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();
    let deep = pool.retain(&deep_segments);
    assert_eq!(deep.to_vec(), deep_segments);
    assert_eq!(pool.stats().cursor_depth, 0);
    assert_eq!(pool.stats().cursor_weak_entries, 0);
    let mut empty = pool.retain(&[]);
    assert!(empty.is_empty());
    assert!(Rc::get_mut(&mut empty).is_some());
    assert_eq!(pool.stats().cursor_depth, 0);
    #[cfg(feature = "runtime-diagnostics")]
    {
        let after = prefix_path_diagnostics();
        assert_eq!(
            after.cursor_deep_fallbacks - before.cursor_deep_fallbacks,
            1
        );
        assert_eq!(after.cursor_attempts - before.cursor_attempts, 0);
    }
    assert_eq!(short.first(), Some(&name("root")));
}

#[test]
fn cursor_reuses_the_full_inline_depth_boundary() {
    let mut pool = PrefixPathPool::default();
    let segments: Vec<_> = (0..INLINE_ITER_DEPTH).map(Seg::Idx).collect();
    let first = pool.retain(&segments);
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();
    let mut second = pool.retain(&segments);
    assert_eq!(first, second);
    assert_eq!(
        first.prefix_node_ids().collect::<Vec<_>>(),
        second.prefix_node_ids().collect::<Vec<_>>()
    );
    assert!(Rc::get_mut(&mut second).is_some());
    assert_eq!(pool.stats().cursor_depth, INLINE_ITER_DEPTH);
    #[cfg(feature = "runtime-diagnostics")]
    {
        let after = prefix_path_diagnostics();
        assert_eq!(
            after.reused_prefix_segments - before.reused_prefix_segments,
            INLINE_ITER_DEPTH as u64
        );
        assert_eq!(after.pool_hits, before.pool_hits);
        assert_eq!(after.pool_misses, before.pool_misses);
    }
}

fn equality_array(path: Vec<Seg>, items: Vec<Value>) -> Value {
    Value::Arr(Rc::new(RefCell::new(ArrV {
        path: Rc::new(PrefixPath::from(path)),
        items,
    })))
}

fn equality_map(path: Vec<Seg>, value: Value) -> Value {
    Value::Map(Rc::new(RefCell::new(MapV {
        path: Rc::new(PrefixPath::from(path)),
        entries: [("payload".into(), value)].into_iter().collect(),
    })))
}

#[test]
fn structural_equality_does_not_export_container_paths() {
    let nested = |root: &str, scalar| {
        equality_array(
            vec![name(root)],
            vec![equality_map(
                vec![name(root), Seg::Idx(0)],
                equality_array(
                    vec![name(root), Seg::Idx(0), Seg::Key("payload".into())],
                    vec![Value::Bool(scalar), Value::Null],
                ),
            )],
        )
    };
    let left = nested("left", true);
    let same = nested("different", true);
    let unequal = nested("left", false);
    let empty = equality_array(vec![name("empty")], vec![]);
    let map = equality_map(vec![name("map")], Value::Null);
    let null = Value::Null;
    let scalar = Value::Bool(true);
    let left_path = match &left {
        Value::Arr(array) => Rc::downgrade(&array.borrow().path),
        _ => unreachable!(),
    };
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();

    for (a, b, expected) in [
        (&left, &same, true),
        (&left, &unequal, false),
        (&left, &empty, false),
        (&left, &map, false),
        (&left, &null, false),
        (&map, &null, false),
        (&map, &scalar, false),
    ] {
        assert_eq!(value_eq(a, b), expected);
        assert_eq!(value_eq(b, a), expected);
    }

    #[cfg(feature = "runtime-diagnostics")]
    {
        let after = prefix_path_diagnostics();
        assert_eq!(after.flat_exports, before.flat_exports);
        assert_eq!(after.flat_export_segments, before.flat_export_segments);
    }
    assert_eq!(
        left_path.strong_count(),
        1,
        "comparison retains no path owner"
    );
    drop(left);
    assert!(left_path.upgrade().is_none());
}

#[test]
fn reference_equality_preserves_path_comparison_and_explicit_exports() {
    let container_path = vec![name("root"), name("items"), Seg::Idx(2)];
    let reference_path = vec![name("root"), Seg::Key("items".into()), Seg::Idx(2)];
    assert_ne!(
        PrefixPath::from(container_path.clone()),
        PrefixPath::from(reference_path.clone()),
        "retained paths distinguish Name and Key"
    );
    assert_eq!(
        cmp_path(&container_path, &reference_path),
        std::cmp::Ordering::Equal,
        "reference equality follows canonical path comparison"
    );
    let larger_index = vec![name("root"), name("items"), Seg::Idx(10)];
    assert_eq!(
        cmp_path(&container_path, &larger_index),
        std::cmp::Ordering::Less,
        "indices compare numerically"
    );
    let array = equality_array(container_path.clone(), vec![Value::Bool(true)]);
    let map = equality_map(container_path.clone(), Value::Null);
    let matching = Value::Ref(Rc::new(reference_path));
    let different_index = Value::Ref(Rc::new(larger_index));
    let prefix = Value::Ref(Rc::new(vec![name("root"), name("items")]));
    let different_root = Value::Ref(Rc::new(vec![name("other"), name("items"), Seg::Idx(2)]));
    let first = Rc::new(container_path.clone());
    let second = Rc::new(container_path);
    assert!(!Rc::ptr_eq(&first, &second));
    #[cfg(feature = "runtime-diagnostics")]
    let before = prefix_path_diagnostics();

    for container in [&array, &map] {
        for (reference, expected) in [
            (&matching, true),
            (&different_index, false),
            (&prefix, false),
            (&different_root, false),
        ] {
            assert_eq!(value_eq(reference, container), expected);
            assert_eq!(value_eq(container, reference), expected);
        }
    }
    assert!(value_eq(&Value::Ref(first), &Value::Ref(second)));

    #[cfg(feature = "runtime-diagnostics")]
    {
        let after = prefix_path_diagnostics();
        assert_eq!(after.flat_exports - before.flat_exports, 16);
        assert_eq!(after.flat_export_segments - before.flat_export_segments, 48);
    }
}
