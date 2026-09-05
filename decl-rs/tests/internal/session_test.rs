//! session: the operation log — apply, undo, redo, and a new operation
//! after undo discarding the redo tail.
use crate::common::root;
use decl_lang::session::{BindSource, Op, Session};

#[test]
fn undo_redo() {
    let entry = root().join("tests/repl/documents/main.decl");
    let mut s = Session::new(Some(entry.to_str().unwrap()));
    let bind = |s: &mut Session, text: &str| {
        s.apply(Op::Bind {
            name: "extra".into(),
            src: BindSource::Inline { text: text.into() },
        })
        .unwrap()
    };
    bind(&mut s, "{ \"port\": 1, \"name\": \"x\" }");
    let bound = s.document_text("extra").unwrap();
    assert_eq!(bound, "{\"port\":1,\"name\":\"x\"}");
    assert_eq!(s.undo(1), 1);
    assert!(
        s.document_text("extra").is_err(),
        "undone: the root is invalid"
    );
    assert_eq!(s.redo(1), 1);
    assert_eq!(s.document_text("extra").unwrap(), bound);
    s.undo(1);
    bind(&mut s, "{ \"port\": 2, \"name\": \"y\" }");
    assert_eq!(
        s.redo(1),
        0,
        "a new operation after undo discards the redo tail"
    );
    assert_eq!(
        s.document_text("extra").unwrap(),
        "{\"port\":2,\"name\":\"y\"}"
    );
}
