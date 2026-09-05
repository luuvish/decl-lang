"""semantics (tests/internal/checks.json): type resolution, the number and
string writers, canonical paths, and the order of diagnostics."""

from __future__ import annotations

from decl.infer import type_text
from decl.parse import parse_source
from decl.semantics import Env, cmp_path, js_num_str, json_str, parse_path, path_str, sort_diags


def test_resolve_types() -> None:
    env = Env()
    env.load(
        parse_source(
            "type A = int\ntype Vec<T, N: int> = T[N]\ntype V3 = Vec<int, 3>\ntype Small = 1..10\n"
        )["decls"]
    )

    def t(name: str) -> str:
        return type_text(
            env.resolve({"k": "named", "name": name, "args": [], "preds": None, "ext": None})
        )

    assert (t("A"), t("V3"), t("Small")) == ("int", "int[3..3]", "1..10")


def test_number_text() -> None:
    want = [
        (1.0, "1"),
        (100.0, "100"),
        (2.5, "2.5"),
        (0.1 + 0.2, "0.30000000000000004"),
        (1e21, "1e+21"),
        (1e-7, "1e-7"),
        (123456789.125, "123456789.125"),
    ]
    assert [js_num_str(x) for x, _ in want] == [s for _, s in want]


def test_json_string() -> None:
    assert json_str('a"b\\c\n\t\x01é') == '"a\\"b\\\\c\\n\\t\\u0001é"'


def test_paths() -> None:
    def p(s: str) -> list:
        return parse_path(s, "r")

    segs = p('$.a.b[0]["k"]')
    assert path_str(segs) == 'r.a.b[0]["k"]'
    assert path_str(segs, "r") == '$.a.b[0]["k"]'
    assert cmp_path(p("$.a.b"), p("$.a.c")) < 0
    assert cmp_path(p("$.a[1]"), p("$.a[2]")) < 0
    assert cmp_path(p("$.a"), p("$.a.b")) < 0


def test_diag_order() -> None:
    def d(path: str, id_: str | None = None) -> dict:
        out = {"severity": "error", "message": "m", "path": path}
        if id_:
            out["id"] = id_
        return out

    sorted_ = sort_diags([d("x.b"), d(""), d("x.a", "T.z"), d("x.a", "T.a"), d("x[2]"), d("x[10]")])
    keys = " ".join(f"{x['path']}/{x.get('id', '')}" for x in sorted_)
    assert keys == "/ x[2]/ x[10]/ x.a/T.a x.a/T.z x.b/"
