"""The API corpus (tests/api/) through the Python API: every case's answer
(scripts/api_corpus.py) against tests/api/expected.json, documents
compared by value; then the surface that exists in Python alone."""

from __future__ import annotations

import importlib.metadata
import importlib.util
import json
from pathlib import Path

import pytest

import decl

ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("api_corpus", ROOT / "decl-py/scripts/api_corpus.py")
assert _spec and _spec.loader
api_corpus = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(api_corpus)

EXPECTED = json.loads((ROOT / "tests/api/expected.json").read_text(encoding="utf-8"))
ANSWERS = api_corpus.run_all()


@pytest.mark.parametrize("i", range(len(EXPECTED)), ids=[e["name"] for e in EXPECTED])
def test_case(i: int) -> None:
    assert i < len(ANSWERS), "fewer answers than expected"
    # canonical text: key order counts, as the parity harness counts it
    assert json.dumps(ANSWERS[i]) == json.dumps(EXPECTED[i])


def test_every_case_answered() -> None:
    assert len(ANSWERS) == len(EXPECTED)


# ---- the Python-only surface: path-like arguments, iterable inputs, format_file, __version__


def test_path_like_and_pairs(tmp_path: Path) -> None:
    module = tmp_path / "svc.decl"
    module.write_text(
        "type Service = { name: string, port?: 1..65535 = 8080 }\n"
        "input svc: Service\nexport output out: Service = svc\n"
    )
    out = decl.evaluate(module, inputs=[("svc", {"name": "a"})], outputs=["out"])
    assert out == {"out": {"name": "a", "port": 8080}}
    assert decl.check(module) == []


def test_validate_expect_errors(tmp_path: Path) -> None:
    module = tmp_path / "cfg.decl"
    module.write_text("type Cfg = { port: 1..65535, ... }\ninput deployed: Cfg\n")
    diags = decl.validate(module, inputs={"deployed": {"port": 70000}}, expect_errors=["E4001"])
    assert [d["code"] for d in diags] == ["E4001"]
    with pytest.raises(decl.DeclError):
        decl.validate(module, inputs={"deployed": {"port": 70000}}, expect_errors=["E9999"])


def test_format_file(tmp_path: Path) -> None:
    f = tmp_path / "x.decl"
    f.write_text("const x=1+2\n")
    assert decl.format_file(f, check=True) is True
    assert f.read_text() == "const x=1+2\n"
    assert decl.format_file(f) is True
    assert f.read_text() == "const x = 1 + 2\n"
    assert decl.format_file(f) is False


def test_version() -> None:
    assert decl.__version__ == importlib.metadata.version("decl-lang")
