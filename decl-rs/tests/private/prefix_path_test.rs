use super::*;

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
}
