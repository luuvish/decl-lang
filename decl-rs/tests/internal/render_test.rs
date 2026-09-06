//! render: the form `@render` declares and the layouts of a document.
use decl_lang::parse::parse_source;
use decl_lang::render::{declared_form as form_declared, layout as layout_text, Form};
use decl_lang::semantics::read_json;

fn form_of(src: &str) -> Result<Form, String> {
    form_declared(&parse_source(src).decls[0])
}

#[test]
fn declared_form() {
    let f = form_of("@render({ format: \"yaml\", indent: 4, file: \"out/x.yaml\" })\nexport output o: int = 1\n").unwrap();
    assert!(f.yaml && f.indent == Some(4) && f.file.as_deref() == Some("out/x.yaml"));
    assert!(f.template.is_none() && f.each.is_none());
    let plain = form_of("export output o: int = 1\n").unwrap();
    assert!(!plain.yaml && plain.indent.is_none());
    assert_eq!(
        form_of("@render({ indent: 99 })\nexport output o: int = 1\n").unwrap_err(),
        "@render: indent must be an integer in 0..16"
    );
    assert_eq!(
        form_of("@render({ colour: 1 })\nexport output o: int = 1\n").unwrap_err(),
        "@render: unknown key colour"
    );
}

#[test]
fn layout() {
    let raw = read_json("{\"a\":[1,2],\"b\":{}}").ok().expect("JSON");
    assert_eq!(
        layout_text(&raw, false, Some(2)),
        "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}\n"
    );
    assert_eq!(layout_text(&raw, true, None), "a:\n  - 1\n  - 2\nb: {}\n");
    assert_eq!(
        layout_text(&raw, false, Some(0)),
        "{\"a\":[1,2],\"b\":{}}\n"
    );
}
