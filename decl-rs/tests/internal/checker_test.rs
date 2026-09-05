//! checker: the checker's boundary — codes anchored to their declaration,
//! and a clean module reporting nothing.
use decl_lang::checker::check_module;
use decl_lang::parse::parse_source;

#[test]
fn anchored() {
    let codes = |src: &str| check_module(&parse_source(src).decls, None, None);
    let bad = codes("type Bad = 10..3\n");
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].code.as_deref(), Some("E4011"));
    assert!(
        bad[0].loc.is_none(),
        "a declaration-level finding carries no range"
    );
    assert!(codes("const x = y\n")
        .iter()
        .any(|d| d.code.as_deref() == Some("E3003")
            && d.loc.map(|l| (l.sl, l.sc, l.ec)) == Some((0, 10, 11))));
    assert!(codes("type T = { a: int }\nexport output t: T = { a: 1 }\n").is_empty());
}
