//! engine: the engine's boundary through the single-module pipeline —
//! quantities, references, $referrers, a cycle.
use super::common::{get, read_json, text};
use decl_lang::engine::{Engine, RootSrc};
use decl_lang::parse::parse_source;
use decl_lang::pipeline::run_pipeline;
use decl_lang::qengine::qeval::{qevaluate_universe, BoundSpec};
use decl_lang::semantics::{sort_diags, Env, Scope, Seg, Value};

#[test]
fn shared_programs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let Value::JArr(cases) =
        read_json(&std::fs::read_to_string(root.join("tests/internal/programs.json")).unwrap())
            .ok()
            .unwrap()
    else {
        panic!("programs.json is a list");
    };
    for row in cases.iter() {
        let decls = parse_source(text(get(row, "source").unwrap())).decls;
        let input = text(get(row, "input").unwrap());
        let element = text(get(row, "element").unwrap());
        let Some(Value::JArr(sizes)) = get(row, "sizes") else {
            panic!("sizes is a list");
        };
        let mut first = None;
        for size in sizes.iter() {
            let Value::Int(size) = size else {
                panic!("size is an int");
            };
            let size: usize = size.to_string().parse().unwrap();
            let raw = read_json(&format!("[{}]", vec![element; size].join(",")))
                .ok()
                .unwrap();
            let reference = Env::new();
            reference.load(&decls);
            let tree = Engine::evaluate(
                &reference,
                &|eng| {
                    let ty = reference.inputs.borrow().get(input).unwrap().0.clone();
                    let rt = reference.resolve(&ty, None).unwrap();
                    eng.bind_root(
                        input,
                        RootSrc::Doc(raw.clone()),
                        &rt,
                        &Scope::new(input, None),
                    );
                    for (name, ty, expr) in reference.outputs.borrow().iter() {
                        let rt = reference.resolve(ty, None).unwrap();
                        eng.bind_root(name, RootSrc::Expr(expr), &rt, &Scope::new(name, None));
                    }
                },
                false,
            );
            tree.validate_all("");
            let env = Env::new();
            env.load(&decls);
            let (report, eng) = qevaluate_universe(
                std::slice::from_ref(&env),
                &env,
                &[BoundSpec {
                    name: input.into(),
                    raw,
                    menv: env.clone(),
                }],
            )
            .unwrap();
            assert!(report.ok, "{:?}", report.diagnostics);
            let expected: Vec<_> = reference
                .outputs
                .borrow()
                .iter()
                .map(|(name, _, _)| {
                    (
                        name.clone(),
                        tree.serialize(&reference.root(name).unwrap(), name, false),
                    )
                })
                .collect();
            assert_eq!(report.outputs, expected);
            let diagnostics = |ds: Vec<decl_lang::semantics::Diag>| {
                ds.iter().map(|d| d.to_json(None)).collect::<Vec<_>>()
            };
            assert_eq!(
                diagnostics(report.diagnostics),
                diagnostics(sort_diags(reference.diagnostics_vec()))
            );
            assert!(env.registry_snapshot().len() >= size);
            let programs = eng.programs.borrow();
            let programs = programs.as_ref().unwrap();
            let counts = (programs.compiled.get(), programs.compiled_schemas.get());
            assert!(counts.0 > 0 && counts.1 > 0);
            if let Some(first) = first {
                assert_eq!(counts, first, "size {size}");
            }
            first = Some(counts);
        }
    }
}

#[test]
fn values() {
    let q = run_pipeline(&parse_source("dimension Speed = Length / Time\nunit mps: Speed\noutput v: quantity<Speed> = 3km / 2s\n").decls);
    assert!(q.diags.is_empty(), "{:?}", q.diags);
    let v = q
        .eng
        .resolve_segs(&[Seg::Name("v".into())])
        .ok()
        .expect("v");
    assert!(
        matches!(&v, Value::Q { dim, value } if dim == "Length*Time^-1" && *value == 1500.0),
        "{v:?}"
    );
    let r = run_pipeline(&parse_source("type S = { name: string, inbound = $referrers(L, \"target\") }\ntype L = { source: ref<S>, target: ref<S> }\ntype Top = { services: S[], links: L[] }\nexport output top: Top = { services: [{ name: \"a\" }, { name: \"b\" }], links: [{ source: services[0], target: services[1] }] }\n").decls);
    assert!(r.diags.is_empty(), "{:?}", r.diags);
    let ser = r
        .eng
        .serialize(&r.env.root("top").expect("top"), "top", false);
    assert!(ser.contains("\"source\":\"$.services[0]\""), "{ser}");
    assert!(ser.contains("\"inbound\":[\"$.links[0]\"]"), "{ser}");
}

#[test]
fn cycle() {
    let p =
        run_pipeline(&parse_source("type T = { a = b, b = a }\nexport output t: T = {}\n").decls);
    assert!(
        p.diags.iter().any(|d| d.code.as_deref() == Some("E5007")),
        "{:?}",
        p.diags
    );
}

#[test]
fn reference_round_reuse() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let Ok(Value::JArr(cases)) =
        read_json(&std::fs::read_to_string(root.join("tests/internal/rounds.json")).unwrap())
    else {
        panic!("rounds.json is a list");
    };
    for row in cases.iter() {
        let file = text(get(row, "file").unwrap());
        let parsed = parse_source(&std::fs::read_to_string(root.join(file)).unwrap());
        let reference = run_pipeline(&parsed.decls);
        let env = Env::new();
        env.load(&parsed.decls);
        let (report, eng) = qevaluate_universe(std::slice::from_ref(&env), &env, &[])
            .unwrap_or_else(|e| panic!("{}", e.0));
        let ok = !reference.diags.iter().any(|d| d.severity == "error");
        let outputs: Vec<_> = if ok {
            reference
                .env
                .outputs
                .borrow()
                .iter()
                .filter_map(|(name, _, _)| {
                    reference
                        .env
                        .root(name)
                        .map(|v| (name.clone(), reference.eng.serialize(&v, name, false)))
                })
                .collect()
        } else {
            vec![]
        };
        assert_eq!(report.ok, ok, "{file}");
        assert_eq!(report.outputs, outputs, "{file}");
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|d| d.to_json(None))
                .collect::<Vec<_>>(),
            reference
                .diags
                .iter()
                .map(|d| d.to_json(None))
                .collect::<Vec<_>>(),
            "{file}"
        );
        if matches!(get(row, "reuse"), Some(Value::Bool(true))) {
            let cache = eng.round_cache.borrow();
            let cache = cache.as_ref().unwrap();
            assert!(cache.reused_rounds.get() >= 3, "{file}");
            assert!(cache.retained_records.get() >= 20, "{file}");
        }
        if matches!(get(row, "cutoff"), Some(Value::Bool(true))) {
            let revisions = eng.revisions.borrow();
            let revisions = revisions.as_ref().unwrap();
            assert!(revisions.verified_cutoffs.get() >= 3, "{file}");
            assert!(revisions.value_cutoffs.get() >= 3, "{file}");
        }
    }
}
