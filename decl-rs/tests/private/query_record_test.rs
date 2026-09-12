//! Rust-only capture guards and identity handoff through actual slot forcing.
use super::*;
use crate::parse::parse_source;
use crate::pipeline::run_pipeline;

#[test]
fn recorded_identity_respects_tracking_and_verification_depth() {
    let eng = Engine::bare(Env::new());
    eng.step("untracked", || {
        let entries = eng.query_pool().census().entries;
        assert!(eng.record_query("untracked.read").is_none());
        assert_eq!(eng.query_pool().census().entries, entries);
    });
    eng.track.set(true);
    let entries = eng.query_pool().census().entries;
    assert!(eng.record_query("without.parent").is_none());
    assert_eq!(eng.query_pool().census().entries, entries);
    eng.step("parent", || {
        eng.record("kept".into());
        let id = eng.record_query("new.read").unwrap();
        assert_eq!(id.as_str(), "new.read");
        assert_eq!(eng.record_query("new.read"), Some(id));
        eng.verify_reads(|| {
            let entries = eng.query_pool().census().entries;
            assert!(eng.record_query("verification.only").is_none());
            assert_eq!(eng.query_pool().census().entries, entries);
            eng.step("nested", || {
                assert_eq!(
                    eng.record_query("nested.read").unwrap().as_str(),
                    "nested.read"
                );
            });
        });
        assert_eq!(eng.query_stack(), ["parent"]);
    });
    assert_eq!(
        eng.query_dependencies_for("parent"),
        Some(vec!["kept".into(), "new.read".into()])
    );
    assert_eq!(
        eng.query_dependencies_for("nested"),
        Some(vec!["nested.read".into()])
    );
    assert!(eng.query_stack().is_empty());
}

#[test]
fn forcing_guards_preserve_parent_reads_and_producer_registration() {
    for state in [
        SlotState::Ok,
        SlotState::Absent,
        SlotState::Invalid,
        SlotState::Deferred,
        SlotState::Forcing,
        SlotState::Unforced,
    ] {
        let pipeline = run_pipeline(
            &parse_source("type T = { n: int = 1, twice: int = n + n }\nexport output t: T = {}\n")
                .decls,
        );
        let eng = pipeline.eng;
        let Value::Rec(inst) = eng.env.root("t").unwrap() else {
            panic!("record output");
        };
        *eng.round_cache.borrow_mut() = None;
        eng.track.set(true);
        eng.set_phase(1);
        eng.remove_query_slot("t.twice");
        inst.borrow_mut().slot_mut("twice").unwrap().state = state;
        let result = eng.step("caller", || {
            eng.record("unrelated".into());
            eng.force_slot(&inst, "twice")
        });
        assert_eq!(
            eng.query_dependencies_for("caller"),
            Some(vec!["t.twice".into(), "unrelated".into()])
        );
        assert!(eng.query_stack().is_empty());
        assert_eq!(
            eng.query_slot("t.twice").is_some(),
            state == SlotState::Unforced
        );
        match state {
            SlotState::Ok | SlotState::Unforced => {
                let value = result.ok().expect("successful force");
                assert_eq!(eng.serialize(&value, "t", false), "2");
            }
            SlotState::Absent => assert!(matches!(result, Ok(Value::Absent))),
            SlotState::Deferred => assert!(matches!(result, Err(Fail::Defer))),
            SlotState::Invalid | SlotState::Forcing => {
                assert!(matches!(result, Err(Fail::Taint)));
            }
        }
        if state == SlotState::Unforced {
            assert_eq!(
                eng.query_dependencies_for("t.twice"),
                Some(vec!["t.n".into()])
            );
        }
        if state == SlotState::Forcing {
            assert!(eng
                .env
                .diagnostics_vec()
                .iter()
                .any(|diag| diag.code.as_deref() == Some("E5007")));
        }
    }
}
