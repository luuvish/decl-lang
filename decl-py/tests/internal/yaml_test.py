"""yaml (tests/internal/checks.json): the YAML reader's core schema and
refusals, the writer's plain-string rule, and the round trip."""

from __future__ import annotations

from decl.semantics import read_json
from decl.yaml import YamlError, plain_safe, read_yaml, to_json, to_yaml


def refusal(src: str) -> str:
    try:
        read_yaml(src)
    except YamlError as e:
        return str(e)
    return ""


def test_core_schema() -> None:
    v = read_yaml('a: 1\nb: 2.5\nc: yes\nd: 0x1F\ne: ~\nf: "12"\ng: [x, {h: true}]\n')
    assert to_json(v) == '{"a":1,"b":2.5,"c":"yes","d":31,"e":null,"f":"12","g":["x",{"h":true}]}'
    assert isinstance(v.entries[0][1], int) and isinstance(v.entries[1][1], float)


def test_refused() -> None:
    assert refusal("a: !!str 1\n") == "uses a tag at line 1"
    assert refusal("1: x\n") == "mapping key is not a string at line 1"
    assert refusal("a: 1\na: 2\n") == 'mapping repeats the key "a" at line 2'
    assert refusal("a: 1\n---\nb: 2\n") == "stream holds more than one document at line 2"


def test_plain_strings() -> None:
    assert all(plain_safe(s) for s in ("my-service", "with space", "a_b"))
    assert not any(
        plain_safe(s) for s in ("yes", "n", "true", "12", "1e3", "a: b", "-x", "", "x #y")
    )


def test_round_trip() -> None:
    doc = '{"name":"s","xs":[{"a":1,"b":[]},2.0],"m":{},"q":{"value":3000.0,"unit":"m"}}'
    raw = read_json(doc)
    y = to_yaml(raw, 2)
    assert y == "name: s\nxs:\n  - a: 1\n    b: []\n  - 2.0\nm: {}\nq:\n  value: 3000.0\n  unit: m"
    assert to_json(read_yaml(y)) == doc
    assert to_json(read_json(to_json(raw, 2))) == doc
