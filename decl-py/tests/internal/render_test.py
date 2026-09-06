"""render (tests/internal/checks.json): the form `@render` declares and
the layouts of a document."""

from __future__ import annotations

from decl.parse import parse_source
from decl.render import Form, declared_form, layout
from decl.semantics import read_json


def form_of(src: str) -> Form:
    return declared_form(parse_source(src)["decls"][0])


def test_declared_form() -> None:
    f = form_of(
        '@render({ format: "yaml", indent: 4, file: "out/x.yaml" })\nexport output o: int = 1\n'
    )
    assert (f.format, f.indent, f.file, f.template, f.each, f.error) == (
        "yaml",
        4,
        "out/x.yaml",
        None,
        None,
        None,
    )
    plain = form_of("export output o: int = 1\n")
    assert (plain.format, plain.indent, plain.error) == ("json", None, None)
    assert form_of("@render({ indent: 99 })\nexport output o: int = 1\n").error == (
        "@render: indent must be an integer in 0..16"
    )
    assert form_of("@render({ colour: 1 })\nexport output o: int = 1\n").error == (
        "@render: unknown key colour"
    )


def test_layout() -> None:
    raw = read_json('{"a":[1,2],"b":{}}')
    assert layout(raw, "json", 2) == '{\n  "a": [\n    1,\n    2\n  ],\n  "b": {}\n}\n'
    assert layout(raw, "yaml", None) == "a:\n  - 1\n  - 2\nb: {}\n"
    assert layout(raw, "json", 0) == '{"a":[1,2],"b":{}}\n'
