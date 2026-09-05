//! parse: the parser's boundary — the AST a text produces, its source
//! ranges, and the document reader.
use decl_lang::ast::{DeclBody, Expr, MemberAst, TypeAst};
use decl_lang::parse::parse_source;
use decl_lang::semantics::{read_json, Value};
use num_bigint::BigInt;

fn int(v: &Expr, n: i64) -> bool {
    matches!(v, Expr::Lit(Value::Int(i)) if *i == BigInt::from(n))
}

#[test]
fn const_binary() {
    let r = parse_source("const x = 1 + 2\n");
    assert!(r.errors.is_empty() && r.decls.len() == 1);
    let DeclBody::Const { name, expr, .. } = &r.decls[0].body else {
        panic!("a const")
    };
    assert_eq!(name, "x");
    let Expr::Bin { op, l, r } = &**expr else {
        panic!("a binary expression")
    };
    assert!(op == "+" && int(l, 1) && int(r, 2), "{op} {l:?} {r:?}");
}

#[test]
fn member_kinds() {
    let r = parse_source("type T = { a: int, b?: int, c?: int = 1, d = 2, e$ = 3 }\n");
    let DeclBody::Type {
        ty: TypeAst::Record { members, .. },
        ..
    } = &r.decls[0].body
    else {
        panic!("a record type")
    };
    let kinds: Vec<(String, &str)> = members
        .iter()
        .map(|m| match m {
            MemberAst::Value {
                name, opt, dflt, ..
            } => (
                name.clone(),
                if !*opt {
                    "required"
                } else if dflt.is_some() {
                    "defaulted"
                } else {
                    "optional"
                },
            ),
            MemberAst::Derived { name, hidden, .. } => {
                (name.clone(), if *hidden { "hidden" } else { "derived" })
            }
            other => panic!("unexpected member {other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("a".to_string(), "required"),
            ("b".to_string(), "optional"),
            ("c".to_string(), "defaulted"),
            ("d".to_string(), "derived"),
            ("e$".to_string(), "hidden"),
        ]
    );
}

#[test]
fn decl_locs() {
    let r =
        parse_source("const a = 1\n\ntype T = {\n    x: int\n}\nexport output o: T = { x: 1 }\n");
    assert_eq!(r.decls.len(), 3);
    let lines: Vec<Option<(usize, bool)>> = r
        .decls
        .iter()
        .map(|d| d.loc.map(|l| (l.sl, l.el >= l.sl)))
        .collect();
    assert_eq!(
        lines,
        vec![Some((0, true)), Some((2, true)), Some((5, true))]
    );
}

#[test]
fn json_documents() {
    let v = read_json("{\"a\": [1, 2.5, \"s\", true, null], \"n\": 12345678901234567890}")
        .ok()
        .expect("a document");
    let Value::JObj(es) = &v else {
        panic!("an object")
    };
    assert_eq!(es[0].0, "a");
    let Value::JArr(a) = &es[0].1 else {
        panic!("an array")
    };
    assert!(matches!(&a[0], Value::Int(i) if *i == BigInt::from(1)));
    assert!(matches!(&a[1], Value::Float(f) if *f == 2.5));
    assert!(matches!(&a[2], Value::Str(s) if s == "s"));
    assert!(matches!(&a[3], Value::Bool(true)));
    assert!(matches!(&a[4], Value::Null));
    assert_eq!(es[1].0, "n");
    assert!(
        matches!(&es[1].1, Value::Int(i) if i.to_string() == "12345678901234567890"),
        "an integer beyond 2^53 is kept exactly"
    );
    assert!(
        read_json("{\"a\": 1} x").is_err(),
        "trailing characters are refused"
    );
}
