//! Rust-native child-path ownership, reentry and failure boundaries.
use super::*;

fn array_type(elem: RT) -> RT {
    ty(RTk::Arr {
        elem,
        lo: None,
        hi: None,
    })
}

fn map_type(val: RT) -> RT {
    ty(RTk::Map {
        key: ty(RTk::Prim("string".into())),
        val,
    })
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    Value::JObj(Rc::new(
        entries.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    ))
}

fn callback(run: impl Fn() -> R<Value> + 'static) -> Value {
    Value::PreVal(Rc::new(PreValV {
        expr: Rc::new(Expr::Call {
            fun: Rc::new(Expr::Lit(Value::native(move |_| run()))),
            args: vec![],
        }),
        scope: Scope::new("root", None),
    }))
}

fn bind_at(eng: &Engine, raw: Value, rt: &RT, path: &[Seg]) -> R<Value> {
    eng.bind(raw, rt, path, None, &Scope::new("root", None))
}

fn path_of(value: &Value) -> String {
    path_str(&value.place().expect("bound container path"), None)
}

#[test]
fn bound_nested_children_keep_their_paths_after_later_siblings() {
    let eng = Engine::bare(Env::new());
    let rt = array_type(map_type(ty(RTk::Rec(rec_type(false)))));
    let raw = Value::JArr(Rc::new(
        (0..2)
            .map(|_| object(vec![("a.b", object(vec![])), ("next", object(vec![]))]))
            .collect(),
    ));
    let path = [Seg::Name("root".into()), Seg::Key("outer.key".into())];
    let result = bind_at(&eng, raw, &rt, &path).ok().expect("nested binding");
    assert_eq!(path_of(&result), "root[\"outer.key\"]");
    let Value::Arr(array) = result else {
        panic!("array result");
    };
    let items = array.borrow().items.clone();
    for (i, item) in items.iter().enumerate() {
        assert_eq!(path_of(item), format!("root[\"outer.key\"][{i}]"));
        let Value::Map(map) = item else {
            panic!("map child");
        };
        let map = map.borrow();
        assert_eq!(
            path_of(map.get("a.b").unwrap()),
            format!("root[\"outer.key\"][{i}][\"a.b\"]")
        );
        assert_eq!(
            path_of(map.get("next").unwrap()),
            format!("root[\"outer.key\"][{i}][\"next\"]")
        );
    }
    assert!(eng.env.diagnostics_vec().is_empty());
}

#[test]
fn native_reentry_uses_an_independent_child_path_buffer() {
    let eng = Engine::bare(Env::new());
    let weak_engine = Rc::downgrade(&eng);
    let captured = Rc::new(RefCell::new(None));
    let inner_result = captured.clone();
    let raw = Value::JArr(Rc::new(vec![
        callback(move || {
            let eng = weak_engine.upgrade().expect("outer binding engine");
            let value = bind_at(
                &eng,
                object(vec![
                    ("first", object(vec![])),
                    ("last.key", object(vec![])),
                ]),
                &map_type(ty(RTk::Rec(rec_type(false)))),
                &[Seg::Name("inner".into()), Seg::Idx(7)],
            )?;
            *inner_result.borrow_mut() = Some(value);
            Ok(object(vec![]))
        }),
        object(vec![]),
    ]));
    let result = bind_at(
        &eng,
        raw,
        &array_type(ty(RTk::Rec(rec_type(false)))),
        &[Seg::Name("outer".into())],
    )
    .ok()
    .expect("outer binding");
    let Value::Arr(array) = result else {
        panic!("array result");
    };
    let array = array.borrow();
    assert_eq!(path_of(&array.items[0]), "outer[0]");
    assert_eq!(path_of(&array.items[1]), "outer[1]");
    let captured = captured.borrow();
    let Value::Map(map) = captured.as_ref().expect("reentrant result") else {
        panic!("map result");
    };
    let map = map.borrow();
    assert_eq!(path_of(map.get("first").unwrap()), "inner[7][\"first\"]");
    assert_eq!(
        path_of(map.get("last.key").unwrap()),
        "inner[7][\"last.key\"]"
    );
    assert!(eng.env.diagnostics_vec().is_empty());
}

#[test]
fn tainted_children_restore_the_parent_path_before_next_sibling() {
    for is_map in [false, true] {
        let eng = Engine::bare(Env::new());
        let bad = || Value::Str("not a boolean".into());
        let (raw, rt) = if is_map {
            (
                object(vec![
                    ("bad.one", bad()),
                    ("good", Value::Bool(true)),
                    ("bad.two", bad()),
                ]),
                map_type(ty(RTk::Prim("bool".into()))),
            )
        } else {
            (
                Value::JArr(Rc::new(vec![bad(), Value::Bool(true), bad()])),
                array_type(ty(RTk::Prim("bool".into()))),
            )
        };
        let result = bind_at(
            &eng,
            raw,
            &rt,
            &[
                Seg::Name("root".into()),
                Seg::Key("a.b".into()),
                Seg::Idx(3),
            ],
        )
        .ok()
        .expect("container survives tainted children");
        match result {
            Value::Arr(array) => {
                let array = array.borrow();
                assert_eq!(array.items.len(), 3);
                assert!(matches!(array.items[0], Value::Absent));
                assert!(matches!(array.items[1], Value::Bool(true)));
                assert!(matches!(array.items[2], Value::Absent));
            }
            Value::Map(map) => {
                let map = map.borrow();
                assert_eq!(map.entries.len(), 1);
                assert!(matches!(map.get("good"), Some(Value::Bool(true))));
            }
            _ => panic!("bound container"),
        }
        let diagnostics = eng.env.diagnostics_vec();
        let expected = if is_map {
            [
                "root[\"a.b\"][3][\"bad.one\"]",
                "root[\"a.b\"][3][\"bad.two\"]",
            ]
        } else {
            ["root[\"a.b\"][3][0]", "root[\"a.b\"][3][2]"]
        };
        assert_eq!(diagnostics.len(), expected.len());
        for (diagnostic, path) in diagnostics.iter().zip(expected) {
            assert_eq!(diagnostic.path, path);
            assert_eq!(diagnostic.code.as_deref(), Some("E4001"));
        }
    }
}

#[test]
fn deferred_and_failed_children_stop_and_release_temporary_parent_owners() {
    for is_map in [false, true] {
        for defer in [false, true] {
            let eng = Engine::bare(Env::new());
            let root: Rc<str> = Rc::from("root");
            let weak_root = Rc::downgrade(&root);
            let path = [Seg::Name(root)];
            let anchor = eng.retained_path(&path);
            let before = weak_root.strong_count();
            let calls = Rc::new(Cell::new(0));
            let failing_calls = calls.clone();
            let failing = callback(move || {
                failing_calls.set(failing_calls.get() + 1);
                if defer {
                    Err(Fail::Defer)
                } else {
                    err_code("stop child binding", "E5008")
                }
            });
            let later_calls = calls.clone();
            let later = callback(move || {
                later_calls.set(later_calls.get() + 100);
                Ok(Value::Bool(true))
            });
            let (raw, rt) = if is_map {
                (
                    object(vec![
                        ("first", Value::Bool(true)),
                        ("stop", failing),
                        ("later", later),
                    ]),
                    map_type(ty(RTk::Prim("bool".into()))),
                )
            } else {
                (
                    Value::JArr(Rc::new(vec![Value::Bool(true), failing, later])),
                    array_type(ty(RTk::Prim("bool".into()))),
                )
            };
            let result = bind_at(&eng, raw, &rt, &path);
            if defer {
                assert!(matches!(result, Err(Fail::Defer)));
            } else {
                let Err(Fail::Eval(error)) = result else {
                    panic!("original evaluation failure");
                };
                assert_eq!(error.code.as_deref(), Some("E5008"));
                assert_eq!(error.msg, "stop child binding");
            }
            assert_eq!(calls.get(), 1, "later siblings must not execute");
            assert_eq!(weak_root.strong_count(), before, "scratch owner released");
            assert!(eng.env.diagnostics_vec().is_empty());
            drop(anchor);
            drop(path);
            drop(eng);
            assert!(weak_root.upgrade().is_none());
        }
    }
}

#[test]
fn empty_and_rejected_maps_delay_scratch_ownership_until_an_accepted_key() {
    for accept in [false, true] {
        let eng = Engine::bare(Env::new());
        let root: Rc<str> = Rc::from("root");
        let weak_root = Rc::downgrade(&root);
        let path = [Seg::Name(root)];
        let anchor = eng.retained_path(&path);
        let before = weak_root.strong_count();
        let observed = Rc::new(RefCell::new(Vec::new()));
        let observations = observed.clone();
        let callback_root = weak_root.clone();
        let key = ty(RTk::Pred {
            base: ty(RTk::Prim("string".into())),
            preds: vec![Rc::new(Expr::Lit(Value::native(move |_| {
                observations.borrow_mut().push(callback_root.strong_count());
                Ok(Value::Bool(accept))
            })))],
        });
        let rt = ty(RTk::Map {
            key,
            val: ty(RTk::Prim("bool".into())),
        });
        let empty_map = bind_at(&eng, object(vec![]), &rt, &path)
            .ok()
            .expect("empty map");
        let empty_array = bind_at(
            &eng,
            Value::JArr(Rc::new(vec![])),
            &array_type(ty(RTk::Prim("bool".into()))),
            &path,
        )
        .ok()
        .expect("empty array");
        assert!(observed.borrow().is_empty());
        assert_eq!(weak_root.strong_count(), before);
        let result = bind_at(
            &eng,
            object(vec![
                ("first", Value::Bool(true)),
                ("second", Value::Bool(false)),
            ]),
            &rt,
            &path,
        )
        .ok()
        .expect("key validation result");
        assert_eq!(
            *observed.borrow(),
            [before, before + usize::from(accept)],
            "only an accepted key starts the parent-segment scratch lifetime"
        );
        assert_eq!(
            weak_root.strong_count(),
            before,
            "scratch released on return"
        );
        let Value::Map(map) = &result else {
            panic!("map result");
        };
        assert_eq!(map.borrow().entries.len(), if accept { 2 } else { 0 });
        let diagnostics = eng.env.diagnostics_vec();
        assert_eq!(diagnostics.len(), if accept { 0 } else { 2 });
        for diagnostic in diagnostics {
            assert_eq!(diagnostic.path, "root", "keys fail at their parent path");
        }
        drop(result);
        drop(empty_array);
        drop(empty_map);
        drop(anchor);
        drop(path);
        drop(eng);
        assert!(weak_root.upgrade().is_none());
    }
}
