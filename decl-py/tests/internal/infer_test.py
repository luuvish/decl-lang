"""infer (tests/internal/checks.json): the inference boundary — the type
text of literals, a quantity, an array, and range arithmetic (D31)."""

from __future__ import annotations

from decl.infer import infer, make_ctx, type_text
from decl.parse import parse_source
from decl.semantics import Env


def test_expressions() -> None:
    env = Env()
    env.load(parse_source("type Small = 1..10\nconst a: Small = 1\nconst b: Small = 2\n")["decls"])
    cx = make_ctx(env, lambda _c, _m: None)

    def ty(src: str) -> str:
        return type_text(infer(cx, parse_source(f"const z = {src}\n")["decls"][0]["expr"])["rt"])

    want = [
        ("1", "1"),
        ("1.5", "1.5"),
        ('"s"', '"s"'),
        ("true", "true"),
        ("null", "null"),
        ("1km", "quantity<Length>"),
        ("[1, 2]", "(1 | 2)[]"),
        ("a + b", "2..20"),
    ]
    assert [ty(src) for src, _ in want] == [t for _, t in want]
