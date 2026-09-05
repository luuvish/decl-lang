//! semantics: type resolution, the number and string writers, canonical
//! paths, and the order of diagnostics.
use decl_lang::ast::TypeAst;
use decl_lang::infer::type_text;
use decl_lang::parse::parse_source;
use decl_lang::semantics::{
    cmp_path, js_num_str, json_str, parse_path, path_str, sort_diags, Diag, Env,
};
use std::cmp::Ordering;

#[test]
fn resolve_types() {
    let env = Env::new();
    env.load(
        &parse_source(
            "type A = int\ntype Vec<T, N: int> = T[N]\ntype V3 = Vec<int, 3>\ntype Small = 1..10\n",
        )
        .decls,
    );
    let t = |name: &str| {
        let ast = TypeAst::Named {
            name: name.into(),
            args: vec![],
            preds: None,
            ext: None,
            loc: None,
        };
        type_text(Some(&env.resolve(&ast, None).unwrap()))
    };
    assert_eq!(t("A"), "int");
    assert_eq!(t("V3"), "int[3..3]");
    assert_eq!(t("Small"), "1..10");
}

#[test]
fn number_text() {
    let want = [
        (1.0, "1"),
        (100.0, "100"),
        (2.5, "2.5"),
        (0.1 + 0.2, "0.30000000000000004"),
        (1e21, "1e+21"),
        (1e-7, "1e-7"),
        (123456789.125, "123456789.125"),
    ];
    for (x, s) in want {
        assert_eq!(js_num_str(x), s, "{x}");
    }
}

#[test]
fn json_string() {
    assert_eq!(
        json_str("a\"b\\c\n\t\u{1}é"),
        "\"a\\\"b\\\\c\\n\\t\\u0001é\""
    );
}

#[test]
fn paths() {
    let p = |s: &str| parse_path(s, "r").ok().expect("a path");
    let segs = p("$.a.b[0][\"k\"]");
    assert_eq!(path_str(&segs, None), "r.a.b[0][\"k\"]");
    assert_eq!(path_str(&segs, Some("r")), "$.a.b[0][\"k\"]");
    assert_eq!(cmp_path(&p("$.a.b"), &p("$.a.c")), Ordering::Less);
    assert_eq!(cmp_path(&p("$.a[1]"), &p("$.a[2]")), Ordering::Less);
    assert_eq!(cmp_path(&p("$.a"), &p("$.a.b")), Ordering::Less);
}

#[test]
fn diag_order() {
    let d = |path: &str, id: Option<&str>| Diag {
        severity: "error".into(),
        id: id.map(String::from),
        message: "m".into(),
        path: path.into(),
        code: None,
        loc: None,
        by: None,
    };
    let sorted = sort_diags(vec![
        d("x.b", None),
        d("", None),
        d("x.a", Some("T.z")),
        d("x.a", Some("T.a")),
        d("x[2]", None),
        d("x[10]", None),
    ]);
    let keys: Vec<String> = sorted
        .iter()
        .map(|x| format!("{}/{}", x.path, x.id.clone().unwrap_or_default()))
        .collect();
    assert_eq!(keys.join(" "), "/ x[2]/ x[10]/ x.a/T.a x.a/T.z x.b/");
}
