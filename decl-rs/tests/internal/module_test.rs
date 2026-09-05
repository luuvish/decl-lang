//! module: the module graph — loading a universe, and the graph's error
//! codes.
use crate::common::root;
use decl_lang::module::load_modules;

#[test]
fn graph() {
    let r = load_modules(&root().join("tests/modules/basic/main.decl"), None, None);
    assert_eq!(r.modules.len(), 3);
    assert!(r
        .entry
        .as_ref()
        .is_some_and(|e| e.path.ends_with("main.decl")));
    assert!(r.diags.is_empty(), "{:?}", r.diags);
}

#[test]
fn errors() {
    let code = |file: &str, want: &str| {
        load_modules(&root().join("tests/modules").join(file), None, None)
            .diags
            .iter()
            .any(|d| d.code.as_deref() == Some(want))
    };
    assert!(code("cycle/a.decl", "E3007"));
    assert!(code("errors/not_exported.decl", "E3005"));
    assert!(code("errors/collision.decl", "E3006"));
    assert!(code("errors/root_a.decl", "E3018"));
    assert!(code("errors/missing_target.decl", "E3004"));
}
