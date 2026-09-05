//! fmt: what the formatter keeps — comments and the author's line
//! structure (§2.9).
use decl_lang::fmt::format;

#[test]
fn structure() {
    for t in [
        "// a comment\ntype T = {\n    a: int // trailing\n    b: string\n}\n",
        "const x = [1,\n    2]\n",
    ] {
        assert_eq!(format(t).as_deref(), Ok(t));
    }
}
