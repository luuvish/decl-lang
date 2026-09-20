//! Rust-only record emission: the supplied names are scanned when few and indexed when many,
//! and either way a record is emitted as before.
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
    for width in [3, Supplied::SCANNED, Supplied::SCANNED + 1, 40] {
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
