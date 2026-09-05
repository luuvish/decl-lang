"""checker (tests/internal/checks.json): the checker's boundary — codes
anchored to their declaration, and a clean module reporting nothing."""

from __future__ import annotations

from decl.checker import check_module
from decl.parse import parse_source


def test_anchored() -> None:
    def codes(src: str) -> list:
        return check_module(parse_source(src)["decls"])

    bad = codes("type Bad = 10..3\n")
    assert len(bad) == 1 and bad[0]["code"] == "E4011"
    assert "loc" not in bad[0], "a declaration-level finding carries no range"
    assert any(
        d["code"] == "E3003" and (d["loc"]["sl"], d["loc"]["sc"], d["loc"]["ec"]) == (0, 10, 11)
        for d in codes("const x = y\n")
    )
    assert codes("type T = { a: int }\nexport output t: T = { a: 1 }\n") == []
