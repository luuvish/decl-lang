//! The subsumption judgment (§3.17) and structural emptiness (§3.19) over
//! the shared corpus tests/subsume/: prelude.decl declares the types,
//! cases.txt lists the judgments — the same file every implementation runs.
mod common;
use common::*;
use decl_lang::parse::parse_source;

/// The subsumption judgment (§3.17) and structural emptiness (§3.19) over
/// the shared corpus tests/subsume/: prelude.decl declares the types,
/// cases.txt lists the judgments — the same file every implementation
/// runs (decl-ts/tests/subsume_test.ts, decl-py/tests/subsume_test.py).
#[test]
fn subsume_corpus() {
    use decl_lang::ast::DeclBody;
    use decl_lang::semantics::{Env, RT};
    use decl_lang::subsume::{structurally_empty, subsumes};
    use std::rc::Rc;

    let prelude = std::fs::read_to_string(root().join("tests/subsume/prelude.decl")).unwrap();
    let parsed = parse_source(&prelude);
    assert!(
        parsed.errors.is_empty(),
        "prelude parse errors: {}",
        parsed.errors.len()
    );
    let env = Env::new();
    env.load(&parsed.decls);

    // a side of a case is a type written in the language: parsed as a
    // declaration's type and resolved in the prelude's environment
    let type_of = |env: &Rc<Env>, text: &str| -> RT {
        let r = parse_source(&format!("type __case = {text}\n"));
        match r.decls.as_slice() {
            [d] if r.errors.is_empty() => match &d.body {
                DeclBody::Type { ty, .. } => env
                    .resolve(ty, None)
                    .unwrap_or_else(|e| panic!("cannot resolve the type {text}: {e}")),
                _ => panic!("cannot parse the type: {text}"),
            },
            _ => panic!("cannot parse the type: {text}"),
        }
    };

    let line_re = regex::Regex::new(r"^(yes|no|empty|full)\s+(.+?)\s+::\s+(.+)$").unwrap();
    let (mut count, mut failures) = (0usize, Vec::new());
    for line in std::fs::read_to_string(root().join("tests/subsume/cases.txt"))
        .unwrap()
        .lines()
    {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        count += 1;
        let Some(m) = line_re.captures(t) else {
            failures.push(format!("unreadable case: {t}"));
            continue;
        };
        let (verdict, name, judgment) = (&m[1], &m[2], &m[3]);
        let ok = match verdict {
            "empty" | "full" => {
                structurally_empty(&env, &type_of(&env, judgment)) == (verdict == "empty")
            }
            _ => match judgment.split(" ⊑ ").collect::<Vec<_>>().as_slice() {
                [a, b] => {
                    subsumes(&env, &type_of(&env, a), &type_of(&env, b)) == (verdict == "yes")
                }
                _ => {
                    failures.push(format!("unreadable judgment: {judgment}"));
                    continue;
                }
            },
        };
        if !ok {
            failures.push(format!("{name} :: {judgment} (expected {verdict})"));
        }
    }
    println!("subsume corpus: {count} cases");
    assert!(
        failures.is_empty(),
        "subsume corpus failures:\n  {}",
        failures.join("\n  ")
    );
}
