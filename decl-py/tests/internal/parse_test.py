"""parse (tests/internal/checks.json): the parser's boundary — the AST a
text produces, its source ranges, and the document reader."""

from __future__ import annotations

import pytest

from decl.parse import parse_source
from decl.semantics import EvalErr, read_json


def test_const_binary() -> None:
    r = parse_source("const x = 1 + 2\n")
    assert not r["errors"] and len(r["decls"]) == 1
    d = r["decls"][0]
    e = d["expr"]
    assert d["d"] == "const" and d["name"] == "x"
    assert e["e"] == "bin" and e["op"] == "+"
    assert e["l"]["e"] == "lit" and e["l"]["v"] == 1
    assert e["r"]["e"] == "lit" and e["r"]["v"] == 2


def test_member_kinds() -> None:
    t = parse_source("type T = { a: int, b?: int, c?: int = 1, d = 2, e$ = 3 }\n")["decls"][0]

    def kind(m: dict) -> str:
        if m["m"] == "value":
            if not m["opt"]:
                return "required"
            return "defaulted" if m.get("dflt") is not None else "optional"
        return "hidden" if m.get("hidden") else "derived"

    ms = t["type"]["members"]
    assert [kind(m) for m in ms] == ["required", "optional", "defaulted", "derived", "hidden"]
    assert [m["name"] for m in ms] == ["a", "b", "c", "d", "e$"]


def test_decl_locs() -> None:
    r = parse_source("const a = 1\n\ntype T = {\n    x: int\n}\nexport output o: T = { x: 1 }\n")
    assert len(r["decls"]) == 3
    assert [(d["loc"]["sl"], d["loc"]["el"] >= d["loc"]["sl"]) for d in r["decls"]] == [
        (0, True),
        (2, True),
        (5, True),
    ]


def test_json_documents() -> None:
    v = read_json('{"a": [1, 2.5, "s", true, null], "n": 12345678901234567890}')
    assert [k for k, _ in v.entries] == ["a", "n"]
    a = dict(v.entries)["a"]
    assert a == [1, 2.5, "s", True, None]
    assert isinstance(a[0], int) and isinstance(a[1], float)
    n = dict(v.entries)["n"]
    assert isinstance(n, int) and n == 12345678901234567890
    with pytest.raises(EvalErr):  # trailing characters are refused
        read_json('{"a": 1} x')


def test_annotations() -> None:
    r = parse_source('@deprecated\ntype T = {\n    @doc("x")\n    a: int\n}\n')
    assert not r["errors"]
    d = r["decls"][0]
    assert [(a["name"], a["args"]) for a in d["annotations"]] == [("deprecated", [])]
    m = d["type"]["members"][0]
    assert [a["name"] for a in m["annotations"]] == ["doc"]
    (arg,) = m["annotations"][0]["args"]
    assert arg["e"] == "lit" and arg["v"] == "x"
