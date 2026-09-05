//! pipeline: the source-level report — the phase that decided, and what it
//! carries.
use decl_lang::pipeline::{evaluate_source, Phase};

#[test]
fn report() {
    let parse = evaluate_source("const x = \n");
    assert!(matches!(parse.phase, Phase::Parse) && !parse.ok && !parse.parse_errors.is_empty());
    let checked = evaluate_source("type Bad = 10..3\n");
    assert!(matches!(checked.phase, Phase::Check) && !checked.ok);
    assert!(checked
        .checks
        .iter()
        .any(|d| d.code.as_deref() == Some("E4011")));
    let clean = evaluate_source("export output x: int = 1\ninput y: int\n");
    assert!(matches!(clean.phase, Phase::Evaluate) && clean.ok);
    assert_eq!(clean.outputs, vec![("x".to_string(), "1".to_string())]);
    assert_eq!(clean.inputs, vec!["y".to_string()]);
}
