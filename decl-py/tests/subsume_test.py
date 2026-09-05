"""The subsumption judgment (§3.17) and structural emptiness (§3.19) over
the shared corpus tests/subsume/: prelude.decl declares the types,
cases.txt lists the judgments — the same file every implementation runs."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

import pytest

from decl.parse import parse_source
from decl.semantics import Env
from decl.subsume import structurally_empty, subsumes

ROOT = Path(__file__).resolve().parents[2]
LINE = re.compile(r"^(yes|no|empty|full)\s+(.+?)\s+::\s+(.+)$")


def _cases() -> list[tuple[str, str, str]]:
    out = []
    for line in (ROOT / "tests/subsume/cases.txt").read_text(encoding="utf-8").split("\n"):
        t = line.strip()
        if not t or t.startswith("#"):
            continue
        m = LINE.match(t)
        assert m, f"unreadable case: {t}"
        out.append((m[1], m[2], m[3]))
    return out


CASES = _cases()


@pytest.fixture(scope="module")
def env() -> Env:
    parsed = parse_source((ROOT / "tests/subsume/prelude.decl").read_text(encoding="utf-8"))
    assert not parsed["errors"], "prelude parse errors"
    e = Env()
    e.load(parsed["decls"])
    return e


def type_of(env: Env, text: str) -> Any:
    """a side of a case is a type written in the language: parsed as a
    declaration's type and resolved in the prelude's environment"""
    r = parse_source(f"type __case = {text}\n")
    assert not r["errors"] and len(r["decls"]) == 1 and r["decls"][0]["d"] == "type", (
        f"cannot parse the type: {text}"
    )
    return env.resolve(r["decls"][0]["type"])


@pytest.mark.parametrize(("verdict", "name", "judgment"), CASES, ids=[c[1] for c in CASES])
def test_case(verdict: str, name: str, judgment: str, env: Env) -> None:
    if verdict in ("empty", "full"):
        assert structurally_empty(env, type_of(env, judgment)) == (verdict == "empty")
        return
    sides = judgment.split(" ⊑ ")
    assert len(sides) == 2, f"unreadable judgment: {judgment}"
    a, b = (type_of(env, x) for x in sides)
    assert subsumes(env, a, b) == (verdict == "yes")
