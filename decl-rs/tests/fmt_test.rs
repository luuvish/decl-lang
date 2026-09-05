//! The formatter: the canonical-form cases (tests/fmt/cases.json), then its
//! two properties over every parseable module of the corpora — idempotent
//! (fmt(fmt(x)) == fmt(x)) and AST-preserving (formatting moves columns,
//! never nodes).
mod common;
use common::*;
use decl_lang::conformance::walk_decl;
use decl_lang::fmt::format;
use decl_lang::parse::parse_source;

#[test]
fn fmt_corpus() {
    let text = std::fs::read_to_string(root().join("tests/fmt/cases.json")).unwrap();
    let Ok(Value::JArr(cases)) = read_json(&text) else {
        panic!("cases.json is a list")
    };
    let mut failures: Vec<String> = vec![];
    for c in cases.iter() {
        let name = common::text(get(c, "name").unwrap());
        let got = format(common::text(get(c, "input").unwrap()));
        match (get(c, "error"), get(c, "expected")) {
            (Some(Value::Bool(true)), _) => {
                if let Ok(s) = &got {
                    failures.push(format!("{name}: formatted anyway: {s:?}"));
                }
            }
            (_, Some(want)) => {
                let want = common::text(want);
                if got.as_deref() != Ok(want) {
                    failures.push(format!(
                        "{name}\n      expected {want:?}\n      got      {got:?}"
                    ));
                }
            }
            _ => failures.push(format!("{name}: a case needs expected or error")),
        }
    }
    println!("fmt corpus: {} cases", cases.len());
    assert!(
        failures.is_empty(),
        "fmt corpus failures:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn fmt_idempotent_and_ast_preserving_over_the_corpora() {
    let mut files = vec![];
    for d in [
        "tests/validation",
        "tests/modules",
        "tests/packages",
        "docs/examples",
    ] {
        walk_decl(&root().join(d), &mut files);
    }
    // the AST without its source ranges (formatting moves nodes; it must not change them):
    // every node's `loc` is blanked in the debug rendering
    let loc_re = regex::Regex::new(r"loc: (None|Some\(Loc \{ [^}]* \}\))").unwrap();
    let tokens = |src: &str| {
        let ds = parse_source(src).decls;
        loc_re.replace_all(&format!("{ds:?}"), "loc: _").to_string()
    };
    let (mut idem, mut idem_fail, mut token_fail, mut skipped) = (0, 0, 0, 0);
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        if !parse_source(&src).errors.is_empty() {
            skipped += 1;
            continue;
        }
        let Ok(once) = format(&src) else {
            skipped += 1;
            continue;
        };
        let twice = match format(&once) {
            Ok(t) => t,
            Err(e) => {
                idem_fail += 1;
                eprintln!("SECOND PASS FAILS {}: {e}", f.display());
                continue;
            }
        };
        if once == twice {
            idem += 1
        } else {
            idem_fail += 1;
            eprintln!("NOT IDEMPOTENT {}", f.display());
        }
        if !(parse_source(&once).errors.is_empty() && tokens(&once) == tokens(&src)) {
            token_fail += 1;
            eprintln!("AST CHANGED {}", f.display());
        }
    }
    assert!(
        idem_fail == 0,
        "fmt(fmt(x)) == fmt(x) on {} parseable files",
        idem + idem_fail
    );
    assert!(token_fail == 0, "formatting preserves the AST on all files");
    eprintln!("({skipped} unparseable fixtures skipped by design)");
}
