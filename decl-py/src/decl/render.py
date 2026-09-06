"""The renderer (docs/tooling/05_render.md): the form a module declares
for an output with `@render` — a format and a layout, a template, a
destination, a fan-out — read from the annotation (§3), the structured
text of a document in that form (§4), and the templates (§5) and the
fan-out (§6) that turn one evaluated root into text or files. The
command line, the REPL, the library, and the editor preview all emit
through here, so that the three implementations print the same bytes.
A port of the reference's render.ts."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from .semantics import is_bool, is_int
from .yaml import to_json, to_yaml

DEFAULT_DELIMITERS: dict[str, tuple[str, str]] = {
    "value": ("{=", "=}"),
    "statement": ("{%", "%}"),
    "comment": ("{#", "#}"),
}


@dataclass
class Form:
    """the declared form of a root (§3): what `@render` says, every key optional"""

    format: str = "json"
    indent: int | None = None
    template: str | None = None
    file: str | None = None
    each: str | None = None
    delimiters: dict[str, tuple[str, str]] | None = None
    error: str | None = field(default=None)  # the E7004 message, when the annotation is invalid


_FORM_KEYS = ("format", "indent", "template", "file", "each", "delimiters")
_MISSING = object()


def _literal(e: dict[str, Any]) -> Any:
    """a literal value in an annotation argument: a string, an integer, a
    bool, or null (a negative integer is a unary minus over a literal)"""
    if e["e"] == "lit":
        return e["v"]
    if e["e"] == "un" and e["op"] == "-" and e["x"]["e"] == "lit" and is_int(e["x"]["v"]):
        return -e["x"]["v"]
    return _MISSING


def _is_str(v: Any) -> bool:
    return isinstance(v, str)


def declared_form(decl: dict[str, Any]) -> Form:
    """the form `@render` declares on a declaration (§3), with the E7004
    message in `error` when it is invalid; a declaration without one is
    canonical JSON"""
    anns = [a for a in decl.get("annotations") or [] if a["name"] == "render"]
    if not anns:
        return Form()
    if len(anns) > 1:
        return Form(error="more than one @render")
    a = anns[0]
    if len(a["args"]) != 1 or a["args"][0]["e"] != "obj":
        return Form(error="@render takes one object literal")
    form = Form()
    seen: set[str] = set()
    for entry in a["args"][0]["entries"]:
        key, val = entry["key"], entry["val"]
        if key not in _FORM_KEYS:
            return Form(error=f"@render: unknown key {key}")
        if key in seen:
            return Form(error=f"@render: key {key} repeats")
        seen.add(key)
        lit = _literal(val)
        if key == "format":
            if lit not in ("json", "yaml"):
                return Form(error='@render: format must be "json" or "yaml"')
            form.format = lit
        elif key == "indent":
            if not is_int(lit) or is_bool(lit) or lit < 0 or lit > 16:
                return Form(error="@render: indent must be an integer in 0..16")
            form.indent = int(lit)
        elif key in ("template", "file", "each"):
            if not _is_str(lit) or lit == "":
                return Form(error=f"@render: {key} must be a non-empty string")
            setattr(form, key, lit)
        else:
            d = _delimiters(val)
            if isinstance(d, str):
                return Form(error=f"@render: {d}")
            form.delimiters = d
    return form


def _delimiters(e: dict[str, Any]) -> dict[str, tuple[str, str]] | str:
    if e["e"] != "obj":
        return "delimiters must be an object of three pairs"
    out = dict(DEFAULT_DELIMITERS)
    seen: set[str] = set()
    for entry in e["entries"]:
        key, val = entry["key"], entry["val"]
        if key not in ("value", "statement", "comment"):
            return f"delimiters: unknown key {key}"
        if key in seen:
            return f"delimiters: key {key} repeats"
        seen.add(key)
        if val["e"] != "arr" or len(val["items"]) != 2 or any(it["spread"] for it in val["items"]):
            return f"delimiters: {key} must be a pair of strings"
        pair = [_literal(it["expr"]) for it in val["items"]]
        if any(not _is_str(p) or p == "" for p in pair):
            return f"delimiters: {key} must be a pair of non-empty strings"
        out[key] = (pair[0], pair[1])
    if len({out["value"][0], out["statement"][0], out["comment"][0]}) != 3:
        return "delimiters: the three openers must differ"
    return out


def layout(raw: Any, fmt: str, indent: int | None) -> str:
    """the structured text of a document (read_json's shape) in a format and
    layout (§4), one trailing newline"""
    if fmt == "yaml":
        return to_yaml(raw, 2 if indent is None else indent) + "\n"
    return to_json(raw, 0 if indent is None else indent) + "\n"
