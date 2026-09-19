//! Rust-only map-key binding: an accepted key needs no text value, a rejected one reports as before.
use super::*;
use crate::parse::parse_source;
use crate::pipeline::run_pipeline;
use crate::semantics::compile_pattern;

fn pattern(src: &str) -> RT {
    ty(RTk::Pattern {
        src: src.into(),
        re: compile_pattern(src).expect("pattern"),
    })
}

#[test]
fn only_string_and_matching_pattern_keys_skip_the_full_bind() {
    let string = ty(RTk::Prim("string".into()));
    for text in ["", "plain", "Any Key", "값🙂", "quote\"slash\\"] {
        assert!(key_accepts_text(&string, text));
    }
    let slug = pattern("[a-z][a-z0-9_]*");
    assert!(key_accepts_text(&slug, "ok_1"));
    // The whole key must match, as the full bind requires.
    for text in ["", "Bad", "ok-1", "ok_1 ", "9lives"] {
        assert!(!key_accepts_text(&slug, text));
    }
    // Every other key type takes the full bind, whatever the text.
    for other in [
        ty(RTk::Prim("int".into())),
        ty(RTk::Lit(Value::Str("ok_1".into()))),
        ty(RTk::Any),
    ] {
        assert!(!key_accepts_text(&other, "ok_1"));
    }
    // The shortcut and the full bind agree on every key it may answer for.
    let eng = Engine::bare(Env::new());
    let scope = Scope::new("root", None);
    for (rt, text) in [(&string, "Any Key"), (&slug, "ok_1"), (&slug, "Bad")] {
        let bound = eng.bind(Value::Str(text.into()), rt, &[], None, &scope);
        assert_eq!(bound.is_ok(), key_accepts_text(rt, text));
    }
}

const MODULE: &str = r#"
type Slug = /[a-z][a-z0-9_]*/
    else error `keys are lowercase slugs`
type Plain = { [string]: int }
type Inline = { [/[a-z]+/]: int }
type Named = { [Slug]: int }
export output plain: Plain = { "Any Key": 1, "": 2, "값🙂": 3 }
export output inline: Inline = { "ok": 1, "BAD": 2, "fine": 3 }
export output named: Named = { "good_1": 1, "Bad": 2, "also_good": 3 }
"#;

#[test]
fn rejected_keys_report_as_the_full_bind_does_and_accepted_keys_bind() {
    let pipeline = run_pipeline(&parse_source(MODULE).decls);
    let keys = |root: &str| -> Vec<String> {
        let Some(Value::Map(map)) = pipeline.env.root(root) else {
            panic!("bound map root");
        };
        let map = map.borrow();
        (&map.entries).into_iter().map(|(k, _)| k.clone()).collect()
    };
    assert_eq!(keys("plain"), ["Any Key", "", "값🙂"]);
    assert_eq!(keys("inline"), ["ok", "fine"]);
    assert_eq!(keys("named"), ["good_1", "also_good"]);

    let mut reports: Vec<(String, String, String)> = pipeline
        .diags
        .iter()
        .map(|d| {
            (
                d.path.clone(),
                d.code.clone().unwrap_or_default(),
                d.message.clone(),
            )
        })
        .collect();
    reports.sort();
    assert_eq!(
        reports,
        [
            (
                "inline".to_string(),
                "E4001".to_string(),
                "does not match /[a-z]+/".to_string()
            ),
            (
                "named".to_string(),
                "E4001".to_string(),
                "keys are lowercase slugs".to_string()
            ),
        ]
    );
}
