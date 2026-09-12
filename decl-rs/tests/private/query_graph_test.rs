//! Rust-only representation and lifetime checks, included by qengine::graph.
use super::*;
use rustc_hash::FxHashMap;
use std::collections::{BTreeSet, HashSet};
use std::hash::DefaultHasher;

fn hash(key: &QueryId) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

fn names(deps: &ReadSet) -> BTreeSet<&str> {
    deps.iter().map(QueryId::as_str).collect()
}

#[test]
fn scratch_subset_excludes_ordinary_keys_and_preserves_exact_boundaries() {
    let pool = QueryPool::default();
    let mut held: Vec<_> = (0..4096)
        .map(|index| pool.intern(&format!("ordinary.rows[{index}].value")))
        .collect();
    let selected = [
        "_",
        "_.x",
        "_[0]",
        "_[\"한글\"]",
        "assert:_",
        "assert:_.x",
        "assert:_[0]",
    ];
    let excluded = [
        "",
        "_other",
        "_é",
        "assert:_other",
        "root:_",
        "assert:assert:_",
        "snapshot:_.x",
    ];
    held.extend(
        selected
            .iter()
            .chain(excluded.iter())
            .map(|text| pool.intern(text)),
    );
    let before = pool.census();
    let candidates = pool.scratch_ids();
    assert_eq!(
        candidates
            .iter()
            .map(QueryId::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(selected)
    );
    assert_eq!(candidates.len(), selected.len());
    drop(candidates);
    let after = pool.census();
    assert_eq!(after.live, before.live);
    assert_eq!(after.dead, before.dead);
    assert_eq!(after.scratch_index_entries, selected.len());
    assert_eq!(after.scratch_index_entry_bytes, size_of::<Weak<QueryKey>>());
    assert_eq!(after.query_id_bytes, size_of::<Rc<QueryKey>>());
    drop(held);
    pool.sweep();
    assert!(pool.scratch_ids().is_empty());
    assert!(pool.census().scratch_index_capacity <= 64);
}

#[test]
fn scratch_revival_at_maintenance_threshold_keeps_one_weak_entry() {
    let pool = QueryPool::default();
    let held: Vec<_> = (0..800)
        .map(|index| pool.intern(&format!("stable[{index}]")))
        .collect();
    let temporary: Vec<_> = (0..224)
        .map(|index| pool.intern(&format!("_.temporary[{index}]")))
        .collect();
    let original = Rc::as_ptr(&temporary[0].0);
    drop(temporary);
    let revived = pool.intern("_.temporary[0]");
    assert_eq!(Rc::as_ptr(&revived.0), original);
    assert_eq!(pool.next_sweep.get(), 1600);
    assert_eq!(pool.census().scratch_index_entries, 1);
    drop(revived);
    for _ in 0..64 {
        drop(pool.intern("_.temporary[0]"));
        assert_eq!(pool.census().scratch_index_entries, 1);
    }
    assert_eq!(pool.census().live, held.len());
    pool.maintain_transient();
    assert!(pool.scratch_ids().is_empty());
    assert_eq!(pool.census().live, held.len());
}

#[test]
fn scratch_weak_storage_is_bounded_under_churn_and_retained_owners() {
    let pool = QueryPool::default();
    let retained = pool.intern("_.old_run.value");
    for block in 0..8 {
        for index in block * 1024..(block + 1) * 1024 {
            drop(pool.intern(&format!("_.temporary[{index}]")));
        }
        let stats = pool.census();
        assert_eq!(stats.live, 1);
        assert!(stats.entries <= 1024);
        assert_eq!(stats.scratch_index_entries, stats.entries);
        assert!(pool.scratch_ids().iter().any(|key| key == &retained));
    }
    pool.sweep();
    assert_eq!(pool.scratch_ids(), vec![retained.clone()]);
    assert_eq!(pool.census().scratch_index_entries, 1);
    drop(retained);
    pool.sweep();
    let stats = pool.census();
    assert_eq!(stats.live, 0);
    assert_eq!(stats.scratch_index_entries, 0);
    assert!(stats.scratch_index_capacity <= 64);
}

#[test]
fn cross_pool_keys_preserve_text_hashing_and_lexical_order() {
    let first = QueryPool::default();
    let second = QueryPool::default();
    let long = "input[\"한글\"].e\u{301}.value".repeat(1024);
    let text = [
        "",
        "\0",
        "root:input",
        "input.rows[11].value",
        "input.rows[2].value",
        "snapshot:input.rows[2].value",
        "input[\"한글\"].é",
        "input[\"한글\"].e\u{301}",
        "input.\0tail",
        "round:nested",
        long.as_str(),
    ];
    let mut map = FxHashMap::default();
    let mut set = HashSet::new();
    let mut ordered = BTreeSet::new();
    for (index, text) in text.iter().enumerate() {
        let a = first.intern(text);
        let same = first.intern(text);
        let b = second.intern(text);
        assert!(Rc::ptr_eq(&a.0, &same.0));
        assert!(!Rc::ptr_eq(&a.0, &b.0));
        assert_eq!(a, same);
        assert_eq!(a, b);
        assert_eq!(hash(&a), hash(&b));
        assert_eq!(a.cmp(&b), Ordering::Equal);
        assert_eq!(a.to_string(), *text);
        assert_eq!(a.as_ref(), *text);
        map.insert(a.clone(), index);
        assert_eq!(map.get(&b), Some(&index));
        set.insert(a.clone());
        assert!(!set.insert(b.clone()));
        ordered.insert(a);
        ordered.insert(b);
    }
    assert_eq!(set.len(), text.len());
    assert_eq!(first.census().entries, text.len());
    assert_eq!(second.census().entries, text.len());
    let mut expected = text.to_vec();
    expected.sort();
    assert_eq!(
        ordered.iter().map(QueryId::as_str).collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn an_external_id_survives_its_pool() {
    let id = {
        let pool = QueryPool::default();
        let id = pool.intern("input[\"한글\"].\0value");
        assert_eq!(Rc::strong_count(&id.0), 2);
        id
    };
    assert_eq!(Rc::strong_count(&id.0), 1);
    assert_eq!(id.as_str(), "input[\"한글\"].\0value");
    let another = QueryPool::default();
    let equivalent = another.intern(id.as_str());
    assert_eq!(id, equivalent);
    assert_eq!(hash(&id), hash(&equivalent));
    let snapshot: ReadSet = [id].into_iter().collect();
    assert_eq!(names(&snapshot), BTreeSet::from([equivalent.as_str()]));
}

#[test]
fn census_and_lookup_do_not_revive_pool_only_entries() {
    let pool = QueryPool::default();
    let id = pool.intern("root:α");
    for _ in 0..4 {
        let before = Rc::strong_count(&id.0);
        let stats = pool.census();
        assert_eq!((stats.entries, stats.live, stats.dead), (1, 1, 0));
        assert_eq!(Rc::strong_count(&id.0), before);
        assert_eq!(pool.lookup(id.as_str()).as_ref(), Some(&id));
        assert_eq!(Rc::strong_count(&id.0), before);
        assert_eq!(pool.creations_since_sweep.get(), 1);
    }
    drop(id);
    for _ in 0..4 {
        assert!(pool.lookup("root:α").is_none());
        assert!(pool.lookup("missing").is_none());
        let stats = pool.census();
        assert_eq!((stats.entries, stats.live, stats.dead), (1, 0, 1));
        assert_eq!(stats.dead_text_bytes, "root:α".len());
        assert_eq!(stats.live_text_bytes, 0);
        assert_eq!(pool.creations_since_sweep.get(), 1);
        assert!(!pool.swept_during_intern.get());
    }
    pool.sweep();
    assert_eq!(pool.census().entries, 0);
}

#[test]
fn growth_and_shrink_preserve_survivors_and_unique_entries() {
    let pool = QueryPool::default();
    let all: Vec<_> = (0..2048)
        .map(|index| pool.intern(&format!("input[\"한글{index}\"].value")))
        .collect();
    let expanded = pool.census().capacity;
    for id in &all {
        assert_eq!(pool.lookup(id.as_str()).as_ref(), Some(id));
    }
    let survivors: Vec<_> = all
        .into_iter()
        .enumerate()
        .filter(|(index, _)| index % 127 == 0)
        .map(|(_, id)| id)
        .collect();
    pool.sweep();
    let stats = pool.census();
    assert_eq!(
        (stats.entries, stats.live, stats.dead),
        (survivors.len(), survivors.len(), 0)
    );
    assert!(stats.capacity < expanded);
    let foreign = QueryPool::default();
    let map: FxHashMap<_, _> = survivors
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect();
    for (index, id) in survivors.iter().enumerate() {
        assert_eq!(pool.lookup(id.as_str()).as_ref(), Some(id));
        assert_eq!(map.get(&foreign.intern(id.as_str())), Some(&index));
        let again = pool.intern(id.as_str());
        assert!(Rc::ptr_eq(&id.0, &again.0));
    }
    assert_eq!(pool.census().entries, survivors.len());
    assert_eq!(pool.creations_since_sweep.get(), 0);
    drop(map);
    drop(survivors);
    pool.sweep();
    assert_eq!(pool.census().entries, 0);
}

#[test]
fn dead_revival_at_the_threshold_is_not_counted_as_a_sweep_survivor() {
    let pool = QueryPool::default();
    let retained: Vec<_> = (0..800)
        .map(|index| pool.intern(&format!("retained:{index:04}")))
        .collect();
    for index in 0..224 {
        drop(pool.intern(&format!("temporary:{index:04}")));
    }
    let before = pool.census();
    assert_eq!((before.entries, before.live, before.dead), (1024, 800, 224));
    assert_eq!(pool.next_sweep.get(), 1024);
    assert_eq!(pool.creations_since_sweep.get(), 1024);
    assert!(!pool.swept_during_intern.get());

    let revived = pool.intern("temporary:0000");
    let after = pool.census();
    assert_eq!((after.entries, after.live, after.dead), (801, 801, 0));
    assert_eq!(pool.next_sweep.get(), 1600);
    assert_eq!(pool.creations_since_sweep.get(), 1);
    assert!(pool.swept_during_intern.get());
    drop(revived);
    // The in-flight sweep flag requires cleanup even with only one new birth.
    pool.maintain_transient();
    let completed = pool.census();
    assert_eq!(
        (completed.entries, completed.live, completed.dead),
        (800, 800, 0)
    );
    assert_eq!(pool.next_sweep.get(), 1600);
    assert_eq!(pool.creations_since_sweep.get(), 0);
    assert!(!pool.swept_during_intern.get());
    for id in &retained {
        assert_eq!(pool.lookup(id.as_str()).as_ref(), Some(id));
    }
}

#[test]
fn repeated_revival_reuses_one_entry_and_counts_maintenance_work() {
    let pool = QueryPool::default();
    let original = pool.intern("temporary:repeat");
    let identity = Rc::as_ptr(&original.0);
    drop(original);
    for birth in 2..=1024 {
        let revived = pool.intern("temporary:repeat");
        assert_eq!(Rc::as_ptr(&revived.0), identity);
        let stats = pool.census();
        assert_eq!((stats.entries, stats.live, stats.dead), (1, 1, 0));
        assert_eq!(Rc::strong_count(&revived.0), 2);
        assert_eq!(pool.creations_since_sweep.get(), birth);
        drop(revived);
        assert!(pool.lookup("temporary:repeat").is_none());
        assert_eq!(pool.census().dead, 1);
    }
    assert!(!pool.swept_during_intern.get());
    pool.maintain_transient();
    assert_eq!(pool.census().entries, 0);
    assert_eq!(pool.creations_since_sweep.get(), 0);
    assert_eq!(pool.next_sweep.get(), 1024);
}

#[test]
fn direct_churn_reclaims_dead_entries_without_explicit_sweeps() {
    let pool = QueryPool::default();
    for index in 0..8192 {
        let text = format!("temporary:{index:05}");
        assert_eq!(pool.intern(&text).as_str(), text);
        if index % 257 == 0 {
            let stats = pool.census();
            assert_eq!(stats.live, 0);
            assert_eq!(stats.entries, stats.dead);
            assert!(stats.entries <= 1024);
            assert_eq!(stats.text_bytes, stats.entries * "temporary:00000".len());
        }
    }
    assert!(pool.census().entries <= 1024);
}

#[test]
fn changing_dependencies_preserves_a_shared_snapshot_and_its_ids() {
    let pool = QueryPool::default();
    let a = pool.intern("input.a");
    let mut current: ReadSet = [a.clone(), a].into_iter().collect();
    let old = current.clone();
    let shared = current.storage().unwrap();
    assert_eq!(shared.identity, old.storage().unwrap().identity);
    assert_eq!(shared.len, 1);
    assert_eq!(shared.strong_count, 2);
    assert_eq!(current.storage().unwrap().strong_count, 2);
    let b = pool.intern("input.b");
    assert!(current.insert(b.clone()));
    assert!(!current.insert(b));
    assert_ne!(
        current.storage().unwrap().identity,
        old.storage().unwrap().identity
    );
    assert_eq!(names(&old), BTreeSet::from(["input.a"]));
    assert_eq!(names(&current), BTreeSet::from(["input.a", "input.b"]));
    drop(current);
    pool.sweep();
    assert_eq!((pool.census().entries, pool.census().live), (1, 1));
    assert_eq!(pool.lookup("input.a").as_ref(), old.iter().next());
    assert!(pool.lookup("input.b").is_none());
    drop(old);
    assert_eq!((pool.census().live, pool.census().dead), (0, 1));
    pool.sweep();
    assert_eq!(pool.census().text_bytes, 0);
}

#[test]
fn empty_snapshots_have_no_body_and_exact_iteration() {
    let empty = ReadSet::default();
    let collected: ReadSet = std::iter::empty::<QueryId>().collect();
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
    assert!(empty.storage().is_none());
    assert!(empty.clone().storage().is_none());
    assert!(collected.storage().is_none());
    let mut iter = empty.iter();
    assert_eq!(iter.size_hint(), (0, Some(0)));
    assert_eq!(iter.len(), 0);
    assert!(iter.next().is_none());
    assert!(iter.next().is_none());
    assert_eq!((&empty).into_iter().count(), 0);
}

#[test]
fn census_keeps_thin_ids_and_reports_the_strong_pool_entry() {
    let pool = QueryPool::default();
    let id = pool.intern("root:unit");
    let stats = pool.census();
    assert_eq!(stats.query_id_bytes, size_of::<usize>());
    assert_eq!(stats.read_set_bytes, size_of::<usize>());
    assert_eq!(stats.query_key_bytes, size_of::<QueryKey>());
    assert_eq!(stats.text_handle_bytes, size_of::<Rc<str>>());
    assert_eq!(stats.weak_key_bytes, 0);
    assert_eq!(stats.interner_entry_bytes, size_of::<QueryId>());
    assert_eq!(stats.rc_header_bytes, 2 * size_of::<usize>());
    assert_eq!(stats.text_bytes, id.len());
    assert_eq!(
        stats.text_bytes,
        stats.live_text_bytes + stats.dead_text_bytes
    );
}
