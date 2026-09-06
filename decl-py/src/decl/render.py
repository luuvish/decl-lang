"""The renderer (docs/tooling/05_render.md): the form a module declares
for an output with `@render` — a format and a layout, a template, a
destination, a fan-out — read from the annotation (§3), the structured
text of a document in that form (§4), and the templates (§5) and the
fan-out (§6) that turn one evaluated root into text or files. The
command line, the REPL, the library, and the editor preview all emit
through here, so that the three implementations print the same bytes.
A port of the reference's render.ts."""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass, field
from typing import Any

from .engine import Engine
from .parse import parse_expr_text
from .semantics import (
    ABSENT,
    ArrV,
    Closure,
    EvalErr,
    Key,
    MapV,
    NatFn,
    NsRef,
    PreObj,
    Quantity,
    RecInst,
    Ref,
    Scope,
    StdRef,
    is_bool,
    is_int,
    js_num_str,
    path_str,
    read_json,
)
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


# ---------------- templates (§5) ----------------
# A template is text with tags in it: `{= expr =}` places the text form of
# a Decl expression, `{% stmt %}` is a statement, `{# … #}` a comment,
# `{% raw %}…{% endraw %}` verbatim text. The dialect is fixed here and
# implemented three times; expressions are the language's, evaluated by
# its engine over the root's document (§5.4).


class RenderError(Exception):
    """a rendering diagnostic: the code, the message, and where — `L:C` of
    the tag, or a document path; `file` is the template's path as given,
    or None for the module's"""

    def __init__(self, code: str, message: str, at: str, file: str | None = None):
        super().__init__(message)
        self.code, self.message, self.at, self.file = code, message, at, file

    def diag(self) -> dict[str, Any]:
        """the diagnostic (§12.2)"""
        return {"severity": "error", "code": self.code, "message": self.message, "path": self.at}


def _fmt_float(n: float) -> str:
    s = js_num_str(n)
    return s if ("." in s or "e" in s or "E" in s) else s + ".0"


def _pos_of(src: str, k: int) -> tuple[int, int]:
    line = 1 + src.count("\n", 0, k)
    last = src.rfind("\n", 0, k)
    return line, k - last


def _at(p: tuple[int, int]) -> str:
    return f"{p[0]}:{p[1]}"


def _lex(src: str, path: str, d: dict[str, tuple[str, str]]) -> list[Any]:
    """the lexer: text and tags, with the whitespace rules of §5.2 applied —
    trim_blocks and lstrip_blocks on for statements, `-` and `+` overriding.
    A token is a text, or ("value" | "stmt", body, position)."""
    openers = sorted(
        [(d["value"][0], "value"), (d["statement"][0], "stmt"), (d["comment"][0], "comment")],
        key=lambda o: -len(o[0]),
    )
    closer_of = {"value": d["value"][1], "stmt": d["statement"][1], "comment": d["comment"][1]}
    out: list[Any] = []
    i = 0
    text = ""
    after = "none"
    first = True

    def fail(message: str, k: int) -> RenderError:
        return RenderError("E7001", message, _at(_pos_of(src, k)), path)

    while i < len(src):
        found = next((o for o in openers if src.startswith(o[0], i)), None)
        if found is None:
            text += src[i]
            i += 1
            continue
        opener, kind = found
        start = i
        j = i + len(opener)
        left = ""
        if kind != "comment" and j < len(src) and src[j] in "-+":
            left = src[j]
            j += 1
        closer = closer_of[kind]
        end = -1
        right = ""
        k = j
        while k <= len(src) - len(closer):
            if src.startswith(closer, k):
                if kind != "comment" and k > j and src[k - 1] in "-+":
                    right = src[k - 1]
                    end = k - 1
                else:
                    end = k
                break
            k += 1
        if end < 0:
            raise fail(f"unclosed {opener} tag", start)
        body = src[j:end]
        nxt = end + len(right) + len(closer)
        # the text before the tag: `-` strips all white space, a statement's
        # default strips the indentation of its line (lstrip_blocks), `+` keeps
        before = text
        if left == "-":
            before = before.rstrip()
        elif kind == "stmt" and left != "+":
            trimmed = before.rstrip(" \t")
            if trimmed.endswith("\n") or (first and trimmed == ""):
                before = trimmed
        if before:
            out.append(before)
        text = ""
        first = False
        if kind == "comment":
            after = "none"
        elif kind == "stmt" and body.strip() == "raw":
            # verbatim text to the matching endraw, which may carry modifiers
            pat = re.compile(
                re.escape(d["statement"][0])
                + r"[-+]?\s*endraw\s*[-+]?"
                + re.escape(d["statement"][1])
            )
            m = pat.search(src, nxt)
            if m is None:
                raise fail("unclosed {% raw %}", start)
            raw = src[nxt : m.start()]
            if right == "-":
                raw = raw.lstrip()
            elif right != "+":
                raw = (
                    raw[2:] if raw.startswith("\r\n") else raw[1:] if raw.startswith("\n") else raw
                )
            end_tag = m.group(0)
            end_left = end_tag[len(d["statement"][0])]
            end_right = end_tag[len(end_tag) - len(d["statement"][1]) - 1]
            if end_left == "-":
                raw = raw.rstrip()
            elif end_left != "+":
                t = raw.rstrip(" \t")
                if t.endswith("\n"):
                    raw = t
            out.append(raw)
            nxt = m.end()
            after = "strip" if end_right == "-" else "none" if end_right == "+" else "trim"
        else:
            out.append((kind, body, _pos_of(src, start)))
            after = (
                "strip"
                if right == "-"
                else "none"
                if right == "+"
                else "trim"
                if kind == "stmt"
                else "none"
            )
        i = nxt
        # the text after the tag: `-` strips all white space, a statement's
        # default drops the line break that follows it (trim_blocks)
        if after == "strip":
            while i < len(src) and src[i].isspace():
                i += 1
        elif after == "trim":
            if src.startswith("\n", i):
                i += 1
            elif src.startswith("\r\n", i):
                i += 2
    if text:
        out.append(text)
    return out


_RE_FOR = re.compile(
    r"^([A-Za-z_][A-Za-z0-9_]*)\s*(?:,\s*([A-Za-z_][A-Za-z0-9_]*))?\s+in\s+([\s\S]+)$"
)
_RE_SET = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([\s\S]+)$")
_RE_INC = re.compile(r'^"((?:[^"\\]|\\.)*)"$')
_RE_WORD = re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*([\s\S]*?)\s*$")


class _Parser:
    """the parser: statements nest, every `if` and `for` closes"""

    def __init__(self, toks: list[Any], path: str):
        self.toks, self.k, self.path = toks, 0, path

    def fail(self, message: str, at: tuple[int, int]) -> RenderError:
        return RenderError("E7001", message, _at(at), self.path)

    def expr(self, text: str, at: tuple[int, int]) -> dict[str, Any]:
        e = parse_expr_text(text)
        if e is None:
            raise self.fail(f"expression does not parse: {text.strip()}", at)
        return e

    def iter_and_filter(self, text: str, at: tuple[int, int]) -> tuple[Any, Any]:
        """`for x in e if c`: the filter is the last top-level `if` whose two
        sides both parse; an `if` inside brackets or a string is the expression's"""
        cands: list[int] = []
        depth = 0
        quote: str | None = None
        i = 0
        while i < len(text):
            c = text[i]
            if quote:
                if c == "\\":
                    i += 1
                elif c == quote:
                    quote = None
                i += 1
                continue
            if c in "\"'`":
                quote = c
            elif c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
            elif (
                depth == 0
                and (i == 0 or text[i - 1].isspace())
                and text.startswith("if", i)
                and i + 2 < len(text)
                and text[i + 2].isspace()
            ):
                cands.append(i)
            i += 1
        for i in reversed(cands):
            a, b = parse_expr_text(text[:i]), parse_expr_text(text[i + 2 :])
            if a is not None and b is not None:
                return a, b
        return self.expr(text, at), None

    def body(self, closers: tuple[str, ...], opened: str, at: tuple[int, int]) -> tuple[Any, ...]:
        """a body up to one of the closers: (nodes, the closer, its position, its rest)"""
        nodes: list[Any] = []
        while self.k < len(self.toks):
            tok = self.toks[self.k]
            self.k += 1
            if isinstance(tok, str):
                nodes.append(("text", tok))
                continue
            kind, text, tat = tok
            if kind == "value":
                nodes.append(("value", self.expr(text, tat), tat))
                continue
            m = _RE_WORD.match(text)
            if m is None:
                raise self.fail("empty statement", tat)
            word, rest = m.group(1), m.group(2)
            if word in closers:
                return nodes, word, tat, rest
            if word == "if":
                arms: list[Any] = []
                cond: Any = self.expr(rest, tat)
                arm_at = tat
                while True:
                    b, closer, c_at, c_rest = self.body(("elif", "else", "endif"), "if", arm_at)
                    arms.append((cond, b, arm_at))
                    if closer == "endif":
                        if c_rest:
                            raise self.fail("{% endif %} takes nothing", c_at)
                        break
                    if closer == "else":
                        if c_rest:
                            raise self.fail("{% else %} takes nothing", c_at)
                        if cond is None:
                            raise self.fail("{% else %} after {% else %}", c_at)
                        cond = None
                    else:
                        if cond is None:
                            raise self.fail("{% elif %} after {% else %}", c_at)
                        cond = self.expr(c_rest, c_at)
                    arm_at = c_at
                nodes.append(("if", arms))
            elif word == "for":
                fm = _RE_FOR.match(rest)
                if fm is None:
                    raise self.fail("{% for %} expects `x in e` or `k, v in e`", tat)
                variables = [fm.group(1)] + ([fm.group(2)] if fm.group(2) else [])
                iter_, filt = self.iter_and_filter(fm.group(3), tat)
                b, closer, c_at, c_rest = self.body(("else", "endfor"), "for", tat)
                if c_rest:
                    raise self.fail(f"{{% {closer} %}} takes nothing", c_at)
                empty = None
                if closer == "else":
                    e, c2, c2_at, c2_rest = self.body(("endfor",), "for", c_at)
                    if c2_rest:
                        raise self.fail(f"{{% {c2} %}} takes nothing", c2_at)
                    empty = e
                nodes.append(("for", variables, iter_, filt, b, empty, tat))
            elif word == "set":
                sm = _RE_SET.match(rest)
                if sm is None:
                    raise self.fail("{% set %} expects `x = e`", tat)
                nodes.append(("set", sm.group(1), self.expr(sm.group(2), tat), tat))
            elif word == "include":
                im = _RE_INC.match(rest)
                if im is None:
                    raise self.fail("{% include %} expects a quoted path", tat)
                nodes.append(("include", json.loads(f'"{im.group(1)}"'), tat))
            elif word in ("elif", "else", "endif", "endfor", "endraw"):
                opener = {"endfor": "{% for %}", "endraw": "{% raw %}"}.get(word, "{% if %}")
                raise self.fail(f"{{% {word} %}} without {opener}", tat)
            else:
                raise self.fail(f"unknown tag {{% {word} %}}", tat)
        if opened:
            raise self.fail(f"unclosed {{% {opened} %}}", at)
        return nodes, "", at, ""


@dataclass
class Template:
    """a parsed template: its path as given (diagnostics), its directory (includes), its nodes"""

    path: str
    dir: str
    nodes: list[Any]


def parse_template(
    text: str,
    path: str,
    delimiters: dict[str, tuple[str, str]] | None = None,
    directory: str | None = None,
) -> Template:
    """parse a template's text (§5.2 to §5.3); E7001 for what does not parse.
    `path` is the template's path as given (its diagnostics name it);
    `directory` is where its includes resolve, the absolute directory it
    was read from"""
    d = delimiters or DEFAULT_DELIMITERS
    toks = _lex(text, path, d)
    nodes = _Parser(toks, path).body((), "", (1, 1))[0]
    return Template(
        path, directory if directory is not None else os.path.dirname(absolute(path)), nodes
    )


def absolute(p: str) -> str:
    """an absolute, normalized path, as the reference's host resolves one"""
    return os.path.normpath(os.path.abspath(p))


def resolve_in(directory: str, rel: str) -> str:
    """`dir/rel` absolute and normalized (an absolute `rel` wins)"""
    return os.path.normpath(rel) if rel.startswith("/") else absolute(os.path.join(directory, rel))


@dataclass
class Context:
    """what a template renders over (§5.4)"""

    eng: Engine
    menv: Any  # the entry module's environment: its consts and funcs are in scope
    root_name: str
    root: Any  # the root's document, bound to the root's name
    item: tuple[Any, Any] | None  # a fan-out element and its key (§6)
    read_template: Any  # (abs) -> text | None
    delimiters: dict[str, tuple[str, str]]


def text_form(eng: Engine, v: Any, root_name: str) -> str:
    """the text form of a value (§5.5); RenderError E7002 (without a place) when it has none"""
    if isinstance(v, str):
        return v
    if is_int(v) and not isinstance(v, bool):
        return str(v)
    if isinstance(v, float):
        return _fmt_float(v)
    if isinstance(v, bool):
        return "true" if v else "false"
    if v is None:
        return "null"
    if v is ABSENT:
        raise RenderError("E7002", "value has no text form: absent", "")
    if isinstance(v, (Closure, NatFn, StdRef, NsRef)):
        raise RenderError("E7002", "value has no text form: a function", "")
    if isinstance(v, Quantity):
        return f"{_fmt_float(v.value)} {eng.env.base_unit_of.get(v.dim, v.dim)}"
    if isinstance(v, Ref):
        return path_str(v.segs, root_name)
    if isinstance(v, (ArrV, MapV, RecInst)):
        return eng.serialize(v, root_name)
    raise RenderError("E7002", "value has no text form", "")


def _render_namespace(eng: Engine, root_name: str) -> PreObj:
    """the `render` namespace (§5.6): json, yaml, indent"""

    def raw(v: Any) -> Any:
        return read_json(eng.serialize(v, root_name))

    def indent_arg(a: list[Any], i: int) -> int:
        if len(a) <= i:
            return -1
        if not is_int(a[i]) or isinstance(a[i], bool) or a[i] < 0 or a[i] > 16:
            raise EvalErr("render: indent must be an integer in 0..16")
        return int(a[i])

    def json_fn(a: list[Any]) -> Any:
        if not a:
            raise EvalErr("render.json expects a value")
        return to_json(raw(a[0]), max(indent_arg(a, 1), 0))

    def yaml_fn(a: list[Any]) -> Any:
        if not a:
            raise EvalErr("render.yaml expects a value")
        n = indent_arg(a, 1)
        return to_yaml(raw(a[0]), 2 if n < 0 else n)

    def indent_fn(a: list[Any]) -> Any:
        if (
            len(a) < 2
            or not isinstance(a[0], str)
            or not is_int(a[1])
            or isinstance(a[1], bool)
            or a[1] < 0
        ):
            raise EvalErr("render.indent expects a string and a count")
        return a[0].replace("\n", "\n" + " " * int(a[1]))

    return PreObj(
        [("json", NatFn(json_fn)), ("yaml", NatFn(yaml_fn)), ("indent", NatFn(indent_fn))]
    )


def _record_entries(eng: Engine, inst: RecInst) -> list[tuple[str, Any]]:
    """the members of a record in canonical order (§7.2), as the serializer walks them"""
    out: list[tuple[str, Any]] = []
    done: set[str] = set()
    for n in inst.entry_order:
        done.add(n)
        if n in inst.extras:
            continue
        s = inst.slots.get(n)
        if s is None or s.hidden or s.state in ("invalid", "absent") or s.kind == "der":
            continue
        out.append((n, eng.access(inst, n)))
    for m in inst.rt["members"]:
        if m["name"] in done and m["kind"] != "der":
            continue
        s = inst.slots.get(m["name"])
        if s is None or s.hidden or s.state in ("invalid", "absent", "unforced"):
            continue
        out.append((m["name"], eng.access(inst, m["name"])))
    return out


def _code_of(e: EvalErr) -> str:
    """the language's code for an evaluation failure inside a template"""
    if e.code:
        return e.code
    return "E3003" if e.msg.startswith("unknown name") else "E4001"


def render_template(tpl: Template, cx: Context) -> str:
    """render a parsed template over a context (§5); a RenderError carries the diagnostic"""
    locals_: dict[str, Any] = {cx.root_name: cx.root}

    def members(v: Any) -> None:
        if isinstance(v, RecInst):
            for n, x in _record_entries(cx.eng, v):
                locals_[n] = x
            for n, s in v.slots.items():
                if s.state == "absent" and n not in locals_:
                    locals_[n] = ABSENT

    if cx.item is not None:
        locals_["item"], locals_["key"] = cx.item
        members(cx.item[0])
    else:
        members(cx.root)
    locals_["render"] = _render_namespace(cx.eng, cx.root_name)
    parsed: dict[str, Template] = {}
    stack = [resolve_in(tpl.dir, os.path.basename(tpl.path))]
    return _render_nodes(tpl, tpl.nodes, locals_, cx, parsed, stack)


def _render_nodes(
    tpl: Template,
    nodes: list[Any],
    locals_: dict[str, Any],
    cx: Context,
    parsed: dict[str, Template],
    stack: list[str],
) -> str:
    eng = cx.eng

    def scope(loc: dict[str, Any]) -> Scope:
        return Scope(None, loc, cx.root_name, cx.menv)

    def eval_at(e: Any, loc: dict[str, Any], p: tuple[int, int]) -> Any:
        sc = scope(loc)
        try:
            v = eng.ev(e, sc)
            v = eng.materialize(v, ["_"], None, sc)
            eng.force_all(v, True)
            return v
        except EvalErr as err:
            raise RenderError(_code_of(err), err.msg, _at(p), tpl.path) from None
        except RenderError as err:
            if not err.at:
                raise RenderError(err.code, err.message, _at(p), tpl.path) from None
            raise
        except Exception:
            raise RenderError("E4001", "expression cannot be evaluated", _at(p), tpl.path) from None

    def declare(name: str, loc: dict[str, Any], p: tuple[int, int]) -> None:
        if name in loc or name in cx.menv.consts or name in cx.menv.funcs:
            raise RenderError("E3019", f"{name} shadows a name in scope", _at(p), tpl.path)

    out = ""
    for n in nodes:
        t = n[0]
        if t == "text":
            out += n[1]
        elif t == "value":
            _, expr, at = n
            v = eval_at(expr, locals_, at)
            try:
                out += text_form(eng, v, cx.root_name)
            except RenderError as err:
                raise RenderError(err.code, err.message, _at(at), tpl.path) from None
        elif t == "if":
            for cond, body, at in n[1]:
                if cond is None:
                    out += _render_nodes(tpl, body, dict(locals_), cx, parsed, stack)
                    break
                c = eval_at(cond, locals_, at)
                if not isinstance(c, bool):
                    raise RenderError("E4001", "condition is not a bool", _at(at), tpl.path)
                if c:
                    out += _render_nodes(tpl, body, dict(locals_), cx, parsed, stack)
                    break
        elif t == "for":
            _, variables, iter_, filt, body, empty, at = n
            for v in variables:
                declare(v, locals_, at)
            if len(variables) == 2 and variables[0] == variables[1]:
                raise RenderError(
                    "E3019", f"{variables[0]} shadows a name in scope", _at(at), tpl.path
                )
            coll = eval_at(iter_, locals_, at)
            pairs: list[tuple[Any, Any]]
            if len(variables) == 1:
                try:
                    items = eng.iterate(coll)
                except EvalErr:
                    raise RenderError(
                        "E4001", "for over a value that is not an array", _at(at), tpl.path
                    ) from None
                pairs = [(x, None) for x in items]
            elif isinstance(coll, RecInst):
                pairs = list(_record_entries(eng, coll))
            elif isinstance(coll, MapV):
                pairs = list(coll.entries.items())
            else:
                raise RenderError(
                    "E4001",
                    "for k, v over a value that is not an object or a map",
                    _at(at),
                    tpl.path,
                )
            if filt is not None:
                kept = []
                for a, b in pairs:
                    l2 = dict(locals_)
                    l2[variables[0]] = a
                    if len(variables) == 2:
                        l2[variables[1]] = b
                    try:
                        c = eng.ev(filt, scope(l2))
                    except EvalErr as err:
                        raise RenderError(_code_of(err), err.msg, _at(at), tpl.path) from None
                    if not isinstance(c, bool):
                        raise RenderError("E4001", "filter is not a bool", _at(at), tpl.path)
                    if c:
                        kept.append((a, b))
                pairs = kept
            if not pairs:
                if empty is not None:
                    out += _render_nodes(tpl, empty, dict(locals_), cx, parsed, stack)
                continue
            for i, (a, b) in enumerate(pairs):
                l2 = dict(locals_)
                l2[variables[0]] = a
                if len(variables) == 2:
                    l2[variables[1]] = b
                l2["loop"] = PreObj(
                    [
                        ("index", i + 1),
                        ("index0", i),
                        ("first", i == 0),
                        ("last", i == len(pairs) - 1),
                        ("length", len(pairs)),
                    ]
                )
                out += _render_nodes(tpl, body, l2, cx, parsed, stack)
        elif t == "set":
            _, name, expr, at = n
            declare(name, locals_, at)
            if name == "loop":
                raise RenderError("E3019", "loop cannot be assigned", _at(at), tpl.path)
            locals_[name] = eval_at(expr, locals_, at)
        elif t == "include":
            _, path, at = n
            abs_ = resolve_in(tpl.dir, path)
            if abs_ in stack:
                chain = " -> ".join(os.path.basename(p) for p in [*stack, abs_])
                raise RenderError("E7001", f"include cycle: {chain}", _at(at), tpl.path)
            sub = parsed.get(abs_)
            if sub is None:
                text = cx.read_template(abs_)
                if text is None:
                    raise RenderError(
                        "E7003", f"template cannot be read: {path}", _at(at), tpl.path
                    )
                sub = parse_template(text, path, cx.delimiters, os.path.dirname(abs_))
                parsed[abs_] = sub
            stack.append(abs_)
            try:
                out += _render_nodes(sub, sub.nodes, dict(locals_), cx, parsed, stack)
            finally:
                stack.pop()
    return out


# ---------------- emission: one root in its form (§3, §6) ----------------


@dataclass
class Emission:
    """what emits one root: its value, its form with the invocation's
    overrides, and the template's text when there is one"""

    eng: Engine
    menv: Any
    root_name: str
    value: Any
    form: Form
    format: str | None = None  # `--format`
    indent: int | None = None  # `--indent`
    template: tuple[str, str, str] | None = None  # (path as given, text, absolute directory)
    read_template: Any = None  # (abs) -> text | None


def _fan_out_path(each: str, elem: Any, key: Any, at: str, seen: set[str]) -> str:
    """a fan-out element's file path (§6): a string, relative, `/`-separated,
    not leaving the directory, distinct — else E7005 at the element's path"""

    def e7005(m: str) -> RenderError:
        return RenderError("E7005", m, at)

    if each == "$key":
        if not isinstance(key, str):
            raise e7005("fan-out path: $key names no key (the root is an array)")
        p: Any = key
    else:
        if not isinstance(elem, RecInst) or each not in elem.slots:
            raise e7005(f"fan-out path: the element has no member {each}")
        s = elem.slots[each]
        p = ABSENT if s.state == "absent" else s.value
    if not isinstance(p, str):
        raise e7005("fan-out path is not a string")
    if p == "":
        raise e7005("fan-out path is empty")
    if p.startswith("/"):
        raise e7005(f"fan-out path is absolute: {p}")
    if "\\" in p:
        raise e7005(f"fan-out path uses \\: {p}")
    if any(s in ("..", ".", "") for s in p.split("/")):
        raise e7005(f"fan-out path leaves the destination directory: {p}")
    if p in seen:
        raise e7005(f"fan-out path repeats: {p}")
    seen.add(p)
    return p


def emit_root(e: Emission) -> str | list[tuple[str, str]]:
    """emit one root (§3.1): its structured text or its template's text, as
    one text or one file per element (a list of (path, text))"""
    fmt = e.format or e.form.format
    indent = e.indent if e.indent is not None else e.form.indent

    def raw(v: Any) -> Any:
        return read_json(e.eng.serialize(v, e.root_name))

    delimiters = e.form.delimiters or DEFAULT_DELIMITERS
    tpl = (
        parse_template(e.template[1], e.template[0], delimiters, e.template[2])
        if e.template is not None
        else None
    )

    def cx(item: tuple[Any, Any] | None) -> Context:
        return Context(e.eng, e.menv, e.root_name, e.value, item, e.read_template, delimiters)

    if e.form.each is None:
        if tpl is not None:
            return render_template(tpl, cx(None))
        return layout(raw(e.value), fmt, indent)
    # fan-out: every element of the array or map to its own file
    elems: list[tuple[Any, Any, Any]]
    if isinstance(e.value, ArrV):
        elems = [(v, i, i) for i, v in enumerate(e.value.items)]
    elif isinstance(e.value, MapV):
        elems = [(v, k, Key(k)) for k, v in e.value.entries.items()]
    else:
        raise RenderError(
            "E7004", "@render: each on a root that is neither an array nor a map", e.root_name
        )
    seen: set[str] = set()
    paths = [
        _fan_out_path(e.form.each, v, k, path_str([e.root_name, seg]), seen) for v, k, seg in elems
    ]
    files: list[tuple[str, str]] = []
    for i, (v, k, _seg) in enumerate(elems):
        text = render_template(tpl, cx((v, k))) if tpl is not None else layout(raw(v), fmt, indent)
        files.append((paths[i], text))
    return files
