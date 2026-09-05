"""The public Python API (decl/__init__.py): evaluate, check, validate,
format — the command line's vocabulary as functions."""

from __future__ import annotations

from pathlib import Path

import pytest

import decl

SCHEMA = """\
type Service = { name: string, port?: 1..65535 = 8080 }
input svc: Service
export output out: Service = svc
"""


@pytest.fixture
def module(tmp_path: Path) -> Path:
    p = tmp_path / "svc.decl"
    p.write_text(SCHEMA)
    return p


def test_evaluate_example_outputs(root: Path) -> None:
    outputs = decl.evaluate(root / "docs/examples/02_config.decl")
    assert list(outputs) == ["base", "prod", "dev"]
    assert outputs["prod"]["host"] == "api.internal"


def test_evaluate_binds_an_input_value(module: Path) -> None:
    out = decl.evaluate(module, inputs={"svc": {"name": "a"}}, outputs=["out"])
    assert out == {"out": {"name": "a", "port": 8080}}


def test_evaluate_binds_an_input_file(module: Path, tmp_path: Path) -> None:
    doc = tmp_path / "svc.json"
    doc.write_text('{"name": "b", "port": 9000}')
    # a string names a file; any other value is the document itself
    out = decl.evaluate(module, inputs={"svc": str(doc)}, outputs=["out"])
    assert out["out"] == {"name": "b", "port": 9000}


def test_unknown_root_is_a_decl_error(module: Path) -> None:
    with pytest.raises(decl.DeclError, match="no root named nope"):
        decl.evaluate(module, inputs={"svc": {"name": "a"}}, outputs=["nope"])


def test_invalid_document_reports_diagnostics(module: Path) -> None:
    with pytest.raises(decl.DeclError) as info:
        decl.evaluate(module, inputs={"svc": {"name": "a", "port": 70000}}, outputs=["out"])
    codes = {d["code"] for d in info.value.diagnostics}
    assert codes, info.value.diagnostics
    assert all(d["severity"] == "error" for d in info.value.diagnostics)


def test_check_is_clean_for_the_examples(root: Path) -> None:
    assert decl.check(root / "docs/examples/01_interconnect.decl") == []


def test_check_reports_a_static_error(tmp_path: Path) -> None:
    bad = tmp_path / "bad.decl"
    bad.write_text("type Bad = 10..3\n")
    diags = decl.check(bad)
    assert diags and diags[0]["severity"] == "error"


def test_validate_accepts_a_value(module: Path) -> None:
    assert decl.validate(module, inputs={"svc": {"name": "a"}}) == []


def test_format_source_is_canonical_and_idempotent() -> None:
    once = decl.format_source("const x=1+2\n")
    assert once == "const x = 1 + 2\n"
    assert decl.format_source(once) == once
