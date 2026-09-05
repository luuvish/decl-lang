"""The formatter: the canonical-form cases (tests/fmt/cases.json), then its
two properties over every parseable module of the corpora — idempotent
(fmt(fmt(x)) == fmt(x)) and AST-preserving (formatting moves columns,
never nodes)."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from decl.fmt import format_source
from decl.parse import parse_source

ROOT = Path(__file__).resolve().parents[2]
CASES = json.loads((ROOT / "tests/fmt/cases.json").read_text(encoding="utf-8"))


@pytest.mark.parametrize("case", CASES, ids=[c["name"] for c in CASES])
def test_case(case: dict[str, Any]) -> None:
    if case.get("error"):
        with pytest.raises(ValueError):
            format_source(case["input"])
    else:
        assert format_source(case["input"]) == case["expected"]


def _without_loc(x: Any) -> Any:
    """the AST without its source ranges: formatting moves columns, never nodes"""
    if isinstance(x, dict):
        return {k: _without_loc(v) for k, v in x.items() if k != "loc"}
    if isinstance(x, list):
        return [_without_loc(v) for v in x]
    return x


def _tokens(src: str) -> str:
    return json.dumps(_without_loc(parse_source(src)["decls"]), default=str)


def _corpus_files() -> list[Path]:
    files: list[Path] = []
    for d in ("tests/validation", "tests/modules", "tests/packages", "docs/examples"):
        files += sorted((ROOT / d).rglob("*.decl"))
    return files


def test_idempotent_and_ast_preserving_over_the_corpora() -> None:
    failures = []
    formatted = skipped = 0
    for f in _corpus_files():
        src = f.read_text(encoding="utf-8")
        if parse_source(src)["errors"]:
            skipped += 1  # invalid-parsing fixtures
            continue
        try:
            once = format_source(src)
        except ValueError:
            skipped += 1
            continue
        rel = f.relative_to(ROOT)
        try:
            twice = format_source(once)
        except ValueError as e:
            failures.append(f"SECOND PASS FAILS {rel}: {e}")
            continue
        formatted += 1
        if once != twice:
            failures.append(f"NOT IDEMPOTENT {rel}")
        if parse_source(once)["errors"] or _tokens(once) != _tokens(src):
            failures.append(f"AST CHANGED {rel}")
    assert formatted > 0
    assert not failures, "\n".join(failures)
