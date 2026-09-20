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

/// A writer that keeps every write, to count the pieces and rebuild the text.
#[derive(Default)]
struct Kept(Vec<Vec<u8>>);

impl std::io::Write for Kept {
    fn write(&mut self, piece: &[u8]) -> std::io::Result<usize> {
        self.0.push(piece.to_vec());
        Ok(piece.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A writer that accepts `room` bytes and then fails.
struct Full {
    room: usize,
}

impl std::io::Write for Full {
    fn write(&mut self, piece: &[u8]) -> std::io::Result<usize> {
        if piece.len() > self.room {
            return Err(std::io::Error::other("full"));
        }
        self.room -= piece.len();
        Ok(piece.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Records with an absent member, an open record's extras, maps, arrays, a reference and a
/// quantity-free scalar mix, `count` times over: larger than one piece from a few thousand on.
fn document(count: usize) -> String {
    format!(
        r#"
type Item = {{
    id: int
    name: string
    note?: string
    tags: string[]
    weights: {{ [string]: float }}
    label = `item-${{id}}`
    ...
}}
type Doc = {{ items: Item[], first: ref<Item>, index: {{ [string]: int }} }}
export output doc: Doc = {{
    items: [{{ extra: [i, "x"], id: i, name: `n${{i}}`, tags: ["a", `b\n${{i}}`], weights: {{ "w": 0.5, "v": 2.0 }} }} for i in 0..<{count}]
    first: items[0]
    index: {{ `k${{i}}`: i for i in 0..<{count} }}
}}
"#
    )
}

#[test]
fn text_handed_to_a_writer_in_pieces_is_the_text_built_whole() {
    for count in [1, 40, 6000] {
        let pipeline = run_pipeline(&parse_source(&document(count)).decls);
        assert!(pipeline.diags.is_empty(), "{:?}", pipeline.diags);
        let root = pipeline.env.root("doc").expect("root");
        for settable_only in [false, true] {
            let whole = pipeline.eng.serialize(&root, "doc", settable_only);
            let mut kept = Kept::default();
            assert!(pipeline
                .eng
                .serialize_to(&root, "doc", settable_only, &mut kept)
                .expect("written"));
            assert_eq!(kept.0.concat(), whole.as_bytes());
            // Every piece but the last is complete text of at least the piece size, and a
            // document beyond that size is never handed over whole.
            let (last, full) = kept.0.split_last().expect("at least one write");
            assert!(full.iter().all(|piece| piece.len() >= Pieces::PIECE));
            assert!(full.iter().all(|piece| std::str::from_utf8(piece).is_ok()));
            assert_eq!(whole.len() > 2 * Pieces::PIECE, kept.0.len() > 2);
            assert!(last.len() < 2 * Pieces::PIECE || full.is_empty());
            assert!(
                count < 6000 || kept.0.len() >= 3,
                "the large document drains more than once"
            );
        }
    }
}

#[test]
fn a_value_without_text_writes_nothing_and_a_failing_writer_is_reported() {
    let pipeline = run_pipeline(&parse_source(&document(6000)).decls);
    let root = pipeline.env.root("doc").expect("root");
    let mut kept = Kept::default();
    assert!(!pipeline
        .eng
        .serialize_to(&Value::Absent, "doc", false, &mut kept)
        .expect("nothing to write"));
    assert!(kept.0.concat().is_empty());
    // The first piece fits, a later one does not: the error comes back and the walk still ends.
    let mut full = Full {
        room: Pieces::PIECE + Pieces::PIECE / 2,
    };
    assert!(pipeline
        .eng
        .serialize_to(&root, "doc", false, &mut full)
        .is_err());
}
