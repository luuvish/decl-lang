//! engine: the engine's boundary through the single-module pipeline —
//! quantities, references, $referrers, a cycle.
use super::common::{get, read_json, text};
use decl_lang::parse::parse_source;
use decl_lang::pipeline::run_pipeline;
use decl_lang::qengine::qeval::qevaluate_universe;
use decl_lang::semantics::{Env, Seg, Value};

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
    }
}
