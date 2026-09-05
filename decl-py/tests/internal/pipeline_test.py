"""pipeline (tests/internal/checks.json): the source-level report — the
phase that decided, and what it carries."""

from __future__ import annotations

from decl.pipeline import evaluate_source


def test_report() -> None:
    parse = evaluate_source("const x = \n")
    assert parse["phase"] == "parse" and not parse["ok"] and parse["parse_errors"]
    checked = evaluate_source("type Bad = 10..3\n")
    assert checked["phase"] == "check" and not checked["ok"]
    assert any(d.get("code") == "E4011" for d in checked["checks"])
    clean = evaluate_source("export output x: int = 1\ninput y: int\n")
    assert clean["phase"] == "evaluate" and clean["ok"]
    assert clean["outputs"] == [{"name": "x", "json": "1"}]
    assert clean["inputs"] == ["y"]
