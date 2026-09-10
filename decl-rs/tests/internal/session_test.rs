//! session: the operation log — apply, undo, redo, and a new operation
//! after undo discarding the redo tail.
use crate::common::root;
use decl_lang::semantics::path_str;
use decl_lang::session::{BindSource, EditKind, Mode, Op, Run, Session};
use std::collections::HashMap;
use std::rc::Rc;

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

#[test]
fn retained_edits() {
    use super::common::{get, read_json, text};
    use decl_lang::semantics::Value;
    fn field<'a>(v: &'a Value, key: &str) -> &'a str {
        text(get(v, key).unwrap())
    }
    fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
        let Some(Value::JArr(a)) = get(v, key) else {
            panic!("{key} is a list")
        };
        a
    }
    fn apply(s: &mut Session, op: &Value) {
        let action = match field(op, "op") {
            "undo" => {
                s.undo(1);
                return;
            }
            "redo" => {
                s.redo(1);
                return;
            }
            "bind" => Op::Bind {
                name: field(op, "name").into(),
                src: BindSource::Inline {
                    text: field(get(op, "src").unwrap(), "text").into(),
                },
            },
            "unbind" => Op::Unbind {
                name: field(op, "name").into(),
            },
            "edit" => Op::Edit {
                kind: match field(op, "kind") {
                    "create" => EditKind::Create,
                    "update" => EditKind::Update,
                    "remove" => EditKind::Remove,
                    _ => panic!("unknown edit"),
                },
                path: field(op, "path").into(),
                expr: get(op, "expr").map(|e| text(e).to_owned()),
            },
            _ => panic!("unknown operation"),
        };
        s.apply(action).unwrap();
    }
    fn snapshot(s: &Session) -> (Run, String) {
        let run = s.run(Mode::Full);
        let values: Vec<_> = run
            .eng
            .as_ref()
            .map(|eng| {
                eng.env
                    .roots_vec()
                    .iter()
                    .map(|(n, v)| (n.clone(), eng.serialize(v, n, false)))
                    .collect()
            })
            .unwrap_or_default();
        let diags = |ds: &[decl_lang::semantics::Diag]| {
            ds.iter().map(|d| d.to_json(None)).collect::<Vec<_>>()
        };
        let text = format!(
            "{:?}",
            (
                values,
                diags(&run.diags),
                diags(&run.load_diags),
                run.checks
                    .iter()
                    .map(|(f, d)| d.to_json(Some(f)))
                    .collect::<Vec<_>>(),
                diags(&run.session_checks)
            )
        );
        (run, text)
    }
    let Value::JArr(cases) =
        read_json(&std::fs::read_to_string(root().join("tests/internal/edits.json")).unwrap())
            .ok()
            .unwrap()
    else {
        panic!("edits.json is a list");
    };
    for case in cases.iter() {
        let label = field(case, "name");
        let entry = root().join("tests/internal/session_case.decl");
        let overlay = HashMap::from([(entry.clone(), field(case, "source").to_owned())]);
        let mut retained = Session::with_overlay(entry.to_str(), Some(&overlay));
        let mut fresh = Session::with_overlay(entry.to_str(), Some(&overlay));
        fresh.full_recompute = true;
        for op in list(case, "initial") {
            apply(&mut retained, op);
            apply(&mut fresh, op);
        }
        let (mut prior, actual) = snapshot(&retained);
        assert!(prior.eng.is_some(), "{label}");
        assert_eq!(actual, snapshot(&fresh).1, "{label} initial");
        for step in list(case, "steps") {
            let eng = prior.eng.as_ref().unwrap();
            let programs = eng.programs.borrow().clone().unwrap();
            let path = get(step, "retained").map(text);
            let inst = eng
                .env
                .registry_snapshot()
                .into_iter()
                .find(|r| Some(path_str(&r.borrow().path, None).as_str()) == path);
            let edits = eng.edits.borrow().clone().unwrap();
            let (verified, equal) = (
                edits.revisions.verified_cutoffs.get(),
                edits.revisions.value_cutoffs.get(),
            );
            apply(&mut retained, get(step, "action").unwrap());
            apply(&mut fresh, get(step, "action").unwrap());
            let (current, actual) = snapshot(&retained);
            assert_eq!(actual, snapshot(&fresh).1, "{label} {step:?}");
            let next = current.eng.as_ref().unwrap();
            assert!(Rc::ptr_eq(
                &programs,
                next.programs.borrow().as_ref().unwrap()
            ));
            if path.is_some() {
                assert!(next
                    .env
                    .registry_snapshot()
                    .iter()
                    .any(|r| Rc::ptr_eq(r, inst.as_ref().unwrap())));
            }
            let number = |key| {
                get(step, key).and_then(|v| {
                    if let Value::Int(n) = v {
                        n.to_string().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
            };
            if let Some(max) = number("max_prepared") {
                assert!(edits.prepared_queries.get() <= max);
            }
            if let Some(max) = number("max_computes") {
                assert!(
                    current.timing.recomputed.unwrap() <= max,
                    "{}",
                    current.timing.recomputed.unwrap()
                );
            }
            if let Some(min) = number("min_verified") {
                assert!(edits.revisions.verified_cutoffs.get() - verified >= min);
            }
            if let Some(min) = number("min_value_cutoffs") {
                assert!(edits.revisions.value_cutoffs.get() - equal >= min);
            }
            prior = current;
        }
    }
}

#[test]
fn temporary_programs() {
    use super::common::{get, read_json, text};
    use decl_lang::semantics::Value;
    let case =
        read_json(&std::fs::read_to_string(root().join("tests/internal/temporary.json")).unwrap())
            .ok()
            .unwrap();
    let number = |row: &Value, key: &str| match get(row, key).unwrap() {
        Value::Int(n) => n.to_string().parse::<usize>().unwrap(),
        _ => panic!("integer expected"),
    };
    let entry = root().join("tests/internal/temporary_case.decl");
    let overlay = HashMap::from([(
        entry.clone(),
        text(get(&case, "source").unwrap()).to_owned(),
    )]);
    let session = Session::with_overlay(entry.to_str(), Some(&overlay));
    let eng = session.run(Mode::Full).eng.unwrap();
    let programs = eng.programs.borrow().clone().unwrap();
    let counts = || {
        (
            programs.compiled.get(),
            programs.compiled_schemas.get(),
            eng.env.registry_snapshot().len(),
            eng.slots_by_key.borrow().len(),
            eng.reads.borrow().len(),
            eng.cached_literals(),
            eng.edits.borrow().as_ref().unwrap().cached_inputs(),
        )
    };
    let initial = counts();
    let Some(Value::JArr(queries)) = get(&case, "queries") else {
        panic!("queries is an array")
    };
    for i in 1..=number(&case, "iterations") {
        for q in queries.iter() {
            let expr = text(get(q, "expr").unwrap()).replace("{i}", &i.to_string());
            let result = session.evaluate_expr(&expr).unwrap();
            if let Some(error) = get(q, "error") {
                assert_eq!(result.error.unwrap().0.as_deref(), Some(text(error)));
            } else {
                assert_eq!(
                    result.value,
                    Some((number(q, "factor") * i + number(q, "offset")).to_string())
                );
                assert!(result.error.is_none());
            }
            assert!(Rc::ptr_eq(
                &programs,
                eng.programs.borrow().as_ref().unwrap()
            ));
            assert_eq!(counts(), initial, "{expr}");
        }
    }
}

#[test]
fn retained_lifetime() {
    use super::common::{get, read_json, text};
    use decl_lang::semantics::Value;
    let cases =
        read_json(&std::fs::read_to_string(root().join("tests/internal/temporary.json")).unwrap())
            .ok()
            .unwrap();
    let case = get(&cases, "churn").unwrap();
    let field = |key| text(get(case, key).unwrap());
    let entry = root().join("tests/internal/temporary_case.decl");
    let overlay = HashMap::from([(entry.clone(), field("source").to_owned())]);
    let mut session = Session::with_overlay(entry.to_str(), Some(&overlay));
    session
        .apply(Op::Bind {
            name: field("root").into(),
            src: BindSource::Inline {
                text: field("initial").into(),
            },
        })
        .unwrap();
    session.run(Mode::Full);
    let Some(Value::Int(iterations)) = get(case, "iterations") else {
        panic!("iterations is an integer")
    };
    let mut initial = None;
    for i in 0..iterations.to_string().parse::<usize>().unwrap() {
        for create in [true, false] {
            session
                .apply(Op::Edit {
                    kind: if create {
                        EditKind::Create
                    } else {
                        EditKind::Remove
                    },
                    path: format!("{}[\"item{i}\"]", field("root")),
                    expr: create.then(|| field("item").to_owned()),
                })
                .unwrap();
            let run = session.run(Mode::Full);
            let eng = run.eng.as_ref().unwrap();
            assert!(run.diags.is_empty(), "{:?}", run.diags);
            assert_eq!(
                eng.serialize(
                    &eng.env.root(field("output")).unwrap(),
                    field("output"),
                    false
                ),
                field(if create { "present" } else { "absent" })
            );
            if !create {
                let programs = eng.programs.borrow().clone().unwrap();
                let edits = eng.edits.borrow().clone().unwrap();
                let counts = (
                    programs.compiled.get(),
                    programs.compiled_schemas.get(),
                    eng.env.registry_snapshot().len(),
                    eng.slots_by_key.borrow().len(),
                    eng.reads.borrow().len(),
                    edits.revisions.tracked_queries(),
                    edits.cached_inputs(),
                    eng.cached_literals(),
                );
                assert_eq!(*initial.get_or_insert(counts), counts);
            }
        }
    }
}
