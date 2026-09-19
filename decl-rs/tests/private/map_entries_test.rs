//! Native shared-key ownership, ordered mutation and collector boundaries.
use super::*;
use crate::ast::Expr;
use crate::engine::Engine;
use crate::semantics::{collect_cycles, record_instance, ty, ArrV, MapV, PrefixPath, RTk, RecInst};
use crate::semantics::{Env, EvalErr, Fail, PreValV, Scope, Seg, R};
use std::cell::{Cell, RefCell};

fn integer(value: i64) -> Value {
    Value::Int(value.into())
}

fn pairs(items: &[(&str, i64)]) -> MapEntries {
    items
        .iter()
        .map(|(key, value)| ((*key).to_owned(), integer(*value)))
        .collect()
}

fn assert_pairs(entries: &MapEntries, expected: &[(&str, i64)]) {
    assert_eq!(entries.len(), expected.len());
    for (index, ((key, value), (wanted_key, wanted_value))) in
        entries.iter().zip(expected).enumerate()
    {
        assert_eq!(key.as_str(), *wanted_key);
        assert!(crate::semantics::value_eq(value, &integer(*wanted_value)));
        assert_eq!(entries.get_index_of(wanted_key), Some(index));
        assert!(std::ptr::eq(entries.get(wanted_key).unwrap(), value));
        assert!(std::ptr::eq(entries.get_index(index).unwrap().0, key));
    }
    assert!(entries.get_index(expected.len()).is_none());
    assert_eq!(entries.iter().rev().count(), expected.len());
}

fn shared_shape(entries: &MapEntries) -> &Rc<KeyShape> {
    let Storage::Shared { shape, .. } = &entries.storage else {
        panic!("expected sealed shape")
    };
    shape
}

#[test]
fn ordered_shapes_preserve_duplicates_empty_unicode_keys_and_value_independence() {
    let mut pool = MapShapePool::default();
    let mut first = pairs(&[("", 1), ("한글😀", 2), ("", 3), ("z", 4)]);
    let mut same_keys = pairs(&[("", 10), ("한글😀", 20), ("z", 30)]);
    let mut reordered = pairs(&[("z", 4), ("한글😀", 2), ("", 3)]);
    assert!(pool.seal(&mut first));
    assert!(pool.seal(&mut same_keys));
    assert!(pool.seal(&mut reordered));
    assert!(Rc::ptr_eq(shared_shape(&first), shared_shape(&same_keys)));
    assert!(!Rc::ptr_eq(shared_shape(&first), shared_shape(&reordered)));
    assert_pairs(&first, &[("", 3), ("한글😀", 2), ("z", 4)]);
    assert_pairs(&same_keys, &[("", 10), ("한글😀", 20), ("z", 30)]);
    assert_pairs(&reordered, &[("z", 4), ("한글😀", 2), ("", 3)]);
    let path = PrefixPath::default();
    let a = Value::Map(Rc::new(RefCell::new(MapV {
        entries: first,
        path: path.clone(),
    })));
    let b = Value::Map(Rc::new(RefCell::new(MapV {
        entries: reordered,
        path,
    })));
    assert!(
        crate::semantics::value_eq(&a, &b),
        "language equality is independent of key order and shape identity"
    );
}

#[test]
fn shared_value_updates_and_ordered_structural_mutation_isolate_the_other_map() {
    let mut pool = MapShapePool::default();
    let mut stable = pairs(&[("a", 1), ("b", 2), ("c", 3)]);
    assert!(pool.seal(&mut stable));
    let mut changed = stable.clone();
    assert!(Rc::ptr_eq(shared_shape(&stable), shared_shape(&changed)));
    let old = changed.insert("b".into(), integer(20)).unwrap();
    assert!(crate::semantics::value_eq(&old, &integer(2)));
    *changed.get_mut("a").unwrap() = integer(10);
    *changed.get_index_mut(2).unwrap().1 = integer(30);
    assert!(Rc::ptr_eq(shared_shape(&stable), shared_shape(&changed)));
    assert_pairs(&changed, &[("a", 10), ("b", 20), ("c", 30)]);
    assert_pairs(&stable, &[("a", 1), ("b", 2), ("c", 3)]);

    assert!(changed.insert("d".into(), integer(40)).is_none());
    assert!(!changed.is_shared());
    assert!(stable.is_shared());
    assert!(!std::ptr::eq(
        stable.get_index(0).unwrap().0,
        changed.get_index(0).unwrap().0
    ));
    assert!(crate::semantics::value_eq(
        &changed.shift_remove("b").unwrap(),
        &integer(20)
    ));
    assert_pairs(&changed, &[("a", 10), ("c", 30), ("d", 40)]);
    assert!(changed.insert("b".into(), integer(200)).is_none());
    assert!(crate::semantics::value_eq(
        &changed.swap_remove("a").unwrap(),
        &integer(10)
    ));
    assert_pairs(&changed, &[("b", 200), ("c", 30), ("d", 40)]);
    assert_pairs(&stable, &[("a", 1), ("b", 2), ("c", 3)]);

    // Removal from Shared also promotes exactly this owner before applying
    // IndexMap's distinct shift/swap order contracts.
    let mut shifted = stable.clone();
    let mut swapped = stable.clone();
    shifted.shift_remove("a");
    swapped.swap_remove("a");
    assert_pairs(&shifted, &[("b", 2), ("c", 3)]);
    assert_pairs(&swapped, &[("c", 3), ("b", 2)]);
    assert_pairs(&stable, &[("a", 1), ("b", 2), ("c", 3)]);
}

#[test]
fn seal_clone_and_compatibility_conversion_never_share_the_values_vector() {
    let child = Rc::new(RefCell::new(ArrV {
        items: vec![integer(7)],
        path: PrefixPath::default(),
    }));
    let mut pool = MapShapePool::default();
    let mut first: MapEntries = [("child".into(), Value::Arr(child.clone()))]
        .into_iter()
        .collect();
    assert_eq!(Rc::strong_count(&child), 2);
    assert!(pool.seal(&mut first));
    assert_eq!(
        Rc::strong_count(&child),
        2,
        "sealing moves each value without cloning it"
    );
    let mut cloned = first.clone();
    assert_eq!(
        Rc::strong_count(&child),
        3,
        "ordinary clone adds exactly one value owner"
    );
    *cloned.get_mut("child").unwrap() = integer(11);
    assert_eq!(Rc::strong_count(&child), 2);
    assert!(matches!(first.get("child"), Some(Value::Arr(value)) if Rc::ptr_eq(value, &child)));
    assert_pairs(&cloned, &[("child", 11)]);

    let shape = Rc::downgrade(shared_shape(&first));
    let ordered = first.into_ordered();
    assert_eq!(
        Rc::strong_count(&child),
        2,
        "owned compatibility conversion moves values"
    );
    assert!(matches!(ordered.get("child"), Some(Value::Arr(value)) if Rc::ptr_eq(value, &child)));
    assert!(
        shape.upgrade().is_some(),
        "the separate clone still owns the shared keys"
    );
    cloned
        .as_ordered_mut()
        .entry("other".into())
        .or_insert(integer(19));
    assert_pairs(&cloned, &[("child", 11), ("other", 19)]);
    assert!(
        shape.upgrade().is_none(),
        "the weak pool does not retain converted shapes"
    );
    drop(ordered);
    assert_eq!(Rc::strong_count(&child), 1);
}

#[test]
fn forced_fingerprint_collision_and_expired_weak_slot_do_not_merge_shapes() {
    let mut pool = MapShapePool::default();
    let mut first = pairs(&[("ab", 1), ("c", 2)]);
    let mut collision = pairs(&[("a", 3), ("bc", 4)]);
    assert!(pool.seal_with_fingerprint(&mut first, 17));
    assert!(pool.seal_with_fingerprint(&mut collision, 17));
    assert!(!Rc::ptr_eq(shared_shape(&first), shared_shape(&collision)));
    assert_pairs(&first, &[("ab", 1), ("c", 2)]);
    assert_pairs(&collision, &[("a", 3), ("bc", 4)]);
    let expired = Rc::downgrade(shared_shape(&collision));
    drop(collision);
    assert!(expired.upgrade().is_none());
    assert!(pool.entries[&17].upgrade().is_none());
    let mut replacement = pairs(&[("a", 30), ("bc", 40)]);
    assert!(pool.seal_with_fingerprint(&mut replacement, 17));
    assert_pairs(&replacement, &[("a", 30), ("bc", 40)]);
    assert_pairs(&first, &[("ab", 1), ("c", 2)]);
    assert!(pool.entries[&17].upgrade().is_some());
}

#[test]
fn admission_controls_and_pool_cap_do_not_retain_maps_or_invalidate_live_shapes() {
    let mut pool = MapShapePool::default();
    let mut empty = MapEntries::new();
    let mut wide: MapEntries = (0..=MAX_SHAPE_KEYS)
        .map(|i| (format!("key-{i}"), integer(i as i64)))
        .collect();
    assert!(!pool.seal(&mut empty) && !pool.seal(&mut wide));
    assert!(!empty.is_shared() && !wide.is_shared());
    assert!(pool.entries.is_empty());
    let mut retained = pairs(&[("retained", 7)]);
    assert!(pool.seal_with_fingerprint(&mut retained, 0));
    let weak_shape = Rc::downgrade(shared_shape(&retained));
    for i in 1..=MAX_POOL_ENTRIES + 1 {
        let mut transient = pairs(&[("transient", i as i64)]);
        assert!(pool.seal_with_fingerprint(&mut transient, i as u64));
        assert!(pool.entries.len() <= MAX_POOL_ENTRIES);
    }
    assert_pairs(&retained, &[("retained", 7)]);
    assert!(weak_shape.upgrade().is_some());
    drop(retained);
    assert!(
        weak_shape.upgrade().is_none(),
        "pool entries are weak after capacity turnover"
    );
}

#[test]
fn shared_iteration_and_owned_consumption_keep_order_and_one_owner_per_entry() {
    let child = Rc::new(RefCell::new(ArrV {
        items: vec![],
        path: PrefixPath::default(),
    }));
    let mut entries: MapEntries = [
        ("a".into(), Value::Arr(child.clone())),
        ("b".into(), integer(2)),
    ]
    .into_iter()
    .collect();
    assert!(MapShapePool::default().seal(&mut entries));
    let mut iterator = entries.iter();
    assert_eq!(iterator.len(), 2);
    assert_eq!(iterator.next_back().unwrap().0, "b");
    assert_eq!(iterator.next().unwrap().0, "a");
    assert!(iterator.next().is_none() && iterator.next_back().is_none());
    assert_eq!(Rc::strong_count(&child), 2);
    for (key, value) in entries.iter_mut() {
        if key == "b" {
            *value = integer(20);
        }
    }
    assert!(entries.is_shared());
    let moved: Vec<_> = entries.into_iter().collect();
    assert_eq!(
        moved
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(Rc::strong_count(&child), 2);
    assert!(crate::semantics::value_eq(&moved[1].1, &integer(20)));
    drop(moved);
    assert_eq!(Rc::strong_count(&child), 1);
}

fn anchor() -> Rc<RefCell<RecInst>> {
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
fn shared_shapes_add_no_gc_edges_and_duplicate_values_keep_external_roots_alive() {
    collect_cycles();
    let holder = anchor();
    let child = Rc::new(RefCell::new(ArrV {
        items: vec![Value::Rec(holder.clone())],
        path: PrefixPath::default(),
    }));
    let mut entries: MapEntries = [
        ("left".into(), Value::Arr(child.clone())),
        ("right".into(), Value::Arr(child.clone())),
    ]
    .into_iter()
    .collect();
    let mut pool = MapShapePool::default();
    assert!(pool.seal(&mut entries));
    let first = Rc::new(RefCell::new(MapV {
        entries,
        path: PrefixPath::default(),
    }));
    let second = Rc::new(RefCell::new(MapV {
        entries: first.borrow().entries.clone(),
        path: first.borrow().path.clone(),
    }));
    assert_eq!(
        Rc::strong_count(&child),
        5,
        "two duplicate edges in each map and one caller"
    );
    let weak_shape = Rc::downgrade(shared_shape(&second.borrow().entries));
    holder.borrow_mut().extras.extend([
        ("first".into(), Value::Map(first.clone())),
        ("second".into(), Value::Map(second.clone())),
    ]);
    let weak_holder = Rc::downgrade(&holder);
    let weak_child = Rc::downgrade(&child);
    let weak_first = Rc::downgrade(&first);
    let weak_second = Rc::downgrade(&second);
    drop((holder, child, first));
    assert_eq!(
        collect_cycles(),
        0,
        "the external second map roots both duplicate descendants"
    );
    second.borrow_mut().set("left".into(), integer(17));
    second.borrow_mut().set("right".into(), integer(19));
    assert!(second.borrow().entries.is_shared());
    assert!(collect_cycles() >= 3);
    assert!(
        weak_holder.upgrade().is_none()
            && weak_child.upgrade().is_none()
            && weak_first.upgrade().is_none()
    );
    assert_pairs(&second.borrow().entries, &[("left", 17), ("right", 19)]);
    assert!(weak_shape.upgrade().is_some());
    drop(second);
    assert!(weak_second.upgrade().is_none() && weak_shape.upgrade().is_none());
}

#[test]
fn shared_clear_preserves_native_drop_order_and_existing_outer_borrow_boundary() {
    struct OnDrop {
        index: usize,
        owner: Weak<RefCell<MapV>>,
        events: Rc<RefCell<Vec<(usize, bool)>>>,
        calls: Rc<Cell<usize>>,
    }
    impl Drop for OnDrop {
        fn drop(&mut self) {
            let owner = self.owner.upgrade().unwrap();
            self.events
                .borrow_mut()
                .push((self.index, owner.try_borrow().is_err()));
            self.calls.set(self.calls.get() + 1);
        }
    }
    let map = Rc::new(RefCell::new(MapV {
        entries: MapEntries::new(),
        path: PrefixPath::default(),
    }));
    let events = Rc::new(RefCell::new(Vec::new()));
    let calls = Rc::new(Cell::new(0));
    for i in 0..3 {
        let capture = OnDrop {
            index: i,
            owner: Rc::downgrade(&map),
            events: events.clone(),
            calls: calls.clone(),
        };
        let value = Value::native(move |_| {
            let _ = &capture;
            panic!("map conversion and clear must not execute native functions")
        });
        map.borrow_mut().set(format!("key-{i}"), value);
    }
    let mut pool = MapShapePool::default();
    assert!(pool.seal(&mut map.borrow_mut().entries));
    assert_eq!(calls.get(), 0, "sealing does not retire native values");
    map.borrow_mut().entries.clear();
    assert_eq!(*events.borrow(), [(0, true), (1, true), (2, true)]);
    assert!(map.borrow().entries.is_empty());
    assert!(matches!(map.borrow().entries.storage, Storage::Empty));
    assert_eq!(calls.get(), 3);
    map.borrow_mut().set("after".into(), integer(7));
    assert_pairs(&map.borrow().entries, &[("after", 7)]);
}

#[test]
fn construction_seals_without_extra_value_owners_and_moves_wide_entries_once() {
    let child = Rc::new(RefCell::new(ArrV {
        items: vec![integer(7)],
        path: PrefixPath::default(),
    }));
    let mut pool = MapShapePool::default();
    let mut entries = MapEntries::building();
    assert_eq!(entries.capacity(), 0);
    assert!(entries.shift_remove("absent").is_none());
    assert!(matches!(entries.storage, Storage::Small(_)));
    for i in 0..MAX_SHAPE_KEYS {
        entries.insert(format!("key-{i}"), Value::Arr(child.clone()));
    }
    assert_eq!(Rc::strong_count(&child), 1 + MAX_SHAPE_KEYS);
    let old = entries.insert("key-0".into(), Value::Arr(child.clone()));
    assert_eq!(Rc::strong_count(&child), 2 + MAX_SHAPE_KEYS);
    drop(old);
    assert!(matches!(entries.storage, Storage::Small(_)));
    let mut cloned = entries.clone();
    assert_eq!(Rc::strong_count(&child), 1 + 2 * MAX_SHAPE_KEYS);
    *cloned.get_index_mut(0).unwrap().1 = integer(0);
    for (_, value) in cloned.iter_mut().skip(1) {
        *value = integer(1);
    }
    assert_eq!(Rc::strong_count(&child), 1 + MAX_SHAPE_KEYS);
    assert!(matches!(entries.get("key-0"), Some(Value::Arr(value)) if Rc::ptr_eq(value, &child)));
    drop(cloned);

    // Both a cold shape and a hit move values. The forced collision still
    // checks every ordered key before adopting an existing shape.
    assert!(pool.seal_with_fingerprint(&mut entries, 31));
    assert_eq!(Rc::strong_count(&child), 1 + MAX_SHAPE_KEYS);
    let mut same = MapEntries::building();
    same.extend((0..MAX_SHAPE_KEYS).map(|i| (format!("key-{i}"), integer(i as i64))));
    assert!(pool.seal_with_fingerprint(&mut same, 31));
    assert!(Rc::ptr_eq(shared_shape(&entries), shared_shape(&same)));
    let mut different = MapEntries::building();
    different.insert("different".into(), integer(1));
    assert!(pool.seal_with_fingerprint(&mut different, 31));
    assert!(!Rc::ptr_eq(
        shared_shape(&entries),
        shared_shape(&different)
    ));
    drop(entries);
    assert_eq!(Rc::strong_count(&child), 1);

    let mut wide = MapEntries::building();
    wide.extend((0..=MAX_SHAPE_KEYS).map(|i| (format!("key-{i}"), Value::Arr(child.clone()))));
    assert!(matches!(wide.storage, Storage::Owned(_)));
    assert_eq!(Rc::strong_count(&child), 2 + MAX_SHAPE_KEYS);
    assert!(!pool.seal(&mut wide));
    let moved: Vec<_> = wide.into_iter().collect();
    assert_eq!(Rc::strong_count(&child), 2 + MAX_SHAPE_KEYS);
    assert_eq!(moved.first().unwrap().0, "key-0");
    assert_eq!(moved.last().unwrap().0, "key-8");
    drop(moved);
    assert_eq!(Rc::strong_count(&child), 1);

    let mut empty = MapEntries::building();
    assert!(!pool.seal(&mut empty));
    assert!(matches!(empty.storage, Storage::Empty));
    assert_eq!(empty.capacity(), 0);
}

#[test]
fn compact_empty_storage_preserves_zero_capacity_and_ordinary_entry_behavior() {
    // The common inline body must no longer reserve an ordinary table header.
    // This is a layout constraint, not an estimate of retained model savings.
    assert!(std::mem::size_of::<MapEntries>() < std::mem::size_of::<OrderedMap<Value>>());
    let mut pool = MapShapePool::default();
    for mut entries in [
        MapEntries::new(),
        MapEntries::default(),
        MapEntries::with_capacity(0),
        MapEntries::from(OrderedMap::default()),
        std::iter::empty::<(String, Value)>().collect(),
    ] {
        assert!(matches!(entries.storage, Storage::Empty));
        assert!(matches!(entries.clone().storage, Storage::Empty));
        assert!(matches!(
            entries.with_values(vec![]).storage,
            Storage::Empty
        ));
        assert_eq!(entries.capacity(), 0);
        assert!(entries.get("").is_none() && entries.get_mut("").is_none());
        assert!(entries.get_index(0).is_none() && entries.get_index_mut(0).is_none());
        assert_eq!(entries.iter().size_hint(), (0, Some(0)));
        assert!(entries.iter().next_back().is_none());
        assert!(entries.iter_mut().next().is_none());
        assert!(entries.iter_mut().next_back().is_none());
        assert_eq!(entries.keys().len(), 0);
        assert_eq!(entries.values_mut().len(), 0);
        let mut owned = entries.clone().into_iter();
        assert_eq!(owned.len(), 0);
        assert!(owned.next().is_none() && owned.next_back().is_none());
        assert!(entries.shift_remove("missing").is_none());
        assert!(entries.swap_remove("missing").is_none());
        assert!(!pool.seal(&mut entries));
        entries.clear();
        assert!(matches!(entries.storage, Storage::Empty));

        // A public empty map still starts with ordinary insertion, rather than
        // taking the engine-only Small vector's different capacity behavior.
        let mut ordinary = OrderedMap::default();
        for (key, value) in [("", 1), ("한글😀", 2), ("", 3), ("z", 4)] {
            entries.insert(key.into(), integer(value));
            ordinary.insert(key.into(), integer(value));
            assert_eq!(entries.capacity(), ordinary.capacity());
        }
        assert!(matches!(entries.storage, Storage::Owned(_)));
        assert_pairs(&entries, &[("", 3), ("한글😀", 2), ("z", 4)]);
        let capacity = entries.capacity();
        let table = entries.as_ordered_mut() as *mut OrderedMap<Value>;
        entries.clear();
        assert_eq!(entries.capacity(), capacity);
        assert_eq!(entries.as_ordered_mut() as *mut OrderedMap<Value>, table);
    }
}

#[test]
fn boxed_ordinary_round_trip_keeps_entry_addresses_and_native_drop_order() {
    struct OnDrop(usize, Rc<RefCell<Vec<usize>>>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.1.borrow_mut().push(self.0);
        }
    }
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut ordinary = OrderedMap::with_capacity_and_hasher(7, FxBuildHasher);
    let mut owners = Vec::new();
    for index in 0..2 {
        let capture = OnDrop(index, events.clone());
        let value = Value::native(move |_| {
            let _ = &capture;
            panic!("boxing, conversion and iteration must not call native values")
        });
        let Value::Nat(owner) = &value else {
            unreachable!()
        };
        owners.push(Rc::downgrade(owner));
        ordinary.insert(index.to_string(), value);
    }
    let key = ordinary.get_index(0).unwrap().0 as *const String;
    let capacity = ordinary.capacity();
    let mut entries = MapEntries::from(ordinary);
    assert_eq!(entries.get_index(0).unwrap().0 as *const String, key);
    assert_eq!(entries.capacity(), capacity);
    assert!(owners.iter().all(|owner| owner.strong_count() == 1));
    let copy = entries.clone();
    assert!(owners.iter().all(|owner| owner.strong_count() == 2));
    drop(copy);
    assert!(owners.iter().all(|owner| owner.strong_count() == 1));
    assert!(events.borrow().is_empty());
    assert_eq!(
        entries.as_ordered_mut().get_index(0).unwrap().0 as *const String,
        key
    );
    let ordinary = entries.into_ordered();
    assert_eq!(ordinary.get_index(0).unwrap().0 as *const String, key);
    assert_eq!(ordinary.capacity(), capacity);
    let mut iter = MapEntries::from(ordinary).into_iter();
    assert!(events.borrow().is_empty());
    assert!(owners.iter().all(|owner| owner.strong_count() == 1));
    let first = iter.next().unwrap();
    assert_eq!(first.0, "0");
    drop(first);
    assert_eq!(*events.borrow(), [0]);
    drop(iter);
    assert_eq!(*events.borrow(), [0, 1]);
    assert!(owners.iter().all(|owner| owner.upgrade().is_none()));
}

#[test]
fn construction_duplicate_promotion_and_clear_keep_native_drop_borrow_order() {
    struct OnDrop {
        index: usize,
        map: Weak<RefCell<MapV>>,
        events: Rc<RefCell<Vec<(usize, bool)>>>,
    }
    impl Drop for OnDrop {
        fn drop(&mut self) {
            let map = self.map.upgrade().unwrap();
            self.events
                .borrow_mut()
                .push((self.index, map.try_borrow().is_err()));
        }
    }
    for count in [3, MAX_SHAPE_KEYS + 1] {
        let map = Rc::new(RefCell::new(MapV {
            entries: MapEntries::building(),
            path: PrefixPath::default(),
        }));
        let events = Rc::new(RefCell::new(Vec::new()));
        let native = |index| {
            let capture = OnDrop {
                index,
                map: Rc::downgrade(&map),
                events: events.clone(),
            };
            Value::native(move |_| {
                let _ = &capture;
                panic!("storage changes must not call the native value")
            })
        };
        for i in 0..count {
            map.borrow_mut().set(format!("key-{i}"), native(i));
            assert!(events.borrow().is_empty(), "promotion moves each owner");
        }
        map.borrow_mut().set("key-0".into(), native(100));
        assert_eq!(*events.borrow(), [(0, true)]);
        map.borrow_mut().entries.clear();
        let expected: Vec<_> = std::iter::once((0, true))
            .chain(std::iter::once((100, true)))
            .chain((1..count).map(|i| (i, true)))
            .collect();
        assert_eq!(*events.borrow(), expected);
    }
}

fn construction_callback(run: impl Fn() -> R<Value> + 'static) -> Value {
    Value::PreVal(Rc::new(PreValV {
        expr: Rc::new(Expr::Call {
            fun: Rc::new(Expr::Lit(Value::native(move |_| run()))),
            args: vec![],
        }),
        scope: Scope::new("root", None),
    }))
}

#[test]
fn engine_construction_keeps_reentry_duplicate_retirement_and_failure_cleanup() {
    struct OnDrop(usize, Rc<RefCell<Vec<usize>>>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.1.borrow_mut().push(self.0);
        }
    }
    // Both producing loops store each successful value immediately. A tainted
    // child is omitted by binding, while materialization propagates its failure.
    for materialize in [false, true] {
        for stop in 0..4 {
            let eng = Engine::bare(Env::new());
            let events = Rc::new(RefCell::new(Vec::new()));
            let calls = Rc::new(RefCell::new(Vec::new()));
            let produce = |index| {
                let events = events.clone();
                let calls = calls.clone();
                construction_callback(move || {
                    calls.borrow_mut().push(index);
                    let capture = OnDrop(index, events.clone());
                    Ok(Value::native(move |_| {
                        let _ = &capture;
                        panic!("stored native values are not evaluated")
                    }))
                })
            };
            let weak_engine = Rc::downgrade(&eng);
            let observed = events.clone();
            let observed_calls = calls.clone();
            let terminal = construction_callback(move || {
                observed_calls.borrow_mut().push(3);
                assert_eq!(
                    *observed.borrow(),
                    [0],
                    "duplicate retired before next child"
                );
                let eng = weak_engine.upgrade().unwrap();
                let nested = eng.materialize(
                    Value::PreObj(Rc::new(vec![("nested".into(), integer(9))])),
                    &[Seg::Name("nested".into())],
                )?;
                let Value::Map(nested) = nested else {
                    panic!("reentrant materialization must produce a map")
                };
                assert!(nested.borrow().entries.is_shared());
                assert_eq!(*observed.borrow(), [0]);
                match stop {
                    0 => Ok(integer(3)),
                    1 => Err(Fail::Taint),
                    2 => Err(Fail::Defer),
                    _ => Err(Fail::Eval(EvalErr {
                        msg: "native stop".into(),
                        code: Some("E5001".into()),
                    })),
                }
            });
            let raw = Rc::new(vec![
                ("same".into(), produce(0)),
                ("second".into(), produce(1)),
                ("same".into(), produce(2)),
                ("last".into(), terminal),
                ("future".into(), produce(4)),
            ]);
            let path = [Seg::Name("root".into())];
            let result = if materialize {
                eng.materialize(Value::PreObj(raw), &path)
            } else {
                eng.bind(
                    Value::JObj(raw),
                    &ty(RTk::Map {
                        key: ty(RTk::Prim("string".into())),
                        val: ty(RTk::Any),
                    }),
                    &path,
                    None,
                    &Scope::new("root", None),
                )
            };
            if stop == 0 || (stop == 1 && !materialize) {
                let Value::Map(map) = result.ok().expect("successful construction") else {
                    panic!("map result")
                };
                assert_eq!(*calls.borrow(), [0, 1, 2, 3, 4]);
                assert_eq!(*events.borrow(), [0]);
                assert!(map.borrow().entries.is_shared());
                let keys: Vec<_> = map.borrow().entries.keys().cloned().collect();
                assert_eq!(
                    keys,
                    if stop == 0 {
                        vec!["same", "second", "last", "future"]
                    } else {
                        vec!["same", "second", "future"]
                    }
                );
                drop(map);
                assert_eq!(*events.borrow(), [0, 2, 1, 4]);
            } else {
                assert!(match (stop, result) {
                    (1, Err(Fail::Taint)) | (2, Err(Fail::Defer)) => true,
                    (3, Err(Fail::Eval(e))) =>
                        e.msg == "native stop" && e.code.as_deref() == Some("E5001"),
                    _ => false,
                });
                assert_eq!(*calls.borrow(), [0, 1, 2, 3]);
                assert_eq!(
                    *events.borrow(),
                    [0, 2, 1],
                    "partial map retires in key order"
                );
            }
            assert!(eng.env.diagnostics_vec().is_empty());
        }
    }
}

#[test]
fn construction_values_remain_visible_to_collection_before_sealing() {
    collect_cycles();
    let holder = anchor();
    let child = Rc::new(RefCell::new(ArrV {
        items: vec![Value::Rec(holder.clone())],
        path: PrefixPath::default(),
    }));
    let mut entries = MapEntries::building();
    entries.insert("left".into(), Value::Arr(child.clone()));
    entries.insert("right".into(), Value::Arr(child.clone()));
    let map = Rc::new(RefCell::new(MapV {
        entries,
        path: PrefixPath::default(),
    }));
    holder
        .borrow_mut()
        .extras
        .push(("map".into(), Value::Map(map.clone())));
    let weak_holder = Rc::downgrade(&holder);
    let weak_child = Rc::downgrade(&child);
    let weak_map = Rc::downgrade(&map);
    drop((holder, child));
    collect_cycles();
    assert!(weak_holder.upgrade().is_some() && weak_child.upgrade().is_some());
    assert_eq!(map.borrow().entries.values().count(), 2);
    assert!(matches!(map.borrow().entries.storage, Storage::Small(_)));
    drop(map);
    assert!(collect_cycles() >= 3);
    assert!(weak_holder.upgrade().is_none());
    assert!(weak_child.upgrade().is_none());
    assert!(weak_map.upgrade().is_none());
}
