"""session (tests/internal/checks.json): the operation log — apply, undo,
redo, and a new operation after undo discarding the redo tail."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from decl.semantics import path_str
from decl.session import Session, SessionError

ROOT = Path(__file__).resolve().parents[3]


def test_undo_redo() -> None:
    s = Session(str(ROOT / "tests/repl/documents/main.decl"))

    def bind(text: str) -> None:
        s.apply({"op": "bind", "name": "extra", "src": {"kind": "inline", "text": text}})

    bind('{ "port": 1, "name": "x" }')
    bound = s.document_text("extra")
    assert bound == '{"port":1,"name":"x"}'
    assert s.undo() == 1
    with pytest.raises(SessionError):  # undone: the root is invalid
        s.document_text("extra")
    assert s.redo() == 1
    assert s.document_text("extra") == bound
    s.undo()
    bind('{ "port": 2, "name": "y" }')
    assert s.redo() == 0, "a new operation after undo discards the redo tail"
    assert s.document_text("extra") == '{"port":2,"name":"y"}'


@pytest.mark.parametrize(
    "case", json.loads((ROOT / "tests/internal/edits.json").read_text()), ids=lambda c: c["name"]
)
def test_retained_edits(case: dict) -> None:
    entry = str(ROOT / "tests/internal/session_case.decl")
    overlay = {entry: case["source"]}
    retained, fresh = Session(entry, overlay), Session(entry, overlay)

    def apply(session: Session, op: dict, full: bool) -> None:
        Session.full_recompute = full
        try:
            if op["op"] == "undo":
                session.undo()
            elif op["op"] == "redo":
                session.redo()
            else:
                session.apply(op)
        finally:
            Session.full_recompute = False

    def snapshot(session: Session, full: bool):
        Session.full_recompute = full
        try:
            run = session.run()
            values = (
                [(n, run.eng.serialize(v, n)) for n, v in run.entry.env.roots.items()]
                if run.entry and run.eng
                else []
            )
            diags = [{k: v for k, v in d.items() if k != "by"} for d in run.diags]
            return run, (values, diags, run.load_diags, run.checks, run.session_checks)
        finally:
            Session.full_recompute = False

    for op in case["initial"]:
        apply(retained, op, False)
        apply(fresh, op, True)
    prior, actual = snapshot(retained, False)
    assert prior.eng is not None
    assert actual == snapshot(fresh, True)[1]
    for step in case["steps"]:
        assert prior.eng is not None and prior.entry is not None
        programs = prior.eng.programs
        inst = next(
            (r for r in prior.entry.env.registry if path_str(r.path) == step.get("retained")), None
        )
        edits = prior.eng.edits
        assert edits is not None
        verified, equal = edits.revisions.verified_cutoffs, edits.revisions.value_cutoffs
        apply(retained, step["action"], False)
        apply(fresh, step["action"], True)
        current, actual = snapshot(retained, False)
        expected = snapshot(fresh, True)[1]
        assert actual == expected, step
        assert programs is not None and current.eng is not None and programs is current.eng.programs
        if "retained" in step:
            assert inst is not None and inst in current.entry.env.registry
        if "max_prepared" in step:
            assert edits.prepared_queries <= step["max_prepared"]
        if "max_computes" in step:
            assert current.timing["recomputed"] <= step["max_computes"]
        if "min_verified" in step:
            assert edits.revisions.verified_cutoffs - verified >= step["min_verified"]
        if "min_value_cutoffs" in step:
            assert edits.revisions.value_cutoffs - equal >= step["min_value_cutoffs"]
        prior = current


def test_temporary_programs() -> None:
    case = json.loads((ROOT / "tests/internal/temporary.json").read_text())
    entry = str(ROOT / "tests/internal/temporary_case.decl")
    session = Session(entry, {entry: case["source"]})
    eng = session.run().eng
    assert eng is not None and eng.programs is not None and eng.edits is not None
    programs = eng.programs

    def counts():
        return (
            programs.compiled,
            programs.compiled_schemas,
            len(eng.env.registry),
            len(eng.slots_by_key),
            len(eng.reads),
            len(eng.edits.inputs),
        )

    initial = counts()
    for i in range(1, case["iterations"] + 1):
        for q in case["queries"]:
            result = session.evaluate_expr(q["expr"].replace("{i}", str(i)))
            if "error" in q:
                assert result["error"]["code"] == q["error"]
            else:
                assert result["value"] == str(q["factor"] * i + q["offset"])
                assert not result.get("error")
            assert eng.programs is programs and counts() == initial


def test_retained_lifetime() -> None:
    case = json.loads((ROOT / "tests/internal/temporary.json").read_text())["churn"]
    entry = str(ROOT / "tests/internal/temporary_case.decl")
    session = Session(entry, {entry: case["source"]})
    session.apply(
        {"op": "bind", "name": case["root"], "src": {"kind": "inline", "text": case["initial"]}}
    )
    session.run()
    initial = None
    for i in range(case["iterations"]):
        for kind in ("create", "remove"):
            op = {"op": "edit", "kind": kind, "path": f'{case["root"]}["item{i}"]'}
            if kind == "create":
                op["expr"] = case["item"]
            session.apply(op)
            run = session.run()
            eng = run.eng
            assert eng is not None and eng.edits is not None and eng.programs is not None
            assert not run.diags
            assert (
                eng.serialize(eng.env.roots[case["output"]], case["output"])
                == case["present" if kind == "create" else "absent"]
            )
            if kind == "remove":
                counts = (
                    eng.programs.compiled,
                    eng.programs.compiled_schemas,
                    len(eng.env.registry),
                    len(eng.slots_by_key),
                    len(eng.reads),
                    eng.edits.revisions.tracked_queries,
                    len(eng.edits.inputs),
                )
                if initial is None:
                    initial = counts
                assert counts == initial
