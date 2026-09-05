//! engine: the engine's boundary through the single-module pipeline —
//! quantities, references, $referrers, a cycle.
use decl_lang::parse::parse_source;
use decl_lang::pipeline::run_pipeline;
use decl_lang::semantics::{Seg, Value};

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
