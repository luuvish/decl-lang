//! The API corpus (tests/api/): the driver's answers (examples/api_corpus.rs)
//! against the reviewed expected answers, documents compared by value.
mod common;
use common::*;

/// The API corpus (tests/api/): the driver's answers against the reviewed
/// expected answers, documents compared by value.
#[test]
fn api_corpus_matches_expected() {
    let got = parse(&api_corpus::run());
    let want = parse(
        &std::fs::read_to_string(api_corpus::repo_root().join("tests/api/expected.json")).unwrap(),
    );
    let (Value::JArr(got), Value::JArr(want)) = (&got, &want) else {
        panic!("lists")
    };
    assert_eq!(got.len(), want.len(), "every case answered");
    let mut failures = vec![];
    for (g, w) in got.iter().zip(want.iter()) {
        if !json_eq(g, w) {
            failures.push(format!(
                "{}\n      expected {}\n      got      {}",
                text(get(w, "name").unwrap()),
                &json_of(w)[..json_of(w).len().min(300)],
                &json_of(g)[..json_of(g).len().min(300)]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "api corpus failures:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn compact_emission_preserves_non_document_failure() {
    use decl_lang::engine::Engine;
    use decl_lang::render::{emit_root, Emission, Emitted, Form};
    use decl_lang::semantics::{Env, Value};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let eng = Engine::bare(Env::new());
    let read_template = |_: &str| None;
    let emission = |value, indent| Emission {
        eng: eng.clone(),
        menv: eng.env.clone(),
        root_name: "output".into(),
        value,
        form: Form::default(),
        yaml: None,
        indent,
        template: None,
        read_template: &read_template,
    };
    for indent in [None, Some(0)] {
        // The Rust API permits values that cannot be document roots. Empty
        // serialization must keep failing, rather than emit a blank document.
        for value in [Value::Absent, Value::Undef, Value::pattern("x")] {
            let request = emission(value, indent);
            let panic = catch_unwind(AssertUnwindSafe(|| emit_root(&request)))
                .expect_err("a non-document value must not emit successfully");
            let message = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str));
            assert_eq!(message, Some("canonical JSON"));
        }
        // Empty string and null are real documents, unlike empty serialization.
        for (value, expected) in [(Value::Str("".into()), "\"\"\n"), (Value::Null, "null\n")] {
            let Emitted::One(text) = emit_root(&emission(value, indent)).unwrap() else {
                panic!("a single document");
            };
            assert_eq!(text, expected);
        }
    }
}

// These checks exercise values constructed or mutated through Rust's public
// ownership API, including deferred values with observable native callbacks.
mod serialization {
    use decl_lang::engine::Engine;
    use decl_lang::parse::{parse_expr_text, parse_source};
    use decl_lang::pipeline::run_pipeline;
    use decl_lang::semantics::{
        ArrV, Diag, Env, Fail, Locals, MapV, NatFn, Num, PreValV, Scope, Seg, SlotState, Value,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn serialization_reads_mutable_slots_and_preserves_projection() {
        let source = "type T = { n: int, note?: string, doubled = n * 2, hidden$ = n + 100 }\nexport output output: T = { n: 1 }\n";
        let eng = run_pipeline(&parse_source(source).decls).eng;
        let value = eng.env.root("output").unwrap();
        assert_eq!(
            eng.serialize(&value, "output", false),
            "{\"n\":1,\"doubled\":2}"
        );
        let Value::Rec(record) = &value else {
            panic!("record output");
        };
        {
            let mut record = record.borrow_mut();
            record.slot_mut("n").unwrap().value = Value::Int(Num::from(3));
            record.slot_mut("doubled").unwrap().value = Value::Int(Num::from(9));
            let note = record.slot_mut("note").unwrap();
            note.state = SlotState::Ok;
            note.value = Value::Str("visible".into());
            record.set_extra(
                "extra",
                Value::JArr(Rc::new(vec![
                    Value::Absent,
                    Value::Undef,
                    Value::Int(Num::from(1)),
                ])),
            );
            record.entry_order_mut().push("extra".into());
        }
        assert_eq!(
            eng.serialize(&value, "output", false),
            "{\"n\":3,\"extra\":[null,null,1],\"note\":\"visible\",\"doubled\":9}"
        );
        assert_eq!(
            eng.serialize(&value, "output", true),
            "{\"n\":3,\"extra\":[null,null,1],\"note\":\"visible\"}"
        );
        record.borrow_mut().slot_mut("n").unwrap().state = SlotState::Invalid;
        assert_eq!(
            eng.serialize(&value, "output", true),
            "{\"extra\":[null,null,1],\"note\":\"visible\"}"
        );
    }

    #[test]
    fn serialization_reads_mutable_containers_and_unit_metadata() {
        let eng = Engine::bare(Env::new());
        eng.env
            .base_unit_of
            .borrow_mut()
            .insert("Time".into(), "s".into());
        let items = Rc::new(RefCell::new(ArrV {
            items: vec![
                Value::Absent,
                Value::Int(Num::from(1)),
                Value::Undef,
                Value::Null,
                Value::pattern("x"),
            ],
            path: Default::default(),
        }));
        let entries = Rc::new(RefCell::new(MapV {
            entries: Default::default(),
            path: Default::default(),
        }));
        {
            let mut map = entries.borrow_mut();
            map.set("skip".into(), Value::Absent);
            map.set("array".into(), Value::Arr(items.clone()));
            map.set("empty".into(), Value::JObj(Rc::new(vec![])));
            map.set("quantity".into(), Value::quantity("Time", 1.0));
            map.set(
                "ref".into(),
                Value::Ref(Rc::new(vec![
                    Seg::Name("output".into()),
                    Seg::Name("array".into()),
                    Seg::Idx(0),
                ])),
            );
        }
        let value = Value::Map(entries.clone());
        assert_eq!(
            eng.serialize(&value, "output", false),
            "{\"array\":[1,null],\"empty\":{},\"quantity\":{\"value\":1.0,\"unit\":\"s\"},\"ref\":\"$.array[0]\"}"
        );
        items.borrow_mut().items[1] = Value::Int(Num::from(2));
        items.borrow_mut().items.push(Value::Str("more".into()));
        entries.borrow_mut().set("empty".into(), Value::Undef);
        eng.env
            .base_unit_of
            .borrow_mut()
            .insert("Time".into(), "second".into());
        assert_eq!(
            eng.serialize(&value, "output", false),
            "{\"array\":[2,null,\"more\"],\"quantity\":{\"value\":1.0,\"unit\":\"second\"},\"ref\":\"$.array[0]\"}"
        );
    }

    #[test]
    fn raw_serialization_preserves_preval_effect_order_and_null_fallback() {
        let eng = Engine::bare(Env::new());
        eng.track.set(true);
        let calls = Rc::new(RefCell::new(Vec::new()));
        let make = |label: &'static str| {
            let eng = eng.clone();
            let calls = calls.clone();
            let function: NatFn = Rc::new(Box::new(move |_| {
                calls.borrow_mut().push(label);
                eng.env.report(Diag::error(label, "output".into(), None));
                eng.record(format!("pre:{label}"));
                match label {
                    "error" => Err(Fail::Taint),
                    "absent" => Ok(Value::Absent),
                    "first" => Ok(Value::Int(Num::from(1))),
                    _ => Ok(Value::Int(Num::from(4))),
                }
            }));
            Value::PreVal(Rc::new(PreValV {
                expr: parse_expr_text("tick()").unwrap(),
                scope: Scope::new("output", None)
                    .with_locals(Locals::new().with("tick".into(), Value::Nat(function))),
            }))
        };
        let first = make("first");
        let raw = Value::JArr(Rc::new(vec![
            first.clone(),
            make("error"),
            make("absent"),
            make("last"),
        ]));
        for _ in 0..2 {
            assert_eq!(
                eng.step("serialization", || eng.serialize(&raw, "output", false)),
                "[1,null,null,4]"
            );
            assert_eq!(
                eng.query_dependencies_for("serialization"),
                Some(vec![
                    "pre:absent".into(),
                    "pre:error".into(),
                    "pre:first".into(),
                    "pre:last".into()
                ])
            );
        }
        let expected = [
            "first", "error", "absent", "last", "first", "error", "absent", "last",
        ];
        assert_eq!(*calls.borrow(), expected);
        assert_eq!(
            eng.env
                .diagnostics_vec()
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(eng.serialize(&first, "output", false).is_empty());
        let typed = Value::Arr(Rc::new(RefCell::new(ArrV {
            items: vec![first],
            path: Default::default(),
        })));
        assert_eq!(eng.serialize(&typed, "output", false), "[]");
        assert_eq!(*calls.borrow(), expected);
    }

    #[test]
    fn serialization_preserves_public_raw_numeric_and_string_boundaries() {
        let eng = Engine::bare(Env::new());
        let value = Value::JObj(Rc::new(vec![
            (
                "same".into(),
                Value::Int(Num::parse_str("123456789012345678901234567890").unwrap()),
            ),
            (
                "same".into(),
                Value::Int(Num::parse_str("-123456789012345678901234567890").unwrap()),
            ),
            (
                "chars".into(),
                Value::Str("\"\\\0\u{1f}\n\r\t\u{8}\u{c}한글😀".into()),
            ),
            (
                "floats".into(),
                Value::JArr(Rc::new(
                    [
                        -0.0,
                        1.0,
                        1e-6,
                        1e-7,
                        1e20,
                        1e21,
                        f64::from_bits(1),
                        f64::MAX,
                    ]
                    .into_iter()
                    .map(Value::Float)
                    .collect(),
                )),
            ),
            ("empty".into(), Value::JObj(Rc::new(vec![]))),
        ]));
        assert_eq!(
            eng.serialize(&value, "output", false),
            concat!(
                "{\"same\":123456789012345678901234567890,\"same\":-123456789012345678901234567890,",
                "\"chars\":\"\\\"\\\\\\u0000\\u001f\\n\\r\\t\\b\\f한글😀\",",
                "\"floats\":[0.0,1.0,0.000001,1e-7,100000000000000000000.0,1e+21,5e-324,1.7976931348623157e+308],\"empty\":{}}"
            )
        );
    }
}

// Rust's public Rc ownership is checked separately from the shared language
// check inventory, like the other Rust-only API surfaces in this driver.
mod lifetime {
    use decl_lang::ast::TypeAst;
    use decl_lang::parse::parse_source;
    use decl_lang::semantics::Env;
    use decl_lang::session::{Mode, Session};
    use std::{collections::HashMap, path::PathBuf, rc::Rc};
    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn query_inspection_uses_owned_text_and_ordered_stack() {
        use decl_lang::engine::Engine;

        let eng = Engine::bare(Env::new());
        eng.track.set(true);
        assert_eq!(eng.query_dependencies_for("absent"), None);
        assert!(eng.query_slot("absent").is_none());
        let result = eng.step("outer", || {
            eng.record("root:source".into());
            eng.step("inner", || {
                assert_eq!(eng.query_stack(), ["outer", "inner"]);
                Err::<(), _>("ordinary error")
            })
        });
        assert_eq!(result, Err("ordinary error"));
        assert!(eng.query_stack().is_empty());
        assert_eq!(eng.query_dependencies_for("inner"), Some(Vec::new()));
        let mut detached = eng.query_dependencies_for("outer").unwrap();
        assert_eq!(detached, ["root:source"]);
        detached.clear();
        assert_eq!(
            eng.query_dependencies_for("outer"),
            Some(vec!["root:source".into()])
        );
        assert_eq!(eng.dependency_queries(), 2);
    }

    #[test]
    fn empty_assertions_preserve_graph_modes() {
        use decl_lang::pipeline::run_pipeline;
        use decl_lang::qengine::rounds::RoundCache;
        use decl_lang::semantics::{Diag, Value};

        let pipeline = run_pipeline(
            &parse_source("type T = { n: int }\nexport output t: T = { n: 1 }\n").decls,
        );
        let eng = pipeline.eng;
        let Value::Rec(inst) = eng.env.root("t").unwrap() else {
            panic!("record output");
        };
        eng.env.diag_set(vec![Diag::error(
            "existing diagnostic",
            "sentinel".into(),
            None,
        )]);
        let diagnostics = || {
            eng.env
                .diagnostics_vec()
                .iter()
                .map(|d| d.to_json(None))
                .collect::<Vec<_>>()
        };
        let before_diags = diagnostics();
        let key = "assert:t";
        for track in [false, true] {
            for cached in [false, true] {
                for prior in 0..3 {
                    // Prepare absent, empty, and populated owners using public capture APIs.
                    eng.track.set(true);
                    *eng.round_cache.borrow_mut() = Some(Rc::new(RoundCache::default()));
                    eng.step(key, || ());
                    if prior > 0 {
                        *eng.round_cache.borrow_mut() = None;
                        eng.step(key, || {
                            if prior == 2 {
                                eng.record("old.dependency".into());
                            }
                        });
                    }
                    let before = eng.query_dependencies_for(key);
                    assert_eq!(
                        before,
                        match prior {
                            0 => None,
                            1 => Some(Vec::new()),
                            _ => Some(vec!["old.dependency".into()]),
                        }
                    );
                    let expected = if !track {
                        before
                    } else if cached {
                        None
                    } else {
                        Some(Vec::new())
                    };
                    eng.track.set(track);
                    *eng.round_cache.borrow_mut() = cached.then(|| Rc::new(RoundCache::default()));
                    eng.step("outer", || {
                        eng.record("unrelated.dependency".into());
                        let stack = eng.query_stack();
                        let others = || {
                            eng.query_dependencies()
                                .into_iter()
                                .filter(|(owner, _)| owner != key)
                                .collect::<Vec<_>>()
                        };
                        let before_others = others();
                        eng.validate_inst(&inst, "t");
                        assert_eq!(eng.query_stack(), stack);
                        assert_eq!(others(), before_others);
                    });
                    assert_eq!(
                        eng.query_dependencies_for(key),
                        expected,
                        "track={track}, cached={cached}, prior={prior}"
                    );
                    assert!(eng.query_stack().is_empty());
                    assert_eq!(diagnostics(), before_diags);
                }
            }
        }
    }

    #[test]
    fn mutable_assertions_restore_reads_and_error_tagging() {
        use decl_lang::pipeline::run_pipeline;
        use decl_lang::qengine::rounds::RoundCache;
        use decl_lang::semantics::{read_json, RTk, Value};

        for cached in [false, true] {
            let source = "type T = { n: int, denominator: int, assert positive: n > 0, assert safe: n / denominator > 0 }\nexport output t: T = { n: 1, denominator: 1 }\n";
            let pipeline = run_pipeline(&parse_source(source).decls);
            assert!(pipeline.diags.is_empty());
            let eng = pipeline.eng;
            let Value::Rec(inst) = eng.env.root("t").unwrap() else {
                panic!("record output");
            };
            let rt = inst.borrow().rt.clone();
            let RTk::Rec(record) = &rt.k else {
                panic!("record type");
            };
            eng.track.set(true);
            *eng.round_cache.borrow_mut() = cached.then(|| Rc::new(RoundCache::default()));
            eng.validate_inst(&inst, "t");
            let reads = eng.query_dependencies_for("assert:t").unwrap();
            assert!(reads.iter().any(|key| key == "t.n"));
            assert!(reads.iter().any(|key| key == "t.denominator"));

            // RecType.asserts is mutable: emptiness cannot be cached permanently.
            let assertions = record.asserts.take();
            eng.validate_inst(&inst, "t");
            assert_eq!(
                eng.query_dependencies_for("assert:t"),
                if cached { None } else { Some(Vec::new()) }
            );
            record.asserts.replace(assertions);
            eng.validate_inst(&inst, "t");
            assert_eq!(eng.query_dependencies_for("assert:t"), Some(reads.clone()));
            assert!(eng.env.diagnostics_vec().is_empty());

            inst.borrow_mut().slot_mut("denominator").unwrap().value = read_json("0").ok().unwrap();
            eng.validate_inst(&inst, "t");
            let diagnostics = eng.env.diagnostics_vec();
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].code.as_deref(), Some("E5001"));
            assert_eq!(diagnostics[0].path, "t");
            assert_eq!(diagnostics[0].by.as_deref(), Some("assert:t"));
            assert_eq!(eng.query_dependencies_for("assert:t"), Some(reads));
            assert!(eng.query_stack().is_empty());
        }
    }

    #[test]
    fn untracked_query_churn_is_bounded() {
        use decl_lang::engine::Engine;

        // Direct Engine callers have no Session request boundary to sweep
        // temporary keys. Keep this check independent of explicit maintenance.
        for retain_graph in [false, true] {
            let eng = Engine::bare(Env::new());
            let expected: Vec<(String, Vec<String>)> = if retain_graph {
                eng.track.set(true);
                eng.step("owner", || eng.record("root:owner".into()));
                eng.track.set(false);
                vec![("owner".into(), vec!["root:owner".into()])]
            } else {
                Vec::new()
            };
            assert!(!eng.track.get());
            for batch in 0..8 {
                for n in batch * 1024..(batch + 1) * 1024 {
                    let key = format!("temporary:{n:08}");
                    assert_eq!(
                        eng.step(&key, || {
                            eng.record(format!("ignored:{n:08}"));
                            n
                        }),
                        n
                    );
                }
                let stats = eng.query_storage();
                let retained = if retain_graph { 2 } else { 0 };
                assert_eq!(stats["pool_live"], retained);
                assert_eq!(stats["pool_entries"], stats["pool_dead"] + retained);
                assert!(stats["pool_entries"] <= 1024);
                assert!(stats["pool_text_bytes"] <= stats["pool_entries"] * 18);
                assert_eq!(eng.query_dependencies(), expected);
            }
        }
    }

    #[test]
    fn temporary_session_queries_keep_identity_storage_bounded() {
        let entry = root().join("tests/internal/temporary_identity_case.decl");
        let source = "type Item = { n: int, doubled = n * 2 }\ninput scratch: Item = { n: 1 }\nexport output stable: Item = { n: 7 }\nexport output base: int = stable.doubled\n";
        let overlay = HashMap::from([(entry.clone(), source.into())]);
        let session = Session::with_overlay(entry.to_str(), Some(&overlay));
        let initial = session.run(Mode::Full);
        assert!(initial.load_diags.is_empty());
        assert!(initial.checks.is_empty());
        assert!(initial.session_checks.is_empty());
        assert!(initial.diags.is_empty());
        let eng = initial.eng.as_ref().unwrap();
        let dependencies = eng.query_dependencies();
        let baseline = eng.query_storage();
        let base = eng.serialize(&eng.env.root("base").unwrap(), "base", false);
        assert_eq!(base, "14");

        // Exercise both recreated scratch identities and distinct temporary
        // paths through supported Session queries, exceeding a maintenance
        // batch without explicit graph pruning or calls to internal helpers.
        for batch in 0..16 {
            for index in batch * 128..(batch + 1) * 128 {
                let item = "{\"n\":1,\"doubled\":2}";
                let (expression, expected) = if index % 2 == 0 {
                    ("scratch".to_owned(), item.to_owned())
                } else {
                    (
                        format!("{{ item_{index:08}: scratch }}"),
                        format!("{{\"item_{index:08}\":{item}}}"),
                    )
                };
                let result = session.evaluate_expr(&expression).unwrap();
                assert!(result.error.is_none(), "{expression}");
                assert!(result.diags.is_empty(), "{expression}");
                assert_eq!(
                    result.value.as_deref(),
                    Some(expected.as_str()),
                    "{expression}"
                );
            }
            let failed = session.evaluate_expr("1 / 0").unwrap();
            assert_eq!(failed.error.unwrap().0.as_deref(), Some("E5001"));
            assert_eq!(eng.query_dependencies(), dependencies);
            let stats = eng.query_storage();
            assert_eq!(stats["pool_live"], baseline["pool_live"]);
            assert!(stats["pool_entries"] <= baseline["pool_entries"] + 1024);
            assert_eq!(
                stats["pool_entries"],
                stats["pool_live"] + stats["pool_dead"]
            );
            assert_eq!(stats["read_queries"], baseline["read_queries"]);
            assert_eq!(stats["indexed_slots"], baseline["indexed_slots"]);
            assert_eq!(stats["computing_depth"], 0);
            assert_eq!(
                eng.serialize(&eng.env.root("base").unwrap(), "base", false),
                base
            );
            let hot = session.run(Mode::Full);
            assert!(Rc::ptr_eq(eng, hot.eng.as_ref().unwrap()));
            assert!(hot.diags.is_empty());
        }
    }

    #[test]
    fn large_temporary_request_releases_identities_kept_by_inflight_sweeps() {
        let entry = root().join("tests/internal/large_temporary_identity_case.decl");
        let source = "type Item = { n: int, doubled = n * 2 }\ninput scratch_many: Item[] = [{ n: n } for n in 1..2049]\nexport output stable: Item = { n: 7 }\nexport output base: int = stable.doubled\n";
        let overlay = HashMap::from([(entry.clone(), source.into())]);
        let session = Session::with_overlay(entry.to_str(), Some(&overlay));
        let initial = session.run(Mode::Full);
        assert!(initial.load_diags.is_empty());
        assert!(initial.checks.is_empty());
        assert!(initial.session_checks.is_empty());
        assert!(initial.diags.is_empty());
        let eng = initial.eng.as_ref().unwrap();
        assert!(eng.env.root("scratch_many").is_none());
        let dependencies = eng.query_dependencies();
        let baseline = eng.query_storage();

        // The demanded fallback keeps thousands of typed-record query IDs
        // live until scratch cleanup. An automatic scan inside the request
        // must not hide their eventual release from completion maintenance.
        let result = session.evaluate_expr("scratch_many").unwrap();
        assert!(result.error.is_none());
        assert!(result.diags.is_empty());
        let items: Vec<_> = (1..=2049)
            .map(|n| format!("{{\"n\":{n},\"doubled\":{}}}", n * 2))
            .collect();
        assert_eq!(result.value, Some(format!("[{}]", items.join(","))));
        assert!(eng.env.root("scratch_many").is_none());
        assert_eq!(eng.query_dependencies(), dependencies);
        let after = eng.query_storage();
        assert_eq!(after["pool_live"], baseline["pool_live"]);
        assert_eq!(after["pool_dead"], 0);
        assert_eq!(after["pool_entries"], baseline["pool_live"]);
        assert_eq!(after["indexed_slots"], baseline["indexed_slots"]);
        assert_eq!(after["computing_depth"], 0);
        assert_eq!(
            eng.serialize(&eng.env.root("base").unwrap(), "base", false),
            "14"
        );
    }

    // Rc reclamation is a Rust-specific API surface. These checks preserve live
    // values and recursive type metadata as well as releasing abandoned cycles.
    #[test]
    fn cyclic_type_lifetime() {
        use decl_lang::semantics::{collect_cycles, RTk};
        use std::rc::Rc;
        let env = Env::new();
        env.load(&parse_source("type Node = { next?: Node, n: int = 1 }\n").decls);
        let ast = TypeAst::Named {
            name: "Node".into(),
            args: vec![],
            preds: None,
            ext: None,
            loc: None,
        };
        let ty = env.resolve(&ast, None).unwrap();
        let weak_env = Rc::downgrade(&env);
        let weak_type = Rc::downgrade(&ty);
        drop(env);
        collect_cycles();
        assert!(
            weak_env.upgrade().is_some(),
            "a returned type keeps its declaring environment"
        );
        let RTk::Rec(record) = &ty.k else {
            panic!("record type")
        };
        assert_eq!(record.members.borrow().len(), 2);
        drop(ty);
        collect_cycles();
        assert!(weak_env.upgrade().is_none());
        assert!(weak_type.upgrade().is_none());
    }

    #[test]
    fn abandoned_value_cycles() {
        use decl_lang::semantics::collect_cycles;
        for source in [
        "export output result: int = 1\n",
        "type Child = { x: int }\nexport output result: Child = { x: 1 }\n",
        "type Child = { x: int }\ntype Root = { child: Child }\nexport output result: Root = { child: { x: 1 } }\n",
        "type Child = { $parent: ref<Root>, x: int = $parent.n }\ntype Root = { n: int, children: Child[] }\nexport output result: Root = { n: 7, children: [{}] }\n",
    ] {
        let entry = root().join("tests/internal/lifetime_case.decl");
        let overlay = HashMap::from([(entry.clone(),source.into())]);
        let session = Session::with_overlay(entry.to_str(),Some(&overlay));
        let run = session.run(Mode::Full);
        assert!(run.diags.is_empty(), "{source}: {:?}",run.diags);
        let eng = run.eng.as_ref().unwrap_or_else(|| panic!("{source}: {:?}", run.checks));
        let env = Rc::downgrade(&eng.env);
        let records: Vec<_> = eng.env.registry_snapshot().iter().map(Rc::downgrade).collect();
        drop(session);
        collect_cycles();
        assert!(env.upgrade().is_some(),"Run keeps its universe alive");
        drop(run);
        collect_cycles();
        assert!(env.upgrade().is_none(),"{source}");
        assert!(records.iter().all(|r| r.upgrade().is_none()),"{source}");
    }
    }

    #[test]
    fn returned_values_survive_collection() {
        use decl_lang::engine::Engine;
        use decl_lang::semantics::{collect_cycles, Value};
        let entry = root().join("tests/internal/lifetime_case.decl");
        let source = "type Child = { x: int }\ntype Root = { child: Child }\nexport output result: Root = { child: { x: 9 } }\n";
        let overlay = HashMap::from([(entry.clone(), source.into())]);
        let session = Session::with_overlay(entry.to_str(), Some(&overlay));
        let run = session.run(Mode::Full);
        let eng = run.eng.as_ref().unwrap();
        let value = eng.env.root("result").unwrap();
        let weak_env = Rc::downgrade(&eng.env);
        let before = eng.serialize(&value, "result", false);
        drop(session);
        drop(run);
        collect_cycles();
        let env = weak_env.upgrade().expect("a caller still owns a value");
        let fresh = Engine::new(env.clone());
        assert_eq!(fresh.serialize(&value, "result", false), before);
        let Value::Rec(record) = &value else {
            panic!("record")
        };
        let borrowed = record.borrow();
        collect_cycles();
        assert_eq!(borrowed.slots.len(), 1);
        drop(borrowed);
        drop(fresh);
        drop(env);
        drop(value);
        collect_cycles();
        assert!(weak_env.upgrade().is_none());
    }

    #[test]
    fn frozen_run_survives_collection() {
        use decl_lang::semantics::collect_cycles;
        let entry =
            root().join("tests/validation/relationships/valid/referrers_nested_snapshots.decl");
        let session = Session::new(entry.to_str());
        let run = session.run(Mode::Full);
        assert!(run.diags.is_empty());
        let eng = run.eng.as_ref().expect("valid reference-round fixture");
        let before: Vec<_> = eng
            .env
            .roots_vec()
            .iter()
            .map(|(n, v)| eng.serialize(v, n, false))
            .collect();
        let envs: Vec<_> = run.modules.iter().map(|m| Rc::downgrade(&m.env)).collect();
        drop(session);
        collect_cycles();
        let after: Vec<_> = eng
            .env
            .roots_vec()
            .iter()
            .map(|(n, v)| eng.serialize(v, n, false))
            .collect();
        assert_eq!(before, after);
        drop(run);
        collect_cycles();
        assert!(envs.iter().all(|e| e.upgrade().is_none()));
    }
}

// Rust's public representation API: shared immutable computation snapshots and
// container paths must remain usable independently of their original Engine.
mod representation {
    use decl_lang::engine::Engine;
    #[cfg(feature = "runtime-diagnostics")]
    use decl_lang::semantics::prefix_path_diagnostics;
    use decl_lang::semantics::{
        ty, ArrV, Compute, Env, MKind, MapV, Num, PrefixPath, RTk, RecInst, Seg, Slot, SlotState,
        Value,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn public_mutation_preserves_snapshots_and_requires_explicit_state_reset() {
        let engine = Engine::bare(Env::new());
        let record = Rc::new(RefCell::new(RecInst {
            type_name: None,
            rt: ty(RTk::Any),
            path: Rc::new(vec![Seg::Name("output".into())]),
            ps: RefCell::new(None),
            parent: None,
            slots: vec![(
                "n".into(),
                Slot {
                    kind: MKind::Der,
                    hidden: false,
                    state: SlotState::Unforced,
                    value: Value::Undef,
                    compute: Some(Rc::new(Compute::Bridge(Rc::new(|| {
                        Ok(Value::Int(Num::from(7)))
                    })))),
                },
            )],
            entry_order: Vec::new().into(),
            extras: Vec::new(),
            menv: None,
        }));
        assert!(
            matches!(engine.force_slot(&record, "n"), Ok(Value::Int(n)) if n.to_i64() == Some(7))
        );
        let snapshot = record
            .borrow()
            .slot("n")
            .unwrap()
            .computation_snapshot()
            .unwrap();
        {
            let mut borrowed = record.borrow_mut();
            let slot = borrowed.slot_mut("n").unwrap();
            *slot.computation_mut().unwrap() =
                Compute::Bridge(Rc::new(|| Ok(Value::Int(Num::from(9)))));
        }
        let Compute::Bridge(old) = &*snapshot else {
            panic!("binding snapshot remains a Bridge");
        };
        assert!(matches!(old(), Ok(Value::Int(n)) if n.to_i64() == Some(7)));
        assert!(
            matches!(engine.force_slot(&record, "n"), Ok(Value::Int(n)) if n.to_i64() == Some(7))
        );
        {
            let mut borrowed = record.borrow_mut();
            let slot = borrowed.slot_mut("n").unwrap();
            slot.state = SlotState::Unforced;
            slot.value = Value::Undef;
        }
        assert!(
            matches!(engine.force_slot(&record, "n"), Ok(Value::Int(n)) if n.to_i64() == Some(9))
        );
    }

    #[test]
    fn public_container_path_api_preserves_place_snapshot_and_engine_lifetime() {
        let engine = Engine::bare(Env::new());
        let segments = vec![Seg::Name("output".into()), Seg::Name("items".into())];
        let array = Rc::new(RefCell::new(ArrV {
            items: vec![Value::Int(Num::from(1))],
            path: engine.container_path(&segments),
        }));
        let value = Value::Arr(array.clone());
        let snapshot = array.borrow().path.clone();
        #[cfg(feature = "runtime-diagnostics")]
        let before = prefix_path_diagnostics();
        assert_eq!(snapshot.format(Some("output")), "$.items");
        assert_eq!(snapshot.iter().cloned().collect::<Vec<_>>(), segments);
        #[cfg(feature = "runtime-diagnostics")]
        assert_eq!(prefix_path_diagnostics().flat_exports, before.flat_exports);
        assert_eq!(value.place().unwrap(), segments);
        #[cfg(feature = "runtime-diagnostics")]
        assert_eq!(
            prefix_path_diagnostics().flat_exports,
            before.flat_exports + 1
        );
        array.borrow_mut().path.push(Seg::Idx(3));
        assert_eq!(snapshot.len(), 2);
        assert_eq!(value.place().unwrap().last(), Some(&Seg::Idx(3)));
        drop(engine);
        assert_eq!(snapshot.format(None), "output.items");
        assert_eq!(array.borrow().path.format(None), "output.items[3]");
    }

    #[test]
    fn map_direct_construction_and_reference_path_api_remain_usable() {
        let flat = vec![Seg::Name("output".into()), Seg::Key("a.b".into())];
        let map = Value::Map(Rc::new(RefCell::new(MapV {
            entries: Default::default(),
            path: PrefixPath::from(flat.clone()),
        })));
        assert_eq!(map.place().unwrap(), flat);
        let reference = Value::Ref(Rc::new(flat.clone()));
        assert_eq!(reference.place().unwrap(), flat);
        let Value::Map(map) = map else {
            panic!("map");
        };
        assert_eq!(map.borrow().path.format(Some("output")), "$[\"a.b\"]");
    }
}
