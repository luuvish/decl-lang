//! Native path segments can contain delimiters that are not prefix boundaries.
use super::*;

fn prefixes(segments: &[Seg], expected: &[&str]) -> String {
    let (text, ends) = path_str_prefixes(segments);
    assert_eq!(ends.len(), segments.len());
    assert!(ends.iter().all(|&end| text.is_char_boundary(end)));
    assert_eq!(
        ends.iter().map(|&end| &text[..end]).collect::<Vec<_>>(),
        expected
    );
    text
}

#[test]
fn prefix_boundaries_follow_segments_instead_of_text_delimiters() {
    assert_eq!(prefixes(&[], &[]), "");
    let bare = prefixes(
        &[Seg::Name("r.a".into()), Seg::Name("b".into())],
        &["r.a", "r.a.b"],
    );
    let nested = prefixes(
        &[
            Seg::Name("r".into()),
            Seg::Name("a".into()),
            Seg::Name("b".into()),
        ],
        &["r", "r.a", "r.a.b"],
    );
    assert_eq!(bare, nested, "identical text can have different prefixes");
    prefixes(
        &[Seg::Key("r[0]".into()), Seg::Idx(3)],
        &["r[0]", "r[0][3]"],
    );
    for empty in [Seg::Name("".into()), Seg::Key("".into())] {
        prefixes(&[empty, Seg::Name("x".into())], &["", ".x"]);
    }
    prefixes(&[Seg::Idx(12), Seg::Key("".into())], &["12", "12[\"\"]"]);
}

#[test]
fn prefix_boundaries_preserve_unicode_and_json_escaping() {
    for quoted in [Seg::Name("a\"\\\n".into()), Seg::Key("a\"\\\n".into())] {
        prefixes(
            &[Seg::Name("한글".into()), quoted, Seg::Idx(2)],
            &["한글", r#"한글["a\"\\\n"]"#, r#"한글["a\"\\\n"][2]"#],
        );
    }
    prefixes(
        &[Seg::Key("root.🌱".into()), Seg::Key("é".into())],
        &["root.🌱", "root.🌱[\"é\"]"],
    );
    assert_eq!(
        path_str_iter([&Seg::Idx(12), &Seg::Name("x".into())], Some("12")),
        "$.x"
    );
    assert_eq!(
        path_str_iter([&Seg::Key("r.a".into()), &Seg::Idx(0)], Some("r.a")),
        "$[0]"
    );
}
