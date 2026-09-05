"""module (tests/internal/checks.json): the module graph — loading a
universe, and the graph's error codes."""

from __future__ import annotations

from pathlib import Path

from decl.module import load_modules

ROOT = Path(__file__).resolve().parents[3]


def test_graph() -> None:
    r = load_modules(str(ROOT / "tests/modules/basic/main.decl"))
    assert len(r["modules"]) == 3
    assert r["entry"] is not None and str(r["entry"].path).endswith("main.decl")
    assert r["diags"] == []


def test_errors() -> None:
    def code(file: str, want: str) -> bool:
        return any(
            d.get("code") == want for d in load_modules(str(ROOT / "tests/modules" / file))["diags"]
        )

    assert code("cycle/a.decl", "E3007")
    assert code("errors/not_exported.decl", "E3005")
    assert code("errors/collision.decl", "E3006")
    assert code("errors/root_a.decl", "E3018")
    assert code("errors/missing_target.decl", "E3004")
