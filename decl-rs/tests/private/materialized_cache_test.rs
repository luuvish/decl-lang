//! Rust-only ownership and identity checks for the weak-raw cache prototype.
//! These tests intentionally distinguish raw lifetime from returned-value lifetime.
use super::*;

type RawArray = Rc<Vec<(bool, Value)>>;

fn int(n: i64) -> Value {
    Value::Int(Num::Small(n))
}

fn assert_int(value: &Value, expected: i64) {
    let Value::Int(n) = value else {
        panic!("expected integer");
    };
    assert_eq!(n.to_i64(), Some(expected));
}

fn call_callback() -> Expr {
    Expr::Call {
        fun: Rc::new(Expr::Name("callback".into())),
        args: vec![],
    }
}

fn pre(expr: Expr, scope: Scope) -> Value {
    Value::PreVal(Rc::new(PreValV {
        expr: Rc::new(expr),
        scope,
    }))
}

// The marker has no owner outside the native function stored in the LocalFrame.
// Merely observing its Weak does not preserve the captured value.
fn callback_scope(calls: &Rc<Cell<usize>>, fail: bool) -> (Scope, Weak<u8>) {
    let marker = Rc::new(7_u8);
    let weak = Rc::downgrade(&marker);
    let calls = calls.clone();
    let callback = Value::Nat(Rc::new(move |_| {
        calls.set(calls.get() + 1);
        if fail {
            err("planned materialization failure")
        } else {
            Ok(int(i64::from(*marker)))
        }
    }));
    let mut scope = Scope::new("", None);
    scope.locals = scope.locals.with(Rc::from("callback"), callback);
    (scope, weak)
}

fn materialized_array(eng: &Engine, raw: &RawArray) -> Rc<RefCell<ArrV>> {
    let value = eng
        .mat_val(Value::PreArr(raw.clone()))
        .ok()
        .expect("successful array materialization");
    let Value::Arr(array) = value else {
        panic!("expected materialized array");
    };
    array
}

#[test]
fn live_raw_reuses_result_without_repeating_callbacks() {
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let (scope, marker) = callback_scope(&calls, false);
    let raw = Rc::new(vec![(false, pre(call_callback(), scope))]);
    let first = materialized_array(&eng, &raw);
    let second = materialized_array(&eng, &raw);
    assert!(Rc::ptr_eq(&first, &second));
    assert_eq!(calls.get(), 1);
    assert_eq!(eng.cached_literals(), 1);
    assert_int(&first.borrow().items[0], 7);
    assert!(marker.upgrade().is_some());

    drop(raw);
    assert!(marker.upgrade().is_none());
    assert_int(&second.borrow().items[0], 7);
    assert_eq!(calls.get(), 1);
}

#[test]
fn raw_preval_and_unused_local_capture_release_before_cached_result() {
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let (scope, marker) = callback_scope(&calls, false);
    let deferred = Rc::new(PreValV {
        expr: Rc::new(Expr::Lit(int(11))),
        scope,
    });
    let deferred_weak = Rc::downgrade(&deferred);
    let raw = Rc::new(vec![(false, Value::PreVal(deferred))]);
    let raw_weak = Rc::downgrade(&raw);
    let result = materialized_array(&eng, &raw);
    let result_weak = Rc::downgrade(&result);
    assert!(marker.upgrade().is_some());
    assert_eq!(calls.get(), 0);

    drop(raw);
    assert!(raw_weak.upgrade().is_none());
    assert!(deferred_weak.upgrade().is_none());
    assert!(marker.upgrade().is_none());
    assert_eq!(calls.get(), 0);
    assert_eq!(eng.cached_literals(), 1);
    assert_int(&result.borrow().items[0], 11);

    drop(result);
    assert!(result_weak.upgrade().is_some());
    eng.clear_materialized();
    assert!(result_weak.upgrade().is_none());
    assert_eq!(eng.cached_literals(), 0);
}

#[test]
fn returned_native_and_closure_keep_captures_after_original_engine_drops() {
    for return_closure in [false, true] {
        let eng = Engine::bare(Env::new());
        let calls = Rc::new(Cell::new(0));
        let (scope, marker) = callback_scope(&calls, false);
        let expr = if return_closure {
            Expr::Lambda {
                params: vec![],
                body: Rc::new(call_callback()),
            }
        } else {
            Expr::Name("callback".into())
        };
        let raw = Rc::new(vec![(false, pre(expr, scope))]);
        let raw_weak = Rc::downgrade(&raw);
        let result = materialized_array(&eng, &raw);
        let function = result.borrow().items[0].clone();
        assert_eq!(calls.get(), 0);
        drop(raw);
        assert!(raw_weak.upgrade().is_none());
        assert!(marker.upgrade().is_some());

        eng.clear_materialized();
        drop(result);
        drop(eng);
        // Exercise the existing public collector while only the returned value
        // owns its function/scope. No assertion depends on collector node counts.
        collect_cycles();
        assert!(marker.upgrade().is_some());
        let caller = Engine::bare(Env::new());
        let value = caller
            .call(&function, vec![], &Scope::new("", None))
            .ok()
            .expect("returned function remains callable");
        assert_int(&value, 7);
        assert_eq!(calls.get(), 1);
        drop(function);
        assert!(marker.upgrade().is_none());
    }
}

#[test]
fn object_identity_reuses_map_and_releases_raw_capture() {
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let (scope, marker) = callback_scope(&calls, false);
    let raw = Rc::new(vec![("n".into(), pre(call_callback(), scope))]);
    let key = Rc::as_ptr(&raw) as usize;
    let first = eng.mat_val(Value::PreObj(raw.clone())).ok().unwrap();
    let second = eng.mat_val(Value::PreObj(raw.clone())).ok().unwrap();
    let (Value::Map(first), Value::Map(second)) = (first, second) else {
        panic!("expected materialized maps");
    };
    assert!(Rc::ptr_eq(&first, &second));
    assert_eq!(calls.get(), 1);
    assert_int(first.borrow().entries.get("n").unwrap(), 7);
    drop(raw);
    assert!(marker.upgrade().is_none());
    let cache = eng.mat_cache.borrow();
    let (MaterializedLiteral::Object(identity), _) = cache.get(&key).unwrap() else {
        panic!("object identity retains its type");
    };
    assert_eq!(identity.as_ptr() as usize, key);
    assert_eq!(identity.strong_count(), 0);
}

#[test]
fn failures_are_retried_and_do_not_populate_cache() {
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let (scope, marker) = callback_scope(&calls, true);
    let raw = Rc::new(vec![(false, pre(call_callback(), scope))]);
    for expected_calls in 1..=2 {
        let result = eng.mat_val(Value::PreArr(raw.clone()));
        let Err(Fail::Eval(error)) = result else {
            panic!("expected callback failure");
        };
        assert_eq!(error.msg, "planned materialization failure");
        assert_eq!(eng.cached_literals(), 0);
        assert_eq!(calls.get(), expected_calls);
        assert!(marker.upgrade().is_some());
    }
    drop(raw);
    assert!(marker.upgrade().is_none());
}

#[test]
fn clear_recomputes_live_raw_without_invalidating_old_result() {
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let (scope, marker) = callback_scope(&calls, false);
    let raw = Rc::new(vec![(false, pre(call_callback(), scope))]);
    let first = materialized_array(&eng, &raw);
    eng.clear_materialized();
    assert_eq!(eng.cached_literals(), 0);
    assert!(marker.upgrade().is_some());
    let second = materialized_array(&eng, &raw);
    assert!(!Rc::ptr_eq(&first, &second));
    assert_eq!(calls.get(), 2);
    assert_int(&first.borrow().items[0], 7);
    assert_int(&second.borrow().items[0], 7);
    drop(eng);
    assert!(marker.upgrade().is_some());
    drop(raw);
    assert!(marker.upgrade().is_none());
    assert_int(&first.borrow().items[0], 7);
    assert_int(&second.borrow().items[0], 7);
}

#[test]
fn transient_restores_original_cache_and_releases_scratch_results() {
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let (scope, _) = callback_scope(&calls, false);
    let raw = Rc::new(vec![(false, pre(call_callback(), scope))]);
    let original = materialized_array(&eng, &raw);
    let scratch_weak = eng.transient(|| {
        assert_eq!(eng.cached_literals(), 0);
        let scratch_raw = Rc::new(vec![(false, int(19))]);
        let scratch = materialized_array(&eng, &scratch_raw);
        let weak = Rc::downgrade(&scratch);
        drop(scratch_raw);
        drop(scratch);
        assert_eq!(eng.cached_literals(), 1);
        assert!(weak.upgrade().is_some());
        weak
    });
    assert!(scratch_weak.upgrade().is_none());
    assert_eq!(eng.cached_literals(), 1);
    let again = materialized_array(&eng, &raw);
    assert!(Rc::ptr_eq(&original, &again));
    assert_eq!(calls.get(), 1);
}

#[test]
fn transient_unwind_restores_original_cache_and_drops_scratch() {
    let eng = Engine::bare(Env::new());
    let raw = Rc::new(vec![(false, int(31))]);
    let original = materialized_array(&eng, &raw);
    let scratch_weak = RefCell::new(None);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eng.transient(|| {
            assert_eq!(eng.cached_literals(), 0);
            let scratch_raw = Rc::new(vec![(false, int(37))]);
            let scratch = materialized_array(&eng, &scratch_raw);
            *scratch_weak.borrow_mut() = Some(Rc::downgrade(&scratch));
            panic!("planned transient unwind");
        });
    }));
    assert!(outcome.is_err());
    assert!(scratch_weak.borrow().as_ref().unwrap().upgrade().is_none());
    assert_eq!(eng.cached_literals(), 1);
    assert!(Rc::ptr_eq(&original, &materialized_array(&eng, &raw)));
}

#[test]
fn callbacks_can_read_cache_and_materialize_another_literal() {
    let eng = Engine::bare(Env::new());
    let weak_engine = Rc::downgrade(&eng);
    let calls = Rc::new(Cell::new(0));
    let callback_calls = calls.clone();
    let callback = Value::Nat(Rc::new(move |_| {
        callback_calls.set(callback_calls.get() + 1);
        let eng = weak_engine.upgrade().expect("callback engine lives");
        assert_eq!(eng.cached_literals(), 0);
        let nested = Rc::new(vec![(false, int(23))]);
        let result = materialized_array(&eng, &nested);
        assert_int(&result.borrow().items[0], 23);
        assert_eq!(eng.cached_literals(), 1);
        Ok(int(29))
    }));
    let mut scope = Scope::new("", None);
    scope.locals = scope.locals.with(Rc::from("callback"), callback);
    let raw = Rc::new(vec![(false, pre(call_callback(), scope))]);
    let first = materialized_array(&eng, &raw);
    let second = materialized_array(&eng, &raw);
    assert!(Rc::ptr_eq(&first, &second));
    assert_int(&first.borrow().items[0], 29);
    assert_eq!(calls.get(), 1);
    assert_eq!(eng.cached_literals(), 2);
}

#[test]
fn dead_array_and_object_keys_remain_pinned_by_typed_weak_identities() {
    let eng = Engine::bare(Env::new());
    // This inspects each retained Weak, not an assumption that an allocator
    // happens to recycle a recently freed address during this short test.
    let mut keys = HashSet::new();
    for n in 0..32 {
        let raw = Rc::new(vec![(false, int(n))]);
        let array_key = Rc::as_ptr(&raw) as usize;
        assert!(keys.insert(array_key));
        drop(materialized_array(&eng, &raw));
        drop(raw);
        {
            let cache = eng.mat_cache.borrow();
            let (MaterializedLiteral::Array(identity), _) = cache.get(&array_key).unwrap() else {
                panic!("array identity retains its type");
            };
            assert_eq!(identity.as_ptr() as usize, array_key);
            assert_eq!(identity.strong_count(), 0);
            assert!(identity.upgrade().is_none());
        }

        let raw = Rc::new(vec![("n".into(), int(n))]);
        let object_key = Rc::as_ptr(&raw) as usize;
        assert!(keys.insert(object_key));
        drop(eng.mat_val(Value::PreObj(raw)).ok().unwrap());
        let cache = eng.mat_cache.borrow();
        let (MaterializedLiteral::Object(identity), _) = cache.get(&object_key).unwrap() else {
            panic!("object identity retains its type");
        };
        assert_eq!(identity.as_ptr() as usize, object_key);
        assert_eq!(identity.strong_count(), 0);
        assert!(identity.upgrade().is_none());
    }
    assert_eq!(eng.cached_literals(), 64);
}

#[test]
fn make_mut_gets_new_identity_and_leaves_old_materialization_valid() {
    let eng = Engine::bare(Env::new());
    let mut raw = Rc::new(vec![(false, int(1))]);
    let old_key = Rc::as_ptr(&raw) as usize;
    let first = materialized_array(&eng, &raw);
    // The cache's own Weak is sufficient to prevent get_mut and force make_mut
    // to dissociate the allocation; the test adds no extra Weak/strong owner.
    assert_eq!(Rc::strong_count(&raw), 1);
    assert_eq!(Rc::weak_count(&raw), 1);
    assert!(Rc::get_mut(&mut raw).is_none());
    Rc::make_mut(&mut raw).push((false, int(2)));
    assert_ne!(Rc::as_ptr(&raw) as usize, old_key);
    let second = materialized_array(&eng, &raw);
    assert!(!Rc::ptr_eq(&first, &second));
    assert_eq!(first.borrow().items.len(), 1);
    assert_eq!(second.borrow().items.len(), 2);
    assert_int(&first.borrow().items[0], 1);
    assert_int(&second.borrow().items[1], 2);
    assert_eq!(eng.cached_literals(), 2);
    let cache = eng.mat_cache.borrow();
    let (MaterializedLiteral::Array(identity), _) = cache.get(&old_key).unwrap() else {
        panic!("old array identity remains pinned");
    };
    assert_eq!(identity.strong_count(), 0);
    assert_eq!(identity.as_ptr() as usize, old_key);
}

// Bounded destruction checks. They exercise 512 links on the ordinary library
// test thread; they make no claim about arbitrary depth or CLI stack limits.
#[test]
fn bounded_raw_spread_chain_releases_all_raw_owners_after_materialization() {
    const DEPTH: usize = 512;
    let eng = Engine::bare(Env::new());
    let mut raw = Rc::new(Vec::new());
    let mut raw_weaks = Vec::with_capacity(DEPTH + 1);
    raw_weaks.push(Rc::downgrade(&raw));
    for n in 0..DEPTH {
        raw = Rc::new(vec![(true, Value::PreArr(raw)), (false, int(n as i64))]);
        raw_weaks.push(Rc::downgrade(&raw));
    }
    let result = eng
        .mat_val(Value::PreArr(raw))
        .ok()
        .expect("bounded spread chain materializes");
    let Value::Arr(result) = result else {
        panic!("expected materialized array");
    };
    assert_eq!(result.borrow().items.len(), DEPTH);
    for (n, item) in result.borrow().items.iter().enumerate() {
        assert_int(item, n as i64);
    }
    assert!(raw_weaks.iter().all(|raw| raw.upgrade().is_none()));
    assert_eq!(eng.cached_literals(), 1);
    let result_weak = Rc::downgrade(&result);
    drop(result);
    assert!(result_weak.upgrade().is_some());
    eng.clear_materialized();
    assert!(result_weak.upgrade().is_none());
}

#[test]
fn bounded_local_capture_chain_releases_unused_callbacks_with_raw_owner() {
    const DEPTH: usize = 512;
    let eng = Engine::bare(Env::new());
    let calls = Rc::new(Cell::new(0));
    let mut markers = Vec::with_capacity(DEPTH);
    let mut scope = Scope::new("", None);
    for n in 0..DEPTH {
        let marker = Rc::new(n as i64);
        markers.push(Rc::downgrade(&marker));
        let calls = calls.clone();
        let callback = Value::Nat(Rc::new(move |_| {
            calls.set(calls.get() + 1);
            Ok(int(*marker))
        }));
        // Repeated names intentionally leave shadowed frames in the chain;
        // a free-variable lookup must not be used to trim this test's owners.
        scope.locals = scope.locals.with(Rc::from("unused"), callback);
    }
    let raw = Rc::new(vec![(false, pre(Expr::Lit(int(43)), scope))]);
    assert!(markers.iter().all(|marker| marker.upgrade().is_some()));
    let result = eng
        .mat_val(Value::PreArr(raw))
        .ok()
        .expect("bounded capture chain materializes");
    let Value::Arr(result) = result else {
        panic!("expected materialized array");
    };
    assert!(markers.iter().all(|marker| marker.upgrade().is_none()));
    assert_eq!(calls.get(), 0);
    assert_int(&result.borrow().items[0], 43);
    drop(eng);
    assert_int(&result.borrow().items[0], 43);
}
