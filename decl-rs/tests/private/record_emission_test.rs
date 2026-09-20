//! Rust-only record emission: a record whose slots follow its members is walked by position,
//! any other by name with the supplied names scanned when few and indexed when many; either
//! way a record is emitted as before.
use super::*;
use crate::parse::parse_source;
use crate::pipeline::run_pipeline;

#[test]
fn supplied_names_answer_alike_on_both_sides_of_the_scan_limit() {
    for count in [0, 1, Supplied::SCANNED, Supplied::SCANNED + 1, 40] {
        let order: Vec<String> = (0..count).map(|i| format!("m{i}")).collect();
        let supplied = Supplied::new(&order);
        assert_eq!(supplied.index.is_some(), count > Supplied::SCANNED);
        for name in &order {
            assert!(supplied.has(name));
        }
        for absent in ["", "m", "m-1", "M0", "m40"] {
            assert!(!supplied.has(absent));
        }
    }
}

/// A record of `width` members supplied in reverse document order, with a derived member last.
fn module(width: usize) -> String {
    let members: String = (0..width).map(|i| format!("    m{i}: int\n")).collect();
    let entries: Vec<String> = (0..width).rev().map(|i| format!("m{i}: {i}")).collect();
    format!(
        "type T = {{\n{members}    total = m0 + 1\n}}\nexport output t: T = {{ {} }}\n",
        entries.join(", ")
    )
}

#[test]
fn narrow_and_wide_records_emit_supplied_members_in_document_order_then_the_rest() {
    // 64 members is the last width walked by position; 65 and 70 are walked by name.
    for width in [3, Supplied::SCANNED, Supplied::SCANNED + 1, 40, 64, 65, 70] {
        let pipeline = run_pipeline(&parse_source(&module(width)).decls);
        assert!(pipeline.diags.is_empty());
        let root = pipeline.env.root("t").expect("root");
        let supplied: Vec<String> = (0..width).rev().map(|i| format!("\"m{i}\":{i}")).collect();
        let expected = format!("{{{},\"total\":1}}", supplied.join(","));
        assert_eq!(pipeline.eng.serialize(&root, "t", false), expected);
        // Settable members only: the derived one is left out.
        let settable = format!("{{{}}}", supplied.join(","));
        assert_eq!(pipeline.eng.serialize(&root, "t", true), settable);
    }
}

#[test]
fn supplied_order_extras_and_absent_members_survive_the_walk_by_position() {
    // Document order differs from declaration order, an open record carries extras between its
    // members, an optional member is absent, and a defaulted one is filled after the supplied.
    let source = r#"
type T = {
    a: int
    b?: string
    c: int = 7
    d: bool
    twice = a * 2
    ...
}
export output t: T = { z: 1, d: true, y: "s", a: 2 }
"#;
    let pipeline = run_pipeline(&parse_source(source).decls);
    assert!(pipeline.diags.is_empty());
    let root = pipeline.env.root("t").expect("root");
    assert_eq!(
        pipeline.eng.serialize(&root, "t", false),
        r#"{"z":1,"d":true,"y":"s","a":2,"c":7,"twice":4}"#
    );
    // Settable members only: what was supplied, with neither the default nor the derived member.
    assert_eq!(
        pipeline.eng.serialize(&root, "t", true),
        r#"{"z":1,"d":true,"y":"s","a":2}"#
    );
}
