"""conformance (tests/internal/checks.json): the fixture judge and the
corpus walk it rests on."""

from __future__ import annotations

from pathlib import Path

from decl.conformance import judge_fixture, walk_decl

ROOT = Path(__file__).resolve().parents[3]


def test_judge() -> None:
    files = list(walk_decl(str(ROOT / "tests/modules")))
    assert len(files) == 11 and files == sorted(files)
    assert any(f.endswith("tests/modules/basic/main.decl") for f in files)
    valid = judge_fixture(str(ROOT / "tests/validation/types/valid/predicates.decl"), True)
    wrong = judge_fixture(str(ROOT / "tests/validation/types/invalid/empty_range.decl"), True)
    right = judge_fixture(str(ROOT / "tests/validation/types/invalid/empty_range.decl"), False)
    assert valid["ok"], valid["detail"]
    assert not wrong["ok"] and "E4011" in wrong["detail"], wrong["detail"]
    assert right["ok"], right["detail"]
