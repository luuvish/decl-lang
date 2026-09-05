"""fmt (tests/internal/checks.json): what the formatter keeps — comments
and the author's line structure (§2.9)."""

from __future__ import annotations

from decl.fmt import format_source


def test_structure() -> None:
    for t in (
        "// a comment\ntype T = {\n    a: int // trailing\n    b: string\n}\n",
        "const x = [1,\n    2]\n",
    ):
        assert format_source(t) == t
