//! infer: the inference boundary — the type text of literals, a quantity,
//! an array, and range arithmetic (D31).
use decl_lang::ast::DeclBody;
use decl_lang::infer::{infer, make_ctx, type_text};
use decl_lang::parse::parse_source;
use decl_lang::semantics::Env;
use std::rc::Rc;

#[test]
fn expressions() {
    let env = Env::new();
    env.load(&parse_source("type Small = 1..10\nconst a: Small = 1\nconst b: Small = 2\n").decls);
    let cx = make_ctx(env.clone(), Rc::new(|_: &str, _: String| {}));
    let ty = |src: &str| -> String {
        let r = parse_source(&format!("const z = {src}\n"));
        let DeclBody::Const { expr, .. } = &r.decls[0].body else {
            panic!("a const")
        };
        type_text(infer(&cx, expr).rt.as_ref())
    };
    let want = [
        ("1", "1"),
        ("1.5", "1.5"),
        ("\"s\"", "\"s\""),
        ("true", "true"),
        ("null", "null"),
        ("1km", "quantity<Length>"),
        ("[1, 2]", "(1 | 2)[]"),
        ("a + b", "2..20"),
    ];
    for (src, t) in want {
        assert_eq!(ty(src), t, "{src}");
    }
}
