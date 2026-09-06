"""decl-lsp (docs/tooling/03_lsp.md): the language server over stdio — a
port of the reference implementation's lsp.ts. Every answer comes from the
same checker, inference, and engine as the command line, driven through
the session object (session.py) with the open buffers overriding the disk;
positions come from the source ranges every AST node carries, and the
types and resolutions recorded while the checker runs (infer.py hooks).
Messages are handled strictly in order; the server exits when its stdin
closes."""

from __future__ import annotations

import contextlib
import json
import os
import re
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any, cast
from urllib.parse import unquote, urlparse

from tree_sitter import Parser

from ._tree_sitter import LANGUAGE
from .checker import check_module
from .fmt import format_source, u16
from .infer import STD, _std_path, js_str, resolve_in, type_text
from .parse import parse_source
from .semantics import ArrV, MapV, RecInst, js_num_str, parse_path, seg_text
from .session import Session, SessionError, expr_text, fmt_diag

_out: Any = None


# ---------------- transport ----------------
def send(msg: dict[str, Any]) -> None:
    body = json.dumps(msg, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    _out.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body)
    _out.flush()


def reply(id_: Any, result: Any) -> None:
    send({"jsonrpc": "2.0", "id": id_, "result": result})


def notify(method: str, params: Any) -> None:
    send({"jsonrpc": "2.0", "method": method, "params": params})


def log_err(message: str) -> None:
    notify("window/logMessage", {"type": 1, "message": message})


# ---------------- documents ----------------
docs: dict[str, Any] = {}  # uri -> text
overlay: dict[str, Any] = {}  # path -> text (open buffers override the disk)
config: dict[str, Any] = {"inputs": {}}


def path_of(uri: str) -> str:
    return unquote(urlparse(uri).path)


def uri_of(path: str) -> str:
    return Path(path).as_uri()


# positions: the AST's columns are tree-sitter's (bytes); the protocol's
# are UTF-16 units. Every range leaves in units, every position arrives
# in units and is turned into bytes against the line it is on.
def _u16_col(line: str, byte_col: int) -> int:
    return u16(line.encode("utf-8")[:byte_col].decode("utf-8", "replace"))


def _byte_col(line: str, u16_col: int) -> int:
    units = 0
    for i, ch in enumerate(line):
        if units >= u16_col:
            return len(line[:i].encode("utf-8"))
        units += u16(ch)
    return len(line.encode("utf-8"))


def _indent_width(s: str) -> int:
    return len(s) - len(s.lstrip(" "))


def _line(lines: list[Any], i: int) -> str:
    return lines[i] if 0 <= i < len(lines) else ""


def range_of(loc: dict[str, Any], lines: list[Any]) -> dict[str, Any]:
    return {
        "start": {"line": loc["sl"], "character": _u16_col(_line(lines, loc["sl"]), loc["sc"])},
        "end": {"line": loc["el"], "character": _u16_col(_line(lines, loc["el"]), loc["ec"])},
    }


def pos_bytes(pos: dict[str, Any], lines: list[Any]) -> dict[str, Any]:
    return {
        "line": pos["line"],
        "character": _byte_col(_line(lines, pos["line"]), pos["character"]),
    }


def contains(loc: dict[str, Any], p: dict[str, Any]) -> bool:
    return (loc["sl"] < p["line"] or (loc["sl"] == p["line"] and loc["sc"] <= p["character"])) and (
        p["line"] < loc["el"] or (p["line"] == loc["el"] and p["character"] <= loc["ec"])
    )


def span(loc: dict[str, Any]) -> int:
    return (loc["el"] - loc["sl"]) * 100000 + (loc["ec"] - loc["sc"])


def _bslice(line: str, a: int, b: int) -> str:
    return line.encode("utf-8")[a:b].decode("utf-8", "replace")


def _bfind(line: str, name: str, start: int) -> int:
    return line.encode("utf-8").find(name.encode("utf-8"), start)


def _brfind(line: str, name: str, upto: int) -> int:
    """JavaScript's lastIndexOf(name, upto): the last occurrence starting at or before `upto`"""
    b = name.encode("utf-8")
    return line.encode("utf-8").rfind(b, 0, upto + len(b))


# ---------------- analysis ----------------
# one analysis per open document: its universe (the document as entry),
# and for every module the checker's tables — the type of every
# expression and what every name denotes (keyed by node identity)
class Analysis:
    __slots__ = ("path", "run", "session", "tables", "text")

    def __init__(self, path: str, text: str, session: Session, run: Any) -> None:
        self.path, self.text, self.session, self.run = path, text, session, run
        self.tables: dict[str, Any] = {}


analyses: dict[str, Any] = {}
last_good: dict[
    str, Any
] = {}  # the last analysis of a document that parsed (completion while typing)


def analysis_of(uri: str) -> Analysis | None:
    text = docs.get(uri)
    if text is None:
        return None
    have = analyses.get(uri)
    if have is not None and have.text == text:
        return have
    path = path_of(uri)
    if parse_source(text)["errors"]:
        return None
    session = Session(path, overlay)
    run = with_progress(
        f"Decl: evaluating {path.split('/')[-1]}", lambda: session.run(None, "full")
    )
    a = Analysis(path, text, session, run)
    analyses[uri] = a
    last_good[uri] = a
    return a


def tables_of(a: Analysis, m: Any) -> dict[str, Any]:
    have = a.tables.get(m.path)
    if have is not None:
        return have
    t: dict[str, Any] = {"types": {}, "res": {}}
    check_module(
        m.decls,
        m.env,
        {
            "record": lambda e, ty: t["types"].__setitem__(id(e), ty),
            "resolve_hook": lambda e, target: t["res"].__setitem__(id(e), target),
        },
    )
    a.tables[m.path] = t
    return t


def module_of(a: Analysis, path: str) -> Any:
    return next((m for m in a.run.modules if m.path == path), None)


def read_text(path: str) -> str:
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return ""


def text_of(a: Analysis, m: Any) -> str:
    return overlay[m.path] if m.path in overlay else read_text(m.path)


# ---------------- diagnostics ----------------
_SKIP = ("error", "in", "the", "a", "is", "not", "std", "module", "import", "type", "name")


def anchor_for(src: str, message: str) -> dict[str, Any]:
    lines = src.split("\n")
    for n in re.findall(r"[A-Za-z_][A-Za-z0-9_.]*", message):
        if n in _SKIP:
            continue
        pat = re.compile(r"\b" + re.escape(n) + r"\b")
        for i, line in enumerate(lines):
            mm = pat.search(line)
            if mm:
                a = len(line[: mm.start()].encode("utf-8"))
                return {"sl": i, "sc": a, "el": i, "ec": a + len(n.encode("utf-8"))}
    return {
        "sl": 0,
        "sc": 0,
        "el": 0,
        "ec": max(1, len((lines[0] if lines else "").encode("utf-8"))),
    }


def loc_of_path(decls: list[Any], segs: list[Any]) -> dict[str, Any] | None:
    """the source position of a document path: the literal the path leads to
    in the root's declaration, or the deepest literal on the way"""
    root = segs[0]
    decl = next((d for d in decls if d["d"] in ("output", "input") and d["name"] == root), None)
    if decl is None or not decl.get("loc"):
        return None
    e: dict[str, Any] | None = decl.get("expr") if decl["d"] == "output" else decl.get("fallback")
    best = decl["loc"]
    for s in segs[1:]:
        if not e:
            break
        nxt: dict[str, Any] | None = None
        k = seg_text(s)
        if e["e"] == "paren":
            e = e["x"]
        if e["e"] == "obj" and isinstance(k, str):
            nxt = next((en["val"] for en in e["entries"] if en["key"] == k), None)
        elif e["e"] == "arr" and isinstance(k, int) and not isinstance(k, bool):
            nxt = e["items"][k]["expr"] if 0 <= k < len(e["items"]) else None
        elif e["e"] == "with":
            e = e["base"]
            continue
        if not nxt:
            break
        e = nxt
        if e.get("loc"):
            best = e["loc"]
    return best


def severity_of(s: str) -> int:
    return 1 if s == "error" else 2 if s == "warn" else 3


def analyze(uri: str) -> None:
    src = docs[uri]
    path = path_of(uri)
    lines = src.split("\n")
    out: list[Any] = []

    def push(loc: dict[str, Any], d: dict[str, Any]) -> None:
        item: dict[str, Any] = {
            "range": range_of(loc, lines),
            "severity": severity_of(d["severity"]),
            "source": "decl",
        }
        if d.get("code") or d.get("id"):
            item["code"] = d["id"] if d.get("id") is not None else d.get("code")
        item["message"] = f"{d['message']} (at {d['path']})" if d.get("path") else d["message"]
        out.append(item)

    parsed = parse_source(src)
    errors, decls = parsed["errors"], parsed["decls"]
    if errors:
        for e in errors:
            out.append(
                {
                    "range": {
                        "start": {
                            "line": e["row"],
                            "character": _u16_col(_line(lines, e["row"]), e["col"]),
                        },
                        "end": {
                            "line": e["row"],
                            "character": _u16_col(_line(lines, e["row"]), e["col"]) + 1,
                        },
                    },
                    "severity": 1,
                    "source": "decl",
                    "code": "E2001",
                    "message": "syntax error",
                }
            )
    else:
        a = analysis_of(uri)
        assert a is not None  # the text parsed: its analysis exists
        r = a.run
        for d in r.load_diags:
            # a loading problem is anchored to the import it concerns when one is named
            imp = next(
                (
                    x
                    for x in decls
                    if x["d"] in ("import", "re_export")
                    and x.get("loc")
                    and re.sub(r"\.decl$", "", re.sub(r"^\./", "", x["from"])) in d["message"]
                ),
                None,
            )
            push(imp["loc"] if imp is not None else anchor_for(src, d["message"]), d)
        for c in r.checks:
            if c["file"] != path:
                continue
            push(c["diag"].get("loc") or anchor_for(src, c["diag"]["message"]), c["diag"])
        for d in r.diags:
            if d["severity"] == "information":
                continue
            segs = None
            try:
                segs = parse_path(d["path"], "") if d.get("path") else None
            except Exception:
                segs = None
            loc = loc_of_path(decls, segs) if segs else None
            if not loc:
                continue  # a root declared elsewhere: its own module's business
            push(loc, d)
    notify("textDocument/publishDiagnostics", {"uri": uri, "diagnostics": out})


# ---------------- positions -> nodes ----------------
def node_at(decls: list[Any], pos: dict[str, Any]) -> dict[str, Any] | None:
    """the innermost AST node (declaration, member, type, or expression) at a position (bytes)"""
    best: dict[str, Any] = {"hit": None}

    def visit(x: Any, parents: list[Any]) -> None:
        if not x or not isinstance(x, (dict, list)):
            return
        if isinstance(x, list):
            for y in x:
                visit(y, parents)
            return
        own = bool(x.get("loc")) and contains(x["loc"], pos)
        if own and (best["hit"] is None or span(x["loc"]) <= span(best["hit"]["node"]["loc"])):
            best["hit"] = {"node": x, "parents": parents}
        for k, v in x.items():
            if k == "loc" or not v or not isinstance(v, (dict, list)):
                continue
            visit(v, [*parents, x] if own else parents)

    visit(decls, [])
    return best["hit"]


def is_expr(x: Any) -> bool:
    return isinstance(x, dict) and isinstance(x.get("e"), str)


def is_type(x: Any) -> bool:
    return isinstance(x, dict) and isinstance(x.get("k"), str)


def is_decl(x: Any) -> bool:
    return isinstance(x, dict) and isinstance(x.get("d"), str)


def is_member(x: Any) -> bool:
    return isinstance(x, dict) and isinstance(x.get("m"), str)


def name_range(text: str, decl: dict[str, Any], name: str) -> dict[str, Any]:
    """the range of a declaration's name token (the declaration site)"""
    loc = decl["loc"]
    lines = text.split("\n")
    pat = re.compile(rb"\b" + re.escape(name.encode("utf-8")) + rb"\b")
    for i in range(loc["sl"], min(loc["el"], len(lines) - 1) + 1):
        start = loc["sc"] if i == loc["sl"] else 0
        m = pat.search(lines[i].encode("utf-8"), start)
        if m:
            return {"sl": i, "sc": m.start(), "el": i, "ec": m.start() + len(name.encode("utf-8"))}
    return loc


def member_range(text: str, member: dict[str, Any], name: str) -> dict[str, Any]:
    loc = member["loc"]
    line = _line(text.split("\n"), loc["sl"])
    i = _bfind(line, name, loc["sc"])
    return (
        {"sl": loc["sl"], "sc": i, "el": loc["sl"], "ec": i + len(name.encode("utf-8"))}
        if i >= 0
        else loc
    )


# ---------------- what is under the cursor ----------------
class Site:
    __slots__ = ("decl", "kind", "member", "module", "name", "range")

    def __init__(
        self,
        kind: str,
        module: Any,
        decl: dict[str, Any] | None,
        member: dict[str, Any] | None,
        range_: dict[str, Any],
        name: str,
    ) -> None:
        self.kind, self.module, self.decl, self.member, self.range, self.name = (
            kind,
            module,
            decl,
            member,
            range_,
            name,
        )


def site_of_target(a: Analysis, t: dict[str, Any] | None) -> Site | None:
    """the declaration a target denotes, as a site in its module"""
    if not t or t.get("env") is None:
        return None
    m = next((x for x in a.run.modules if x.env is t["env"]), None)
    if m is None:
        return None
    text = text_of(a, m)
    decl = next(
        (d for d in m.decls if d.get("name") == t["name"] and d.get("loc") and d["d"] != "import"),
        None,
    )
    if decl is not None:
        return Site(decl["d"], m, decl, None, name_range(text, decl, t["name"]), t["name"])
    return None


def _record_members(body: dict[str, Any]) -> list[Any]:
    if body.get("k") == "record":
        return body["members"]
    if body.get("k") == "named" and body.get("ext") and body["ext"].get("k") == "record":
        return body["ext"]["members"]
    return []


def member_site(a: Analysis, m: Any, rt: dict[str, Any] | None, member: str) -> Site | None:
    """the member's declaring type, extension chains followed (§4)"""
    seen: set[Any] = set()
    type_name = (
        rt.get("name")
        if rt and rt.get("t") == "rec"
        else (rt["base"].get("name") if rt and rt.get("t") == "pred" and rt.get("base") else None)
    )
    while type_name and type_name not in seen:
        seen.add(type_name)
        site = site_of_target(a, resolve_in(m.env, type_name))
        if site is None or site.decl is None or site.decl["d"] != "type":
            return None
        decl = site.decl
        body = decl["type"]
        mem = next((x for x in _record_members(body) if x.get("name") == member), None)
        if mem is not None and mem.get("loc"):
            return Site(
                "member",
                site.module,
                decl,
                mem,
                member_range(text_of(a, site.module), mem, member),
                member,
            )
        type_name = body.get("name") if body.get("k") == "named" else None
    return None


def site_at(a: Analysis, uri: str, pos: dict[str, Any]) -> dict[str, Any] | None:
    m = module_of(a, path_of(uri))
    if m is None:
        return None
    hit = node_at(m.decls, pos)
    if hit is None:
        return {"site": None, "type": None, "hit": None, "module": m}
    t = tables_of(a, m)
    n = hit["node"]
    if is_expr(n):
        ty = t["types"].get(id(n))
        if n["e"] == "name":
            target = t["res"].get(id(n))
            if target is None:
                target = resolve_in(m.env, n["name"])
            return {"site": site_of_target(a, target), "type": ty, "hit": hit, "module": m}
        if n["e"] == "member":
            x = n["x"]
            if x["e"] == "name" and x["name"] in m.env.namespaces:
                ns = m.env.namespaces[x["name"]]
                ex = ns["exports"].get(n["name"])
                return {
                    "site": site_of_target(a, resolve_in(ex["env"], ex["name"])) if ex else None,
                    "type": ty,
                    "hit": hit,
                    "module": m,
                }
            xt = t["types"].get(id(x))
            return {
                "site": member_site(a, m, xt["rt"] if xt else None, n["name"]),
                "type": ty,
                "hit": hit,
                "module": m,
            }
        return {"site": None, "type": ty, "hit": hit, "module": m}
    if is_type(n) and n["k"] == "named":
        parts = n["name"].split(".")
        head, tail = parts[0], parts[1] if len(parts) > 1 else None
        if tail and head in m.env.namespaces:
            ex = m.env.namespaces[head]["exports"].get(tail)
            target = resolve_in(ex["env"], ex["name"]) if ex else None
        else:
            target = resolve_in(m.env, head)
        return {"site": site_of_target(a, target), "type": None, "hit": hit, "module": m}
    if is_member(n) and "name" in n:
        decl = next((p for p in hit["parents"] if is_decl(p)), None)
        site = (
            Site("member", m, decl, n, member_range(text_of(a, m), n, n["name"]), n["name"])
            if decl is not None
            else None
        )
        return {"site": site, "type": None, "hit": hit, "module": m}
    if is_decl(n) and isinstance(n.get("name"), str):
        r = name_range(text_of(a, m), n, n["name"])
        if contains(r, pos):
            return {
                "site": Site(n["d"], m, n, None, r, n["name"]),
                "type": None,
                "hit": hit,
                "module": m,
            }
    return {"site": None, "type": None, "hit": hit, "module": m}


def _pos_of(uri: str, pos: dict[str, Any]) -> dict[str, Any]:
    return pos_bytes(pos, docs.get(uri, "").split("\n"))


# ---------------- hover ----------------
_DOC = re.compile(r"^\s*///")


def decl_text(a: Analysis, site: Site) -> list[Any]:
    text = text_of(a, site.module)
    lines = text.split("\n")
    if site.member is not None and site.member.get("loc"):
        l = site.member["loc"]
        doc_lines: list[Any] = []
        frm = l["sl"]
        while frm > 0 and _DOC.match(lines[frm - 1]):
            frm -= 1
            doc_lines.insert(0, lines[frm].strip())
        if l["sl"] == l["el"]:
            body = [_bslice(lines[l["sl"]], l["sc"], l["ec"])]
        else:
            body = [
                _bslice(lines[l["sl"]], l["sc"], len(lines[l["sl"]].encode("utf-8"))),
                *lines[l["sl"] + 1 : l["el"]],
                _bslice(lines[l["el"]], 0, l["ec"]),
            ]
        return doc_lines + [x for x in (b.strip() for b in body) if x]
    assert site.decl is not None  # a documented site is a declaration or a member of one
    l = site.decl["loc"]
    doc_lines = []
    frm = l["sl"]
    while frm > 0 and _DOC.match(lines[frm - 1]):
        frm -= 1
        doc_lines.insert(0, lines[frm].strip())
    body = lines[l["sl"] : l["el"] + 1]
    if len(body) > 12:
        body = [*body[:11], "    …", body[-1]]
    return doc_lines + body


def hover(uri: str, pos: dict[str, Any]) -> Any:
    a = analysis_of(uri)
    if a is None:
        return None
    s = site_at(a, uri, _pos_of(uri, pos))
    if s is None:
        return None
    parts: list[Any] = []
    if s["site"] is not None:
        lines = decl_text(a, s["site"])
        doc = [re.sub(r"^///\s?", "", l) for l in lines if l.startswith("///")]
        code = [l for l in lines if not l.startswith("///")]
        if doc:
            parts.append("\n".join(doc))
        parts.append("```decl\n" + "\n".join(code) + "\n```")
    if s["type"] is not None:
        parts.append(f"`{type_text(s['type']['rt'])}{'?' if s['type']['abs'] else ''}`")
    if not parts:
        return None
    hit = s["hit"]
    node = hit["node"] if hit else None
    if node is not None and node.get("loc"):
        return {
            "contents": {"kind": "markdown", "value": "\n\n".join(parts)},
            "range": range_of(node["loc"], text_of(a, s["module"]).split("\n")),
        }
    return {"contents": {"kind": "markdown", "value": "\n\n".join(parts)}}


# ---------------- navigation ----------------
def location(a: Analysis, m: Any, loc: dict[str, Any]) -> dict[str, Any]:
    return {"uri": uri_of(m.path), "range": range_of(loc, text_of(a, m).split("\n"))}


def definition(uri: str, pos: dict[str, Any]) -> Any:
    a = analysis_of(uri)
    if a is None:
        return None
    s = site_at(a, uri, _pos_of(uri, pos))
    return location(a, s["site"].module, s["site"].range) if s and s["site"] is not None else None


def named_types_of(ast: Any, out: list[Any] | None = None) -> list[Any]:
    """the named types a declared type mentions: `T`, `T[]`, `A | B`, `T?`,
    `ref<T>`, `map<K, V>` — the declarations "go to type definition" reaches"""
    if out is None:
        out = []
    if not ast:
        return out
    k = ast.get("k")
    if k == "named":
        out.append(ast["name"])
        for arg in ast.get("args") or []:
            named_types_of(arg, out)
    elif k == "array":
        named_types_of(ast.get("elem"), out)
    elif k in ("union", "isect"):
        for arm in ast["arms"]:
            named_types_of(arm, out)
    elif k == "map":
        named_types_of(ast.get("val"), out)
    return out


def named_types_of_rt(rt: Any, out: list[Any] | None = None) -> list[Any]:
    """the same over a resolved type: record names, through refs, arrays, maps, unions"""
    if out is None:
        out = []
    if not rt:
        return out
    t = rt.get("t")
    if t == "rec":
        if rt.get("name") and not rt["name"].startswith("{"):
            out.append(rt["name"])
    elif t == "pred":
        named_types_of_rt(rt.get("base"), out)
    elif t == "ref":
        named_types_of_rt(rt.get("target"), out)
    elif t == "arr":
        named_types_of_rt(rt.get("elem"), out)
    elif t == "map":
        named_types_of_rt(rt.get("val"), out)
    elif t == "union":
        for arm in rt["arms"]:
            named_types_of_rt(arm, out)
    return out


def type_definition(uri: str, pos: dict[str, Any]) -> Any:
    a = analysis_of(uri)
    if a is None:
        return None
    s = site_at(a, uri, _pos_of(uri, pos))
    if not s:
        return None
    # the declared type first (a member, an output or input, a constant's
    # annotation): the named types it spells, whatever they resolve to —
    # an alias of a literal union has a declaration too; else the
    # expression's inferred type
    names: list[Any] = []
    env = s["module"].env
    site0: Site | None = s["site"]
    if site0 is not None:
        d = site0.decl
        ast = (
            site0.member.get("type")
            if site0.member is not None
            else (
                d.get("type") if d is not None and d["d"] in ("output", "input", "const") else None
            )
        )
        if ast:
            names = named_types_of(ast)
            env = site0.module.env
        elif d is not None and d["d"] == "const" and d.get("expr"):
            ty = tables_of(a, site0.module)["types"].get(id(d["expr"]))
            names = named_types_of_rt(ty.get("rt") if ty else None)
    hit = s["hit"]
    if not names and hit is not None and is_expr(hit["node"]) and hit["node"]["e"] == "member":
        # a member access: the member's declared type, where it is declared
        xt = (tables_of(a, s["module"])["types"].get(id(hit["node"]["x"])) or {}).get("rt")
        ms = member_site(a, s["module"], xt, hit["node"]["name"])
        if ms is not None and ms.member is not None and ms.member.get("type"):
            names = named_types_of(ms.member["type"])
            env = ms.module.env
    if not names:
        names = named_types_of_rt(s["type"]["rt"] if s["type"] else None)
    seen: set[Any] = set()
    locs: list[Any] = []
    for n in names:
        head, _, tail = n.partition(".")
        if tail and head in env.namespaces:
            ex = env.namespaces[head]["exports"].get(tail)
            target = resolve_in(ex["env"], ex["name"]) if ex else None
        else:
            target = resolve_in(env, head)
        site = site_of_target(a, target)
        if site is None:
            continue
        key = f"{site.module.path}:{site.range['sl']}:{site.range['sc']}"
        if key in seen:
            continue
        seen.add(key)
        locs.append(location(a, site.module, site.range))
    return None if not locs else locs[0] if len(locs) == 1 else locs


def member_token_loc(text: str, e: dict[str, Any]) -> dict[str, Any]:
    l = e["loc"]
    line = _line(text.split("\n"), l["el"])
    i = _brfind(line, e["name"], l["ec"])
    return (
        {"sl": l["el"], "sc": i, "el": l["el"], "ec": i + len(e["name"].encode("utf-8"))}
        if i >= 0
        else l
    )


def type_name_loc(t: dict[str, Any], offset: int, name: str) -> dict[str, Any]:
    return {
        "sl": t["loc"]["sl"],
        "sc": t["loc"]["sc"] + offset,
        "el": t["loc"]["sl"],
        "ec": t["loc"]["sc"] + offset + len(name.encode("utf-8")),
    }


def import_item_loc(text: str, d: dict[str, Any], name: str) -> dict[str, Any]:
    l = d["loc"]
    line = _line(text.split("\n"), l["sl"])
    i = _bfind(line, name, l["sc"])
    return (
        {"sl": l["sl"], "sc": i, "el": l["sl"], "ec": i + len(name.encode("utf-8"))}
        if i >= 0
        else l
    )


def references(uri: str, pos: dict[str, Any], include_declaration: bool) -> list[Any]:
    """every reference to a site across the universe: name and member nodes
    that resolve to the same declaration, plus the declaration itself"""
    a = analysis_of(uri)
    if a is None:
        return []
    s = site_at(a, uri, _pos_of(uri, pos))
    if not s or s["site"] is None:
        return []
    target: Site = s["site"]
    out: list[Any] = []

    def same(x: Site | None) -> bool:
        return (
            x is not None
            and x.module is target.module
            and x.name == target.name
            and x.kind == target.kind
            and (x.kind != "member" or x.decl is target.decl)
        )

    for m in a.run.modules:
        t = tables_of(a, m)
        text = text_of(a, m)

        def visit(x: Any, m: Any = m, t: Any = t, text: str = text) -> None:
            if not x or not isinstance(x, (dict, list)):
                return
            if isinstance(x, list):
                for y in x:
                    visit(y)
                return
            if is_expr(x) and x.get("loc"):
                if x["e"] == "name":
                    tg = t["res"].get(id(x))
                    if tg is None:
                        tg = resolve_in(m.env, x["name"])
                    if same(site_of_target(a, tg)):
                        out.append((m, x["loc"]))
                if x["e"] == "member":
                    xx = x["x"]
                    site: Site | None = None
                    if xx["e"] == "name" and xx["name"] in m.env.namespaces:
                        ex = m.env.namespaces[xx["name"]]["exports"].get(x["name"])
                        site = site_of_target(a, resolve_in(ex["env"], ex["name"])) if ex else None
                    else:
                        xt = t["types"].get(id(xx))
                        site = member_site(a, m, xt["rt"] if xt else None, x["name"])
                    if same(site):
                        out.append((m, member_token_loc(text, x)))
            if is_type(x) and x.get("k") == "named" and x.get("loc"):
                parts = x["name"].split(".")
                head, tail = parts[0], parts[1] if len(parts) > 1 else None
                if tail and head in m.env.namespaces:
                    ex = m.env.namespaces[head]["exports"].get(tail)
                    tg = resolve_in(ex["env"], ex["name"]) if ex else None
                else:
                    tg = resolve_in(m.env, head)
                if same(site_of_target(a, tg)):
                    out.append((m, type_name_loc(x, len(head) + 1 if tail else 0, tail or head)))
            # import items naming the declaration
            if is_decl(x) and x["d"] in ("import", "re_export") and x.get("loc") and x.get("names"):
                for it in x["names"]:
                    im = m.env.imports.get(it.get("as") or it["name"])
                    if im and same(site_of_target(a, resolve_in(im["env"], im["name"]))):
                        out.append((m, import_item_loc(text, x, it["name"])))
            for k, v in x.items():
                if k != "loc" and v and isinstance(v, (dict, list)):
                    visit(v)

        visit(m.decls)
    if include_declaration:
        out.insert(0, (target.module, target.range))
    seen: set[Any] = set()
    kept: list[Any] = []
    for r in out:
        key = f"{r[0].path}:{r[1]['sl']}:{r[1]['sc']}"
        if key in seen:
            continue
        seen.add(key)
        kept.append(r)
    kept.sort(key=lambda r: (r[0].path, r[1]["sl"], r[1]["sc"]))
    return kept


# ---------------- completion ----------------
def completion(uri: str, pos: dict[str, Any]) -> dict[str, Any]:
    a = analysis_of(uri)
    text = docs.get(uri)
    if text is None:
        return {"isIncomplete": False, "items": []}
    line = _line(text.split("\n"), pos["line"])
    prefix = line.encode("utf-8")[: _byte_col(line, pos["character"])].decode("utf-8", "replace")
    # while the text does not parse, the scope is the last one that did
    session = (
        a.session
        if a is not None
        else (last_good[uri].session if uri in last_good else Session(path_of(uri), overlay))
    )
    items: list[Any] = []
    for c in session.complete(prefix, []):
        parts = c.split("  ")
        label = parts[0]
        detail = parts[1] if len(parts) > 1 else None
        if detail:
            kind = 5 if detail.startswith(("derived", "required", "optional", "defaulted")) else 6
        else:
            kind = 7 if re.match(r"^[A-Z]", label) else 14 if label.startswith("$") else 6
        item: dict[str, Any] = {"label": label, "kind": kind}
        if detail:
            item["detail"] = detail
        items.append(item)
    return {"isIncomplete": False, "items": items}


# ---------------- symbols, folding, formatting ----------------
SYMBOL_KIND = {
    "type": 5,
    "const": 14,
    "func": 12,
    "output": 13,
    "input": 13,
    "diagnostic": 24,
    "dimension": 13,
    "unit": 13,
}


def document_symbols(uri: str) -> list[Any]:
    text = docs.get(uri)
    if text is None:
        return []
    parsed = parse_source(text)
    if parsed["errors"]:
        return []
    lines = text.split("\n")
    out: list[Any] = []
    for d in parsed["decls"]:
        if not d.get("loc") or not isinstance(d.get("name"), str) or d["d"] not in SYMBOL_KIND:
            continue
        sym: dict[str, Any] = {
            "name": d["name"],
            "kind": SYMBOL_KIND[d["d"]],
            "range": range_of(d["loc"], lines),
            "selectionRange": range_of(name_range(text, d, d["name"]), lines),
        }
        if d["d"] == "type":
            body = (
                d["type"]
                if d["type"].get("k") == "record"
                else (
                    d["type"]["ext"]
                    if d["type"].get("k") == "named"
                    and d["type"].get("ext")
                    and d["type"]["ext"].get("k") == "record"
                    else None
                )
            )
            if body:
                children = []
                for m in body["members"]:
                    if not m.get("loc") or not isinstance(m.get("name"), str):
                        continue
                    children.append(
                        {
                            "name": f"assert {m['name']}"
                            if m["m"] == "assert"
                            else (f"{m['name']}$" if m.get("hidden") else m["name"]),
                            "kind": 24 if m["m"] == "assert" else 7,
                            "range": range_of(m["loc"], lines),
                            "selectionRange": range_of(member_range(text, m, m["name"]), lines),
                        }
                    )
                if children:
                    sym["children"] = children
        out.append(sym)
    return out


def folding_ranges(uri: str) -> list[Any]:
    text = docs.get(uri)
    if text is None:
        return []
    parsed = parse_source(text)
    if parsed["errors"]:
        return []
    out: list[Any] = []

    def visit(x: Any) -> None:
        if not x or not isinstance(x, (dict, list)):
            return
        if isinstance(x, list):
            for y in x:
                visit(y)
            return
        loc = x.get("loc")
        if (
            loc
            and loc["el"] > loc["sl"]
            and (
                is_decl(x)
                or (is_type(x) and x["k"] == "record")
                or (is_expr(x) and x["e"] in ("obj", "arr", "match"))
                or (is_member(x) and x["m"] == "when")
            )
        ):
            out.append({"startLine": loc["sl"], "endLine": loc["el"], "kind": "region"})
        for k, v in x.items():
            if k != "loc" and v and isinstance(v, (dict, list)):
                visit(v)

    visit(parsed["decls"])
    seen: set[Any] = set()
    kept = []
    for r in out:
        key = f"{r['startLine']}-{r['endLine']}"
        if key not in seen:
            seen.add(key)
            kept.append(r)
    return kept


def formatting(uri: str) -> list[Any]:
    text = docs.get(uri)
    if text is None:
        return []
    try:
        out = format_source(text)
    except Exception:
        return []
    if out == text:
        return []
    lines = text.split("\n")
    return [
        {
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": len(lines) - 1, "character": u16(lines[-1])},
            },
            "newText": out,
        }
    ]


# ---------------- rename ----------------
def prepare_rename(uri: str, pos: dict[str, Any]) -> Any:
    a = analysis_of(uri)
    if a is None:
        return None
    s = site_at(a, uri, _pos_of(uri, pos))
    if not s or s["site"] is None:
        text = docs.get(uri) or ""
        lines = text.split("\n")
        bp = pos_bytes(pos, lines)
        locs = local_ranges(uri, bp)
        here = next((l for l in locs if contains(l, bp)), None) if locs else None
        return (
            {
                "range": range_of(here, lines),
                "placeholder": _bslice(_line(lines, here["sl"]), here["sc"], here["ec"]),
            }
            if here
            else None
        )
    if not s["hit"] or not s["hit"]["node"].get("loc"):
        return None
    n = s["hit"]["node"]
    text = text_of(a, s["module"])
    if is_expr(n) and n["e"] == "member":
        loc = member_token_loc(text, n)
    elif is_type(n):
        loc = type_name_loc(
            n, n["name"].index(".") + 1 if "." in n["name"] else 0, n["name"].split(".")[-1]
        )
    elif is_decl(n) or is_member(n):
        loc = s["site"].range
    else:
        loc = n["loc"]
    return {"range": range_of(loc, text.split("\n")), "placeholder": s["site"].name}


def rename(uri: str, pos: dict[str, Any], new_name: str) -> Any:
    refs = references(uri, pos, True)
    if not refs:
        lines = (docs.get(uri) or "").split("\n")
        locs = local_ranges(uri, pos_bytes(pos, lines))
        return (
            {"changes": {uri: [{"range": range_of(l, lines), "newText": new_name} for l in locs]}}
            if locs
            else None
        )
    a = analysis_of(uri)
    changes: dict[str, Any] = {}
    if a is None:  # no analysis, no references
        return {"changes": changes}
    for m, loc in refs:
        changes.setdefault(uri_of(m.path), []).append(
            {"range": range_of(loc, text_of(a, m).split("\n")), "newText": new_name}
        )
    return {"changes": changes}


# ---------------- lenses and commands ----------------
def code_lenses(uri: str) -> list[Any]:
    text = docs.get(uri)
    if text is None:
        return []
    parsed = parse_source(text)
    if parsed["errors"]:
        return []
    lines = text.split("\n")
    out: list[Any] = []
    for d in parsed["decls"]:
        if not d.get("loc"):
            continue
        loc = d["loc"]
        head = {"sl": loc["sl"], "sc": loc["sc"], "el": loc["sl"], "ec": loc["sc"]}
        if d["d"] == "output":
            out.append(
                {
                    "range": range_of(head, lines),
                    "command": {
                        "title": "evaluate",
                        "command": "decl.evaluate",
                        "arguments": [uri, d["name"]],
                    },
                }
            )
        if d["d"] == "input":
            out.append(
                {
                    "range": range_of(head, lines),
                    "command": {
                        "title": "validate",
                        "command": "decl.validate",
                        "arguments": [uri, d["name"]],
                    },
                }
            )
    return out


def execute_command(command: str, args: Any) -> Any:
    args = args or []
    uri = args[0] if len(args) > 0 else None
    root = args[1] if len(args) > 1 else None
    if not isinstance(uri, str):
        return None
    session = Session(path_of(uri), overlay)
    for name, file in config["inputs"].items():
        with contextlib.suppress(Exception):  # reported by :validate
            session.apply(
                {
                    "op": "bind",
                    "name": name,
                    "src": {
                        "kind": "file",
                        "file": file,
                        "text": read_text(os.path.join(os.path.dirname(path_of(uri)), file)),
                    },
                }
            )
    if command == "decl.evaluate":
        r = session.evaluate([root] if root else [])
        run, ds = r["run"], r["docs"]
        diags = [fmt_diag(d) for d in run.load_diags + [c["diag"] for c in run.checks] + run.diags]
        if root:
            return {"root": root, "document": ds[0]["json"] if ds else None, "diagnostics": diags}
        all_ = (
            "{" + ",".join(f"{json.dumps(d['name'])}:{d['json']}" for d in ds) + "}"
            if run.eng is not None and all(d["json"] is not None for d in ds)
            else None
        )
        return {"root": None, "document": all_, "diagnostics": diags}
    if command == "decl.validate":
        r = session.validate([root] if root else [])
        run = r["run"]
        return {
            "verdicts": r["verdicts"],
            "diagnostics": [
                fmt_diag(d) for d in run.load_diags + [c["diag"] for c in run.checks] + r["diags"]
            ],
        }
    if command == "decl.trace":
        return {"lines": session.trace(root)} if root else None
    if command == "decl.showSyntaxTree":
        return syntax_tree(uri)
    if command == "decl.reloadWorkspace":
        analyses.clear()
        for u in list(docs):
            analyze(u)
        return None
    return None


# ---------------- signature help ----------------
def _after(l: dict[str, Any], p: dict[str, Any]) -> bool:
    return p["line"] > l["el"] or (p["line"] == l["el"] and p["character"] >= l["ec"])


def _src_of(text: str, l: dict[str, Any]) -> str:
    lines = text.split("\n")
    if l["sl"] == l["el"]:
        return _bslice(_line(lines, l["sl"]), l["sc"], l["ec"])
    first = _line(lines, l["sl"])
    return "\n".join(
        [
            _bslice(first, l["sc"], len(first.encode("utf-8"))),
            *lines[l["sl"] + 1 : l["el"]],
            _bslice(_line(lines, l["el"]), 0, l["ec"]),
        ]
    )


def signature_help(uri: str, pos: dict[str, Any]) -> Any:
    a = analysis_of(uri)
    if a is None:
        a = last_good.get(uri)
    if a is None:
        return None
    m = module_of(a, path_of(uri))
    if m is None:
        return None
    bpos = _pos_of(uri, pos)
    hit = node_at(m.decls, bpos)
    if hit is None:
        return None
    calls = [n for n in hit["parents"] + [hit["node"]] if is_expr(n) and n["e"] == "call"][::-1]
    for c in calls:
        if not c["fn"].get("loc") or not _after(c["fn"]["loc"], bpos):
            continue
        active = 0
        for i, arg in enumerate(c["args"]):
            if arg.get("loc") and _after(arg["loc"], bpos):
                active = i + 1
            elif arg.get("loc") and contains(arg["loc"], bpos):
                active = i
        if c["fn"]["e"] == "name":
            site = site_of_target(a, resolve_in(m.env, c["fn"]["name"]))
            if site is None or site.decl is None or site.decl["d"] != "func":
                return None
            decl = site.decl
            text = text_of(a, site.module)

            def ptype(p: dict[str, Any], text: str = text) -> str:
                return (
                    _src_of(text, p["type"]["loc"])
                    if p.get("type") and p["type"].get("loc")
                    else "…"
                )

            params = [f"{p['name']}: {ptype(p)}" for p in decl["params"]]
            ret = (
                f": {_src_of(text, decl['ret']['loc'])}"
                if decl.get("ret") and decl["ret"].get("loc")
                else ""
            )
            return {
                "signatures": [
                    {
                        "label": f"{decl['name']}({', '.join(params)}){ret}",
                        "parameters": [{"label": p} for p in params],
                    }
                ],
                "activeSignature": 0,
                "activeParameter": min(active, max(0, len(params) - 1)),
            }
        sp = _std_path(c["fn"])
        if sp is not None and sp in STD:
            params = [f"a{i + 1}" for i in range(STD[sp][0])]
            return {
                "signatures": [
                    {
                        "label": f"std.{sp}({', '.join(params)})",
                        "parameters": [{"label": p} for p in params],
                    }
                ],
                "activeSignature": 0,
                "activeParameter": min(active, max(0, len(params) - 1)),
            }
        return None
    return None


# ---------------- workspace symbols, selection ranges ----------------
def workspace_symbols(query: str) -> list[Any]:
    out: list[Any] = []
    seen: set[Any] = set()
    q = query.lower()
    for a in list(last_good.values()):
        for m in a.run.modules:
            if m.path in seen:
                continue
            seen.add(m.path)
            text = text_of(a, m)
            lines = text.split("\n")
            for d in m.decls:
                if (
                    d.get("loc")
                    and isinstance(d.get("name"), str)
                    and d["d"] in SYMBOL_KIND
                    and q in d["name"].lower()
                ):
                    out.append(
                        {
                            "name": d["name"],
                            "kind": SYMBOL_KIND[d["d"]],
                            "location": {
                                "uri": uri_of(m.path),
                                "range": range_of(name_range(text, d, d["name"]), lines),
                            },
                        }
                    )
    out.sort(key=lambda x: (x["name"], x["location"]["uri"]))
    return out


def selection_ranges(uri: str, positions: list[Any]) -> list[Any]:
    text = docs.get(uri)
    if text is None:
        return []
    parsed = parse_source(text)
    if parsed["errors"]:
        return [{"range": {"start": p, "end": p}} for p in positions]
    lines = text.split("\n")
    out: list[Any] = []
    for p in positions:
        hit = node_at(parsed["decls"], pos_bytes(p, lines))
        chain = [n for n in ([hit["node"], *hit["parents"][::-1]]) if n.get("loc")] if hit else []
        ranges = [range_of(n["loc"], lines) for n in chain]
        sel: Any = None
        for r in reversed(ranges):
            sel = {"range": r, "parent": sel} if sel is not None else {"range": r}
        out.append(sel if sel is not None else {"range": {"start": p, "end": p}})
    return out


# ---------------- semantic tokens ----------------
TOKEN_TYPES = ["type", "property", "function", "variable", "namespace", "parameter"]
TOKEN_MODS = [
    "declaration",
    "required",
    "optional",
    "defaulted",
    "derived",
    "hidden",
    "unresolved",
    "readonly",
]
_T = {t: i for i, t in enumerate(TOKEN_TYPES)}
_M = {t: 1 << i for i, t in enumerate(TOKEN_MODS)}


def _member_mods(kind: str, hidden: Any = None) -> int:
    base = (
        _M["derived"]
        if kind == "der"
        else _M["defaulted"]
        if kind == "dflt"
        else _M["optional"]
        if kind == "opt"
        else _M["required"]
    )
    return base | (_M["hidden"] if hidden else 0)


def _param_loc(text: str, decl: dict[str, Any], name: str) -> dict[str, Any] | None:
    l = decl["loc"]
    line = _line(text.split("\n"), l["sl"]).encode("utf-8")
    open_ = line.find(b"(", l["sc"])
    if open_ < 0:
        return None
    mm = re.compile(rb"\b" + re.escape(name.encode("utf-8")) + rb"\b").search(line, open_)
    return (
        {
            "sl": l["sl"],
            "sc": mm.start(),
            "el": l["sl"],
            "ec": mm.start() + len(name.encode("utf-8")),
        }
        if mm
        else None
    )


def semantic_tokens(uri: str) -> dict[str, Any]:
    a = analysis_of(uri)
    if a is None:
        return {"data": []}
    m = module_of(a, path_of(uri))
    if m is None:
        return {"data": []}
    text = text_of(a, m)
    lines = text.split("\n")
    t = tables_of(a, m)
    toks: list[Any] = []

    def push(l: dict[str, Any], type_: int, mods: int = 0) -> None:
        if l["sl"] == l["el"] and l["ec"] > l["sc"]:
            toks.append((l, type_, mods))

    def member_kind(rt: dict[str, Any] | None, name: str) -> dict[str, Any] | None:
        r = (
            rt["base"]
            if rt and rt.get("t") == "pred"
            else rt["target"]
            if rt and rt.get("t") == "ref"
            else rt
        )
        mem = (
            next((x for x in r["members"] if x.get("name") == name), None)
            if r and r.get("t") == "rec"
            else None
        )
        return {"kind": mem["kind"], "hidden": mem.get("hidden")} if mem else None

    def visit(x: Any, in_func: frozenset[Any]) -> None:
        if not x or not isinstance(x, (dict, list)):
            return
        if isinstance(x, list):
            for y in x:
                visit(y, in_func)
            return
        if is_decl(x) and x.get("loc") and isinstance(x.get("name"), str):
            r = name_range(text, x, x["name"])
            type_ = (
                _T["type"]
                if x["d"] in ("type", "dimension", "unit")
                else _T["function"]
                if x["d"] in ("func", "diagnostic")
                else _T["variable"]
            )
            push(r, type_, _M["declaration"] | (_M["readonly"] if x["d"] == "const" else 0))
            if x["d"] == "func":
                in_func = frozenset(p["name"] for p in x["params"])
                for p in x["params"]:
                    pl = _param_loc(text, x, p["name"])
                    if pl:
                        push(pl, _T["parameter"], _M["declaration"])
        if (
            is_member(x)
            and x.get("loc")
            and isinstance(x.get("name"), str)
            and x["m"] in ("value", "derived")
        ):
            kind = (
                "der"
                if x["m"] == "derived"
                else "dflt"
                if x.get("dflt")
                else "opt"
                if x.get("opt")
                else "req"
            )
            push(
                member_range(text, x, x["name"]),
                _T["property"],
                _M["declaration"] | _member_mods(kind, x.get("hidden")),
            )
        if is_type(x) and x.get("k") == "named" and x.get("loc"):
            parts = x["name"].split(".")
            head, tail = parts[0], parts[1] if len(parts) > 1 else None
            if tail:
                push(type_name_loc(x, 0, head), _T["namespace"])
                push(type_name_loc(x, len(head) + 1, tail), _T["type"])
            elif head not in ("map", "ref", "quantity"):
                push(
                    type_name_loc(x, 0, head),
                    _T["type"],
                    0 if resolve_in(m.env, head) else _M["unresolved"],
                )
        if is_expr(x) and x.get("loc"):
            if x["e"] == "name":
                target = t["res"].get(id(x))
                if target is None:
                    target = resolve_in(m.env, x["name"])
                if x["name"] == "std":
                    push(x["loc"], _T["namespace"])
                elif x["name"] in in_func or (target and target.get("kind") == "var"):
                    push(x["loc"], _T["parameter"])
                elif not target:
                    push(x["loc"], _T["variable"], _M["unresolved"])
                else:
                    k = target["kind"]
                    push(
                        x["loc"],
                        _T["function"]
                        if k == "func"
                        else _T["namespace"]
                        if k == "namespace"
                        else _T["type"]
                        if k == "type"
                        else _T["variable"],
                        _M["readonly"] if k == "const" else 0,
                    )
            elif x["e"] == "member":
                xx = x["x"]
                ml = member_token_loc(text, x)
                sp = _std_path(x)
                if sp is not None:
                    push(ml, _T["function"] if sp in STD else _T["namespace"])
                elif xx["e"] == "name" and xx["name"] in m.env.namespaces:
                    ex = m.env.namespaces[xx["name"]]["exports"].get(x["name"])
                    tg = resolve_in(ex["env"], ex["name"]) if ex else None
                    push(
                        ml,
                        _T["function"]
                        if tg and tg["kind"] == "func"
                        else _T["type"]
                        if tg and tg["kind"] == "type"
                        else _T["variable"],
                        0 if tg else _M["unresolved"],
                    )
                else:
                    xt = t["types"].get(id(xx))
                    mk = member_kind(xt["rt"] if xt else None, x["name"])
                    push(ml, _T["property"], _member_mods(mk["kind"], mk["hidden"]) if mk else 0)
            elif x["e"] == "lambda":
                in_func = in_func | frozenset(x["params"])
            elif x["e"] in ("comp", "mapcomp"):
                in_func = in_func | frozenset(c["v"] for c in x["clauses"])
        for k, v in x.items():
            if k != "loc" and v and isinstance(v, (dict, list)):
                visit(v, in_func)

    visit(m.decls, frozenset())
    toks.sort(key=lambda tk: (tk[0]["sl"], tk[0]["sc"]))
    data: list[Any] = []
    pl = pc = 0
    for l, type_, mods in toks:
        line = _line(lines, l["sl"])
        sc, ec = _u16_col(line, l["sc"]), _u16_col(line, l["ec"])
        dl = l["sl"] - pl
        dc = sc - pc if dl == 0 else sc
        if dl == 0 and dc < 0:
            continue  # overlapping tokens: the first wins
        data.extend([dl, dc, ec - sc, type_, mods])
        pl, pc = l["sl"], sc
    return {"data": data}


# ---------------- inlay hints ----------------
hints: dict[str, Any] = {
    "types": True,
    "parameterNames": True,
    "values": False,
    "units": True,
    "contextVariables": False,
}


def inlay_hints(uri: str, range_: dict[str, Any]) -> list[Any]:
    a = analysis_of(uri)
    if a is None:
        return []
    m = module_of(a, path_of(uri))
    if m is None:
        return []
    text = text_of(a, m)
    lines = text.split("\n")
    t = tables_of(a, m)
    out: list[Any] = []

    def in_range(line: int) -> bool:
        return range_["start"]["line"] <= line <= range_["end"]["line"]

    def at(line: int, byte_col: int) -> dict[str, Any]:
        return {"line": line, "character": _u16_col(_line(lines, line), byte_col)}

    def visit(x: Any) -> None:
        if not x or not isinstance(x, (dict, list)):
            return
        if isinstance(x, list):
            for y in x:
                visit(y)
            return
        if (
            hints["types"]
            and is_member(x)
            and x["m"] == "derived"
            and not x.get("type")
            and x.get("loc")
        ):
            ty = t["types"].get(id(x["expr"]))
            r = member_range(text, x, x["name"])
            if ty and ty.get("rt") and in_range(r["el"]):
                out.append(
                    {
                        "position": at(r["el"], r["ec"] + (1 if x.get("hidden") else 0)),
                        "label": f": {type_text(ty['rt'])}",
                        "kind": 1,
                    }
                )
        if (
            hints["types"]
            and is_decl(x)
            and x["d"] == "const"
            and not x.get("type")
            and x.get("loc")
        ):
            ty = t["types"].get(id(x["expr"]))
            r = name_range(text, x, x["name"])
            if ty and ty.get("rt") and in_range(r["el"]):
                out.append(
                    {
                        "position": at(r["el"], r["ec"]),
                        "label": f": {type_text(ty['rt'])}",
                        "kind": 1,
                    }
                )
        if hints["parameterNames"] and is_expr(x) and x["e"] == "call" and x["fn"]["e"] == "name":
            tg = t["res"].get(id(x["fn"]))
            if tg is None:
                tg = resolve_in(m.env, x["fn"]["name"])
            site = site_of_target(a, tg)
            decl = site.decl if site is not None else None
            if decl is not None and decl["d"] == "func":
                for i, arg in enumerate(x["args"]):
                    p = decl["params"][i] if i < len(decl["params"]) else None
                    if p and arg.get("loc") and in_range(arg["loc"]["sl"]):
                        out.append(
                            {
                                "position": at(arg["loc"]["sl"], arg["loc"]["sc"]),
                                "label": f"{p['name']}:",
                                "kind": 2,
                                "paddingRight": True,
                            }
                        )
        if (
            hints["values"]
            and is_decl(x)
            and x["d"] == "output"
            and x.get("loc")
            and a.run.eng is not None
            and a.run.entry is not None
        ):
            # the evaluated derived members of a literal, at the end of the literal
            eng = a.run.eng

            def walk(v: Any, e: Any) -> None:
                if v is None or not e:
                    return
                if isinstance(v, RecInst) and e.get("e") == "obj":
                    parts: list[Any] = []
                    for mem in v.rt["members"]:
                        sl = v.slots.get(mem["name"])
                        if sl is None or mem.get("kind") != "der" or sl.hidden or sl.state != "ok":
                            continue
                        txt = eng.serialize(sl.value, x["name"]) or ""
                        parts.append(f"{mem['name']} = {txt[:37] + '…' if len(txt) > 40 else txt}")
                    if parts and in_range(e["loc"]["el"]):
                        out.append(
                            {
                                "position": at(e["loc"]["el"], e["loc"]["ec"]),
                                "label": f"// {', '.join(parts)}",
                                "paddingLeft": True,
                            }
                        )
                    for en in e["entries"]:
                        sl = v.slots.get(en["key"])
                        if sl is not None and sl.state == "ok":
                            walk(sl.value, en["val"])
                elif isinstance(v, ArrV) and e.get("e") == "arr":
                    for i, it in enumerate(v.items):
                        walk(it, e["items"][i]["expr"] if i < len(e["items"]) else None)
                elif isinstance(v, MapV) and e.get("e") == "obj":
                    for en in e["entries"]:
                        walk(v.entries.get(en["key"]), en["val"])

            walk(a.run.entry.env.roots.get(x["name"]), x["expr"])
        if (
            hints["contextVariables"]
            and is_expr(x)
            and x["e"] == "ctx"
            and x.get("loc")
            and x["name"] in ("$parent", "$root", "$key")
        ):
            # the bound the enclosing type declares for the variable
            decl = next(
                (
                    d
                    for d in m.decls
                    if d["d"] == "type"
                    and d.get("loc")
                    and d["loc"]["sl"] <= x["loc"]["sl"]
                    and x["loc"]["el"] <= d["loc"]["el"]
                ),
                None,
            )
            body = record_body_of(decl) if decl is not None else None
            ctxm = (
                next(
                    (
                        mm
                        for mm in body["members"]
                        if mm["m"] == "context" and mm.get("variable") == x["name"]
                    ),
                    None,
                )
                if body is not None
                else None
            )
            if (
                ctxm is not None
                and ctxm.get("type")
                and ctxm["type"].get("loc")
                and in_range(x["loc"]["el"])
            ):
                out.append(
                    {
                        "position": at(x["loc"]["el"], x["loc"]["ec"]),
                        "label": f": {_src_of(text, ctxm['type']['loc'])}",
                        "kind": 1,
                    }
                )
        if hints["units"] and is_expr(x) and x["e"] == "unitlit" and x.get("loc"):
            try:
                u = m.env.unit_info(x["unit"])
                base = m.env.base_unit_of.get(u["key"], u["key"])
                if base != x["unit"] and in_range(x["loc"]["el"]):
                    out.append(
                        {
                            "position": at(x["loc"]["el"], x["loc"]["ec"]),
                            "label": f"= {js_num_str(x['num'] * u['to_base'])} {base}",
                            "paddingLeft": True,
                        }
                    )
            except Exception:
                pass  # an unknown unit is a diagnostic
        for k, v in x.items():
            if k != "loc" and v and isinstance(v, (dict, list)):
                visit(v)

    visit(m.decls)
    out.sort(key=lambda h: (h["position"]["line"], h["position"]["character"]))
    return out


def record_body_of(decl: dict[str, Any] | None) -> dict[str, Any] | None:
    """the record body of a type declaration: its own, or its extension's"""
    if decl is None or decl.get("d") != "type":
        return None
    ty = decl["type"]
    if ty["k"] == "record":
        return ty
    if ty["k"] == "named" and ty.get("ext") and ty["ext"]["k"] == "record":
        return ty["ext"]
    return None


def safe_resolve(m: Any, type_name: str) -> dict[str, Any] | None:
    try:
        return m.env.resolve({"k": "named", "name": type_name, "args": []})
    except Exception:
        return None


# ---------------- hierarchies ----------------
def hierarchy_item(a: Analysis, m: Any, decl: dict[str, Any]) -> dict[str, Any]:
    lines = text_of(a, m).split("\n")
    return {
        "name": decl["name"],
        "kind": SYMBOL_KIND.get(decl["d"], 13),
        "uri": uri_of(m.path),
        "range": range_of(decl["loc"], lines),
        "selectionRange": range_of(name_range(text_of(a, m), decl, decl["name"]), lines),
    }


def prepare_hierarchy(uri: str, pos: dict[str, Any], want: str) -> Any:
    a = analysis_of(uri)
    if a is None:
        return None
    s = site_at(a, uri, _pos_of(uri, pos))
    if not s or s["site"] is None or s["site"].decl is None or s["site"].kind != want:
        return None
    return [hierarchy_item(a, s["site"].module, s["site"].decl)]


def module_of_uri(uri: str) -> tuple[Any, ...] | None:
    for a in list(last_good.values()):
        m = next((x for x in a.run.modules if uri_of(x.path) == uri), None)
        if m is not None:
            return a, m
    return None


def decl_containing(m: Any, loc: dict[str, Any]) -> dict[str, Any] | None:
    return next(
        (
            d
            for d in m.decls
            if d.get("loc")
            and d["loc"]["sl"] <= loc["sl"]
            and loc["el"] <= d["loc"]["el"]
            and isinstance(d.get("name"), str)
        ),
        None,
    )


def _resolved_site(a: Analysis, m: Any, t: dict[str, Any], fn: dict[str, Any]) -> Site | None:
    tg = t["res"].get(id(fn))
    if tg is None:
        tg = resolve_in(m.env, fn["name"])
    return site_of_target(a, tg)


def incoming_calls(item: dict[str, Any]) -> list[Any]:
    found = module_of_uri(item["uri"])
    if found is None:
        return []
    a, _ = found
    out: list[Any] = []
    for m in a.run.modules:
        t = tables_of(a, m)
        lines = text_of(a, m).split("\n")
        by_caller: dict[int, Any] = {}
        callers: dict[int, Any] = {}

        def visit(
            x: Any,
            m: Any = m,
            t: Any = t,
            callers: dict[int, Any] = callers,
            by_caller: dict[int, Any] = by_caller,
        ) -> None:
            if not x or not isinstance(x, (dict, list)):
                return
            if isinstance(x, list):
                for y in x:
                    visit(y)
                return
            if is_expr(x) and x["e"] == "call" and x["fn"]["e"] == "name" and x["fn"].get("loc"):
                site = _resolved_site(a, m, t, x["fn"])
                if (
                    site is not None
                    and site.decl is not None
                    and uri_of(site.module.path) == item["uri"]
                    and site.decl["loc"]["sl"] == item["range"]["start"]["line"]
                ):
                    caller = decl_containing(m, x["fn"]["loc"])
                    if caller is not None:
                        callers.setdefault(id(caller), caller)
                        by_caller.setdefault(id(caller), []).append(x["fn"]["loc"])
            for k, v in x.items():
                if k != "loc" and v and isinstance(v, (dict, list)):
                    visit(v)

        visit(m.decls)
        for key, locs in by_caller.items():
            out.append(
                {
                    "from": hierarchy_item(a, m, callers[key]),
                    "fromRanges": [range_of(l, lines) for l in locs],
                }
            )
    return out


def outgoing_calls(item: dict[str, Any]) -> list[Any]:
    found = module_of_uri(item["uri"])
    if found is None:
        return []
    a, m = found
    t = tables_of(a, m)
    lines = text_of(a, m).split("\n")
    decl = next(
        (d for d in m.decls if d.get("loc") and d["loc"]["sl"] == item["range"]["start"]["line"]),
        None,
    )
    if decl is None:
        return []
    by_callee: dict[str, Any] = {}

    def visit(x: Any) -> None:
        if not x or not isinstance(x, (dict, list)):
            return
        if isinstance(x, list):
            for y in x:
                visit(y)
            return
        if is_expr(x) and x["e"] == "call" and x["fn"]["e"] == "name" and x["fn"].get("loc"):
            site = _resolved_site(a, m, t, x["fn"])
            if site is not None and site.decl is not None and site.decl["d"] == "func":
                key = f"{site.module.path}:{site.decl['loc']['sl']}"
                e = by_callee.setdefault(
                    key, {"to": hierarchy_item(a, site.module, site.decl), "locs": []}
                )
                e["locs"].append(x["fn"]["loc"])
        for k, v in x.items():
            if k != "loc" and v and isinstance(v, (dict, list)):
                visit(v)

    visit(decl)
    return [
        {"to": e["to"], "fromRanges": [range_of(l, lines) for l in e["locs"]]}
        for e in by_callee.values()
    ]


def supertypes(item: dict[str, Any]) -> list[Any]:
    found = module_of_uri(item["uri"])
    if found is None:
        return []
    a, m = found
    decl = next(
        (
            d
            for d in m.decls
            if d["d"] == "type"
            and d.get("loc")
            and d["loc"]["sl"] == item["range"]["start"]["line"]
        ),
        None,
    )
    base = (
        decl["type"]["name"]
        if decl is not None
        and decl.get("type")
        and decl["type"].get("k") == "named"
        and decl["type"].get("ext")
        else None
    )
    if not base:
        return []
    site = site_of_target(a, resolve_in(m.env, base))
    return (
        [hierarchy_item(a, site.module, site.decl)]
        if site is not None and site.decl is not None
        else []
    )


def subtypes(item: dict[str, Any]) -> list[Any]:
    found = module_of_uri(item["uri"])
    if found is None:
        return []
    a, _ = found
    out: list[Any] = []
    for m in a.run.modules:
        for d in m.decls:
            if (
                d["d"] != "type"
                or not d.get("loc")
                or not d.get("type")
                or d["type"].get("k") != "named"
                or not d["type"].get("ext")
            ):
                continue
            site = site_of_target(a, resolve_in(m.env, d["type"]["name"]))
            if (
                site is not None
                and site.decl is not None
                and uri_of(site.module.path) == item["uri"]
                and site.decl["loc"]["sl"] == item["range"]["start"]["line"]
            ):
                out.append(hierarchy_item(a, m, d))
    return out


# ---------------- code actions ----------------
def placeholder_for(rt: dict[str, Any] | None) -> str:
    r = rt["base"] if rt and rt.get("t") == "pred" else rt
    if not r:
        return "null"
    t = r.get("t")
    if t == "prim":
        return (
            '""'
            if r["name"] == "string"
            else "0"
            if r["name"] == "int"
            else "0.0"
            if r["name"] == "float"
            else "false"
            if r["name"] == "bool"
            else "null"
        )
    if t == "lit":
        return json.dumps(r["v"], ensure_ascii=False) if isinstance(r["v"], str) else js_str(r["v"])
    if t == "range":
        return js_str(r["lo"])
    if t == "rec":
        return "{ }"
    if t == "arr":
        return "[]"
    if t == "map":
        return "{}"
    if t == "union":
        return placeholder_for(r["arms"][0])
    return "null"


def _relative_path(from_dir: str, to: str) -> str:
    f = [x for x in from_dir.split("/") if x]
    t = [x for x in to.split("/") if x]
    i = 0
    while i < len(f) and i < len(t) and f[i] == t[i]:
        i += 1
    return "/".join([".."] * (len(f) - i) + t[i:])


def _require_rel(from_: str, to: str) -> str:
    rel = _relative_path(os.path.dirname(from_), to)
    return re.sub(r"^\./", "", rel) if rel.startswith(".") else rel


def exporters_of(a: Analysis, m: Any, name: str) -> list[Any]:
    """the modules that export a name: the universe's, the other open
    documents' universes, then the .decl files beside the module"""
    out: list[Any] = []
    seen: set[Any] = {m.path}

    def consider(mod: Any) -> None:
        if mod.path not in seen and name in mod.exports:
            seen.add(mod.path)
            out.append(mod.path)

    for mod in a.run.modules:
        consider(mod)
    for other in list(last_good.values()):
        for mod in other.run.modules:
            consider(mod)
    try:
        names = os.listdir(os.path.dirname(m.path))
    except OSError:
        names = []
    for f in sorted(names):
        if not f.endswith(".decl"):
            continue
        p = os.path.normpath(os.path.join(os.path.dirname(m.path), f))
        if p in seen:
            continue
        text = overlay[p] if p in overlay else read_text(p)
        parsed = parse_source(text)
        if parsed["errors"]:
            continue
        if any(
            d.get("exported") and d.get("name") == name and d["d"] != "import"
            for d in parsed["decls"]
        ):
            seen.add(p)
            out.append(p)
    return out


def _find_all(x: Any, pred: Callable[[Any], bool], out: list[Any]) -> None:
    if not x or not isinstance(x, (dict, list)):
        return
    if isinstance(x, list):
        for y in x:
            _find_all(y, pred, out)
        return
    if pred(x):
        out.append(x)
    for k, v in x.items():
        if k != "loc" and v and isinstance(v, (dict, list)):
            _find_all(v, pred, out)


def _widen(rt: Any) -> Any:
    """a literal's type widened to its primitive: a declaration wants `bool`, not `true`"""
    if rt and rt.get("t") == "lit":
        v = rt["v"]
        name = (
            "string"
            if isinstance(v, str)
            else "bool"
            if isinstance(v, bool)
            else "int"
            if isinstance(v, int)
            else "float"
            if isinstance(v, float)
            else "null"
        )
        return {"t": "prim", "name": name}
    return rt


def _mentions_name(x: Any) -> bool:
    if not x or not isinstance(x, (dict, list)):
        return False
    if isinstance(x, list):
        return any(_mentions_name(y) for y in x)
    return (is_expr(x) and x["e"] in ("name", "ctx", "referrers")) or any(
        k != "loc" and _mentions_name(v) for k, v in x.items()
    )


def code_actions(uri: str, range_: dict[str, Any], diagnostics: list[Any]) -> list[Any]:
    text = docs.get(uri)
    if text is None:
        return []
    a = analysis_of(uri)
    out: list[Any] = []
    parsed = parse_source(text)
    decls, errors = parsed["decls"], parsed["errors"]
    lines = text.split("\n")

    def at(line: int, byte_col: int) -> dict[str, Any]:
        return {"line": line, "character": _u16_col(_line(lines, line), byte_col)}

    def insert_at(p: dict[str, Any], new_text: str) -> dict[str, Any]:
        return {"range": {"start": p, "end": p}, "newText": new_text}

    def chain_at(p: dict[str, Any]) -> list[Any]:
        hit = node_at(decls, p)
        return [hit["node"], *hit["parents"][::-1]] if hit else []

    if a is not None and not errors:
        m = module_of(a, path_of(uri))
        t = tables_of(a, m)

        # the fixes of the diagnostics that touch the range (a client may send more)
        def touches(d: dict[str, Any]) -> bool:
            return bool(d.get("range")) and not (
                d["range"]["end"]["line"] < range_["start"]["line"]
                or d["range"]["start"]["line"] > range_["end"]["line"]
            )

        for d in [d for d in (diagnostics or []) if touches(d)]:
            msg = d.get("message") or ""
            dstart = pos_bytes(d["range"]["start"], lines)
            mm = re.match(r"^unknown name ([A-Za-z_][A-Za-z0-9_]*)", msg)
            if mm:
                name = mm.group(1)
                for other in exporters_of(a, m, name):
                    spec = "./" + _require_rel(m.path, other)
                    existing = next(
                        (
                            x
                            for x in decls
                            if x["d"] == "import"
                            and x.get("names")
                            and os.path.normpath(os.path.join(os.path.dirname(m.path), x["from"]))
                            == other
                        ),
                        None,
                    )
                    if existing is not None:
                        close = _bfind(
                            _line(lines, existing["loc"]["sl"]), "}", existing["loc"]["sc"]
                        )
                        edit = insert_at(at(existing["loc"]["sl"], close), f", {name} ")
                        spec = existing["from"]
                    else:
                        last_import = next(
                            (x for x in reversed(decls) if x["d"] in ("import", "re_export")), None
                        )
                        at_line = (
                            last_import["loc"]["el"] + 1
                            if last_import is not None and last_import.get("loc")
                            else 0
                        )
                        edit = insert_at(
                            {"line": at_line, "character": 0},
                            f'import {{ {name} }} from "{spec}"\n',
                        )
                    out.append(
                        {
                            "title": f'import {name} from "{spec}"',
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {"changes": {uri: [edit]}},
                        }
                    )
            mm = re.match(r"^unknown name ([A-Za-z_][A-Za-z0-9_]*)", msg)
            if mm:
                # a namespace import that exports it: qualify the name
                name = mm.group(1)
                for ns, entry in m.env.namespaces.items():
                    if name not in entry["exports"]:
                        continue
                    n = next(
                        (
                            x
                            for x in chain_at(dstart)
                            if is_expr(x) and x["e"] == "name" and x["name"] == name
                        ),
                        None,
                    )
                    if n is not None and n.get("loc"):
                        out.append(
                            {
                                "title": f"qualify as {ns}.{name}",
                                "kind": "quickfix",
                                "diagnostics": [d],
                                "edit": {
                                    "changes": {
                                        uri: [
                                            {
                                                "range": range_of(n["loc"], lines),
                                                "newText": f"{ns}.{name}",
                                            }
                                        ]
                                    }
                                },
                            }
                        )
            mm = re.match(
                r"^member ([A-Za-z_][A-Za-z0-9_]*) is not declared on ([A-Za-z_][A-Za-z0-9_]*)$",
                msg,
            )
            if mm:
                # declare the member on the type, with the supplied value's inferred type
                name, type_name = mm.group(1), mm.group(2)
                site = site_of_target(a, resolve_in(m.env, type_name))
                decl = site.decl if site is not None else None
                body = None
                if decl is not None and decl["d"] == "type":
                    ty0 = decl["type"]
                    body = (
                        ty0
                        if ty0["k"] == "record"
                        else ty0["ext"]
                        if ty0["k"] == "named" and ty0.get("ext") and ty0["ext"]["k"] == "record"
                        else None
                    )
                if site is not None and body is not None:
                    hit = node_at(decls, dstart)
                    chain = [hit["node"], *hit["parents"][::-1]] if hit else []
                    obj0 = next((n for n in chain if is_expr(n) and n["e"] == "obj"), None)
                    entry = (
                        next((en for en in obj0["entries"] if en["key"] == name), None)
                        if obj0 is not None
                        else None
                    )
                    if entry is None and hit is not None:
                        objs: list[Any] = []
                        _find_all(hit["node"], lambda n: is_expr(n) and n["e"] == "obj", objs)
                        entry = next(
                            (en for o in objs for en in o["entries"] if en["key"] == name), None
                        )
                    ty = t["types"].get(id(entry["val"])) if entry is not None else None
                    member_type = type_text(_widen(ty["rt"])) if ty and ty.get("rt") else "any"
                    last = body["members"][-1] if body["members"] else None
                    tlines = text_of(a, site.module).split("\n")
                    if last is not None and last.get("loc"):
                        p = {
                            "line": last["loc"]["el"],
                            "character": _u16_col(
                                _line(tlines, last["loc"]["el"]), last["loc"]["ec"]
                            ),
                        }
                        new_text = f"\n    {name}: {member_type}"
                    else:
                        p = {
                            "line": body["loc"]["sl"],
                            "character": _u16_col(
                                _line(tlines, body["loc"]["sl"]), body["loc"]["sc"] + 1
                            ),
                        }
                        new_text = f" {name}: {member_type}"
                    out.append(
                        {
                            "title": f"declare {name}: {member_type} on {type_name}",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {
                                "changes": {uri_of(site.module.path): [insert_at(p, new_text)]}
                            },
                        }
                    )
            if msg.startswith("member access on a maybe-absent expression"):
                n = next(
                    (
                        x
                        for x in chain_at(dstart)
                        if is_expr(x) and x["e"] == "member" and not x.get("safe")
                    ),
                    None,
                )
                if n is not None and n.get("loc"):
                    tok = member_token_loc(text, n)
                    out.append(
                        {
                            "title": "use ?.",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {
                                "changes": {
                                    uri: [
                                        {
                                            "range": {
                                                "start": at(tok["sl"], tok["sc"] - 1),
                                                "end": at(tok["sl"], tok["sc"]),
                                            },
                                            "newText": "?.",
                                        }
                                    ]
                                }
                            },
                        }
                    )
            if msg.startswith("maybe-absent expression consumed"):
                hit = node_at(decls, dstart)
                n = hit["node"] if hit else None
                if n is not None and is_expr(n) and n.get("loc"):
                    end = at(n["loc"]["el"], n["loc"]["ec"])
                    out.append(
                        {
                            "title": "supply a fallback with ??",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "edit": {"changes": {uri: [insert_at(end, " ?? null")]}},
                        }
                    )
            if msg.startswith("`??` mixed with"):
                # parenthesize the `??` operand of the mixed expression
                hit = node_at(decls, dstart)
                found: dict[str, Any] = {"target": None}

                def logical(op: Any) -> bool:
                    return op in ("&&", "||")

                def find(x: Any, parent_op: Any, found: dict[str, Any] = found) -> None:
                    if not x or not isinstance(x, (dict, list)) or found["target"] is not None:
                        return
                    if isinstance(x, list):
                        for y in x:
                            find(y, parent_op)
                        return
                    if (
                        is_expr(x)
                        and x["e"] == "bin"
                        and x.get("loc")
                        and parent_op
                        and (
                            (x["op"] == "??" and logical(parent_op))
                            or (logical(x["op"]) and parent_op == "??")
                        )
                    ):
                        found["target"] = x
                        return
                    op = x["op"] if is_expr(x) and x["e"] == "bin" else None
                    for k, v in x.items():
                        if k != "loc" and v and isinstance(v, (dict, list)):
                            find(v, op)

                if hit is not None:
                    find(hit["node"], None)
                target = found["target"]
                if target is not None:
                    tl = target["loc"]
                    out.append(
                        {
                            "title": "parenthesize the ?? expression",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {
                                "changes": {
                                    uri: [
                                        insert_at(at(tl["sl"], tl["sc"]), "("),
                                        insert_at(at(tl["el"], tl["ec"]), ")"),
                                    ]
                                }
                            },
                        }
                    )
            if msg.startswith("`match` is not exhaustive"):
                n = next((x for x in chain_at(dstart) if is_expr(x) and x["e"] == "match"), None)
                subject = (
                    (t["types"].get(id(n["subject"])) or {}).get("rt") if n is not None else None
                )
                arms = (
                    [r["name"] for r in subject["arms"] if r.get("t") == "rec" and r.get("name")]
                    if subject and subject.get("t") == "union"
                    else []
                )
                covered = (
                    set(
                        (
                            arm["type"]["name"]
                            if arm.get("type") and arm["type"].get("k") == "named"
                            else ""
                        )
                        for arm in n["arms"]
                    )
                    if n is not None
                    else set()
                )
                missing = [x for x in arms if x not in covered]
                if n is not None and n.get("loc") and missing:
                    nl = n["loc"]
                    p = at(nl["el"], nl["ec"] - 1)
                    indent = " " * (_indent_width(_line(lines, nl["sl"])) + 4)
                    out.append(
                        {
                            "title": f"add the missing arm{'s' if len(missing) > 1 else ''}: "
                            "{', '.join(missing)}",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {
                                "changes": {
                                    uri: [
                                        insert_at(
                                            p,
                                            "".join(f"{indent}(v: {x}) => null\n" for x in missing)
                                            + indent[4:],
                                        )
                                    ]
                                }
                            },
                        }
                    )
            mm = re.match(
                r"^(\$[a-z]+) used without a context declaration in ([A-Za-z_][A-Za-z0-9_]*)$", msg
            )
            if mm:
                # declare the context variable on the type: `$parent: ref<{ ... }>`, `$root: ref<{
                # ... }>`, `$key: string`
                variable, type_name = mm.group(1), mm.group(2)
                site = site_of_target(a, resolve_in(m.env, type_name))
                body = record_body_of(site.decl) if site is not None else None
                if site is not None and body is not None and body.get("loc"):
                    bound = "string" if variable == "$key" else "ref<{ ... }>"
                    first = body["members"][0] if body["members"] else None
                    tlines = text_of(a, site.module).split("\n")
                    if first is not None and first.get("loc"):
                        p = {
                            "line": first["loc"]["sl"],
                            "character": _u16_col(
                                _line(tlines, first["loc"]["sl"]), first["loc"]["sc"]
                            ),
                        }
                    else:
                        p = {
                            "line": body["loc"]["sl"],
                            "character": _u16_col(
                                _line(tlines, body["loc"]["sl"]), body["loc"]["sc"] + 1
                            ),
                        }
                    indent = "" if first is not None and first.get("loc") else " "
                    if (
                        first is not None
                        and first.get("loc")
                        and first["loc"]["sl"] > body["loc"]["sl"]
                    ):
                        new_text = f"{variable}: {bound}\n{' ' * first['loc']['sc']}"
                    else:
                        new_text = f"{indent}{variable}: {bound}, "
                    out.append(
                        {
                            "title": f"declare {variable}: {bound} on {type_name}",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {
                                "changes": {uri_of(site.module.path): [insert_at(p, new_text)]}
                            },
                        }
                    )
            mm = re.match(
                r"^(\$[a-z]+) declaration must be ref<\.\.\.> \(([A-Za-z_][A-Za-z0-9_]*)\)$", msg
            )
            if mm:
                # the declared bound wrapped in ref<…>
                variable, type_name = mm.group(1), mm.group(2)
                site = site_of_target(a, resolve_in(m.env, type_name))
                body = record_body_of(site.decl) if site is not None else None
                ctxm = (
                    next(
                        (
                            x
                            for x in body["members"]
                            if x["m"] == "context" and x.get("variable") == variable
                        ),
                        None,
                    )
                    if body is not None
                    else None
                )
                if (
                    site is not None
                    and ctxm is not None
                    and ctxm.get("type")
                    and ctxm["type"].get("loc")
                ):
                    tl = ctxm["type"]["loc"]
                    ttext = text_of(a, site.module)
                    tsrc = _src_of(ttext, tl)
                    out.append(
                        {
                            "title": f"declare {variable} as ref<{tsrc}>",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {
                                "changes": {
                                    uri_of(site.module.path): [
                                        {
                                            "range": range_of(tl, ttext.split("\n")),
                                            "newText": f"ref<{tsrc}>",
                                        }
                                    ]
                                }
                            },
                        }
                    )
            mm = re.match(
                r"^(?:illegal member-kind transition for|override widens inherited member) "
                r"([A-Za-z_][A-Za-z0-9_]*)[^(]*\(([A-Za-z_][A-Za-z0-9_]*)\)$",
                msg,
            )
            if mm:
                # the override replaced by the parent's declaration of the member
                member_name, type_name = mm.group(1), mm.group(2)
                site = site_of_target(a, resolve_in(m.env, type_name))
                decl = site.decl if site is not None else None
                body = record_body_of(decl)
                own = (
                    next(
                        (
                            x
                            for x in body["members"]
                            if x.get("name") == member_name and x.get("loc")
                        ),
                        None,
                    )
                    if body is not None
                    else None
                )
                parent = (
                    member_site(
                        a, site.module, safe_resolve(site.module, decl["type"]["name"]), member_name
                    )
                    if site is not None and decl is not None and decl["type"]["k"] == "named"
                    else None
                )
                if (
                    site is not None
                    and own is not None
                    and parent is not None
                    and parent.member is not None
                    and parent.member.get("loc")
                ):
                    parent_text = re.sub(
                        r",\s*$", "", _src_of(text_of(a, parent.module), parent.member["loc"])
                    )
                    out.append(
                        {
                            "title": f"use the parent's declaration: {parent_text}",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {
                                "changes": {
                                    uri_of(site.module.path): [
                                        {
                                            "range": range_of(
                                                own["loc"], text_of(a, site.module).split("\n")
                                            ),
                                            "newText": parent_text,
                                        }
                                    ]
                                }
                            },
                        }
                    )
            mm = re.match(r"^record union arms not discriminable in ([A-Za-z_][A-Za-z0-9_]*)$", msg)
            if mm:
                # a literal-typed `kind` member on every arm that is a local record type
                site = site_of_target(a, resolve_in(m.env, mm.group(1)))
                u = site.decl if site is not None else None
                arms = (
                    u["type"]["arms"]
                    if u is not None and u.get("type") and u["type"]["k"] == "union"
                    else []
                )
                changes: dict[str, Any] = {}
                n = 0
                for arm in arms:
                    if arm["k"] != "named":
                        continue
                    assert site is not None  # the arms came from its declaration
                    as_ = site_of_target(a, resolve_in(site.module.env, arm["name"]))
                    body = record_body_of(as_.decl) if as_ is not None else None
                    first = body["members"][0] if body is not None and body["members"] else None
                    if (
                        as_ is None
                        or body is None
                        or not body.get("loc")
                        or any(x.get("name") == "kind" for x in body["members"])
                    ):
                        continue
                    tlines = text_of(a, as_.module).split("\n")
                    if first is not None and first.get("loc"):
                        p = {
                            "line": first["loc"]["sl"],
                            "character": _u16_col(
                                _line(tlines, first["loc"]["sl"]), first["loc"]["sc"]
                            ),
                        }
                    else:
                        p = {
                            "line": body["loc"]["sl"],
                            "character": _u16_col(
                                _line(tlines, body["loc"]["sl"]), body["loc"]["sc"] + 1
                            ),
                        }
                    multi = (
                        first is not None
                        and first.get("loc")
                        and first["loc"]["sl"] > body["loc"]["sl"]
                    )
                    lead = "" if first is not None and first.get("loc") else " "
                    first_sc = first["loc"]["sc"] if first is not None and first.get("loc") else 0
                    new_text = (
                        f'kind: "{arm["name"]}"\n{" " * first_sc}'
                        if multi
                        else f'{lead}kind: "{arm["name"]}", '
                    )
                    changes.setdefault(uri_of(as_.module.path), []).append(insert_at(p, new_text))
                    n += 1
                if n:
                    out.append(
                        {
                            "title": f"add a discriminant `kind` to the arms of {mm.group(1)}",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {"changes": changes},
                        }
                    )
            mm = re.match(
                r"^derived member ([A-Za-z_][A-Za-z0-9_]*) restated with a differing value", msg
            )
            if mm:
                # a document supplies it: make it defaulted (`x?: T = e`) where it is declared with
                # a type
                member_name = mm.group(1)
                for decl in m.decls:
                    body = record_body_of(decl)
                    own = (
                        next(
                            (
                                x
                                for x in body["members"]
                                if x["m"] == "derived"
                                and x.get("name") == member_name
                                and x.get("type")
                                and x.get("loc")
                            ),
                            None,
                        )
                        if body is not None
                        else None
                    )
                    if own is None:
                        continue
                    r = member_range(text, own, member_name)
                    out.append(
                        {
                            "title": f"make {decl['name']}.{member_name} defaulted (x?: T = e)",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "edit": {"changes": {uri: [insert_at(at(r["el"], r["ec"]), "?")]}},
                        }
                    )
            mm = re.match(r"^required member ([A-Za-z_][A-Za-z0-9_]*) missing", msg)
            if mm:
                name = mm.group(1)
                # the construction: the literal at the diagnostic, or the root's literal when the
                # diagnostic names the declaration
                hit = node_at(decls, dstart)
                chain = [hit["node"], *hit["parents"][::-1]] if hit else []
                obj = next((n for n in chain if is_expr(n) and n["e"] == "obj"), None)
                if obj is None:
                    for n in chain:
                        if is_decl(n):
                            e = (
                                n.get("expr")
                                if n["d"] == "output"
                                else n.get("fallback")
                                if n["d"] == "input"
                                else n.get("expr")
                            )
                            if is_expr(e) and e["e"] == "obj":
                                obj = e
                                break
                if obj is not None:
                    # the literal's type: its declared position (a root's annotation), else what
                    # inference recorded
                    owner = next(
                        (
                            n
                            for n in chain
                            if is_decl(n) and n["d"] in ("output", "input") and n.get("type")
                        ),
                        None,
                    )
                    try:
                        rt = (
                            m.env.resolve(owner["type"])
                            if owner is not None
                            else ((t["types"].get(id(obj)) or {}).get("rt"))
                        )
                    except Exception:
                        rt = None
                    mem = (
                        next((x for x in rt["members"] if x.get("name") == name), None)
                        if rt and rt.get("t") == "rec"
                        else None
                    )
                    value = placeholder_for(mem.get("type") if mem else None)
                    last = obj["entries"][-1] if obj["entries"] else None
                    if last is not None and last.get("val") and last["val"].get("loc"):
                        vl = last["val"]["loc"]
                        edit = insert_at(at(vl["el"], vl["ec"]), f", {name}: {value}")
                    else:
                        ol = obj["loc"]
                        edit = insert_at(at(ol["sl"], ol["sc"] + 1), f" {name}: {value}")
                    out.append(
                        {
                            "title": f"add {name}: {value}",
                            "kind": "quickfix",
                            "diagnostics": [d],
                            "isPreferred": True,
                            "edit": {"changes": {uri: [edit]}},
                        }
                    )

        # assists at the range
        rstart = pos_bytes(range_["start"], lines)
        rend = pos_bytes(range_["end"], lines)
        chain = chain_at(rstart)

        def one(title: str, kind: str, edits: list[Any], target: str = uri) -> None:
            out.append({"title": title, "kind": kind, "edit": {"changes": {target: edits}}})

        # annotate an unannotated derived member or constant with its inferred type
        unannotated = next(
            (
                n
                for n in chain
                if (is_member(n) and n["m"] == "derived" and not n.get("type"))
                or (is_decl(n) and n["d"] == "const" and not n.get("type"))
            ),
            None,
        )
        if unannotated is not None:
            ty = t["types"].get(id(unannotated["expr"]))
            if ty and ty.get("rt"):
                r = (
                    member_range(text, unannotated, unannotated["name"])
                    if is_member(unannotated)
                    else name_range(text, unannotated, unannotated["name"])
                )
                one(
                    f"annotate: {type_text(ty['rt'])}",
                    "refactor.rewrite",
                    [
                        insert_at(
                            at(r["el"], r["ec"] + (1 if unannotated.get("hidden") else 0)),
                            f": {type_text(ty['rt'])}",
                        )
                    ],
                )
        # convert a member's kind: derived <-> hidden, defaulted <-> derived, optional <-> required
        member = next(
            (n for n in chain if is_member(n) and n["m"] in ("derived", "value") and n.get("loc")),
            None,
        )
        if member is not None:
            r = member_range(text, member, member["name"])
            after_name = at(r["el"], r["ec"])
            after_next = at(r["el"], r["ec"] + 1)
            if member["m"] == "derived":
                if member.get("hidden"):
                    one(
                        "make visible (derived)",
                        "refactor.rewrite",
                        [{"range": {"start": after_name, "end": after_next}, "newText": ""}],
                    )
                else:
                    one("make hidden (x$)", "refactor.rewrite", [insert_at(after_name, "$")])
                if member.get("type"):
                    one(
                        "make defaulted (x?: T = e)",
                        "refactor.rewrite",
                        [insert_at(after_name, "?")],
                    )
            elif member.get("dflt"):
                one(
                    "make derived (x: T = e)",
                    "refactor.rewrite",
                    [{"range": {"start": after_name, "end": after_next}, "newText": ""}],
                )
            elif member.get("opt"):
                one(
                    "make required",
                    "refactor.rewrite",
                    [{"range": {"start": after_name, "end": after_next}, "newText": ""}],
                )
            else:
                one("make optional", "refactor.rewrite", [insert_at(after_name, "?")])
        # generate: export, an output or input skeleton for a type, the fixture header
        decl = next((n for n in chain if is_decl(n)), None)
        if (
            decl is not None
            and decl.get("loc")
            and isinstance(decl.get("name"), str)
            and not decl.get("exported")
            and decl["d"] not in ("import", "re_export")
        ):
            one(
                f"export {decl['name']}",
                "refactor.rewrite",
                [insert_at(at(decl["loc"]["sl"], decl["loc"]["sc"]), "export ")],
            )
        if decl is not None and decl["d"] == "type":
            try:
                rt = m.env.resolve(decl["type"])
            except Exception:
                rt = None
            if rt and rt.get("t") == "rec":
                req = [
                    f"{x['name']}: {placeholder_for(x.get('type'))}"
                    for x in rt["members"]
                    if x.get("kind") == "req"
                ]
                end = {"line": len(lines) - 1, "character": u16(lines[-1])}
                lead = "" if lines[-1] == "" else "\n"
                lower = decl["name"][0].lower() + decl["name"][1:]
                one(
                    f"generate an output of {decl['name']}",
                    "refactor.rewrite",
                    [
                        insert_at(
                            end,
                            f"{lead}output {lower}: {decl['name']} = "
                            f"{{ {', '.join(req)}{' ' if req else ''}}}\n",
                        )
                    ],
                )
                one(
                    f"generate an input of {decl['name']}",
                    "refactor.rewrite",
                    [insert_at(end, f"{lead}input {lower}: {decl['name']}\n")],
                )
        if diagnostics and not re.match(r"^// @expect-", _line(lines, 0)):
            first = next((d for d in diagnostics if d.get("severity") == 1), diagnostics[0])
            code = str(first["code"]) if first.get("code") is not None else ""
            phase = (
                "parsing"
                if code.startswith(("E1", "E2"))
                else "binding"
                if code.startswith(("E5", "E6")) or re.match(r"^[A-Z][A-Za-z0-9_]*\.", code)
                else "checking"
            )
            one(
                "generate the fixture header (@expect-phase / @expect-error)",
                "refactor.rewrite",
                [
                    insert_at(
                        {"line": 0, "character": 0},
                        f"// @expect-phase: {phase}\n// @expect-error: {code}\n",
                    )
                ],
            )
        # fill the missing required members of a literal
        obj = next((n for n in chain if is_expr(n) and n["e"] == "obj"), None)
        if obj is not None and obj.get("loc"):
            owner = next(
                (
                    n
                    for n in chain
                    if is_decl(n) and n["d"] in ("output", "input") and n.get("type")
                ),
                None,
            )
            try:
                rt = (
                    m.env.resolve(owner["type"])
                    if owner is not None and owner.get("expr") is obj
                    else ((t["types"].get(id(obj)) or {}).get("rt"))
                )
            except Exception:
                rt = None
            if rt and rt.get("t") == "rec":
                have = set(en["key"] for en in obj["entries"])
                missing = [
                    x for x in rt["members"] if x.get("kind") == "req" and x["name"] not in have
                ]
                if missing:
                    last = obj["entries"][-1] if obj["entries"] else None
                    fill = ", ".join(
                        f"{x['name']}: {placeholder_for(x.get('type'))}" for x in missing
                    )
                    if last is not None and last.get("val") and last["val"].get("loc"):
                        edit = insert_at(
                            at(last["val"]["loc"]["el"], last["val"]["loc"]["ec"]), f", {fill}"
                        )
                    else:
                        edit = insert_at(at(obj["loc"]["sl"], obj["loc"]["sc"] + 1), f" {fill}")
                    one(
                        f"fill the required members: {', '.join(x['name'] for x in missing)}",
                        "refactor.rewrite",
                        [edit],
                    )
        # inline a constant: its expression at every use, the declaration gone
        if (
            decl is not None
            and decl["d"] == "const"
            and decl.get("loc")
            and decl.get("expr")
            and decl["expr"].get("loc")
        ):
            nr = name_range(text, decl, decl["name"])
            refs = [
                r for r in references(uri, at(nr["sl"], nr["sc"]), False) if r[0].path == m.path
            ]
            if refs:
                src = _src_of(text, decl["expr"]["loc"])
                plain = is_expr(decl["expr"]) and decl["expr"]["e"] in (
                    "name",
                    "lit",
                    "unitlit",
                    "call",
                    "member",
                    "paren",
                )
                edits = [
                    {"range": range_of(r[1], lines), "newText": src if plain else f"({src})"}
                    for r in refs
                ]
                edits.append(
                    {
                        "range": {
                            "start": {"line": decl["loc"]["sl"], "character": 0},
                            "end": {"line": decl["loc"]["el"] + 1, "character": 0},
                        },
                        "newText": "",
                    }
                )
                one(f"inline {decl['name']}", "refactor.inline", edits)
        # extract an inline record type into a named type
        own_body = record_body_of(decl) if decl is not None else None
        inline_record = next(
            (
                n
                for n in chain
                if is_type(n) and n["k"] == "record" and n.get("loc") and n is not own_body
            ),
            None,
        )
        if inline_record is None:
            mem = next(
                (
                    n
                    for n in chain
                    if is_member(n)
                    and n["m"] == "value"
                    and n.get("type")
                    and n["type"].get("k") == "record"
                    and n["type"].get("loc")
                ),
                None,
            )
            inline_record = mem["type"] if mem is not None else None
        if inline_record is not None and decl is not None and decl.get("loc"):
            one(
                "extract to a named type",
                "refactor.extract",
                [
                    {
                        "range": {
                            "start": {"line": decl["loc"]["sl"], "character": 0},
                            "end": {"line": decl["loc"]["sl"], "character": 0},
                        },
                        "newText": f"type Extracted = {_src_of(text, inline_record['loc'])}\n",
                    },
                    {"range": range_of(inline_record["loc"], lines), "newText": "Extracted"},
                ],
            )
        # a unit literal in its base unit
        unit_lit = next(
            (n for n in chain if is_expr(n) and n["e"] == "unitlit" and n.get("loc")), None
        )
        if unit_lit is not None:
            try:
                u = m.env.unit_info(unit_lit["unit"])
                base = m.env.base_unit_of.get(u["key"], u["key"])
                if base != unit_lit["unit"]:
                    converted = f"{js_num_str(unit_lit['num'] * u['to_base'])}{base}"
                    one(
                        f"convert to {converted}",
                        "refactor.rewrite",
                        [{"range": range_of(unit_lit["loc"], lines), "newText": converted}],
                    )
            except Exception:
                pass  # an unknown unit is a diagnostic
        # reorder a record's members into the canonical order
        type_decl = next((n for n in chain if is_decl(n) and n["d"] == "type"), None)
        body = record_body_of(type_decl) if type_decl is not None else None
        if (
            body is not None
            and body.get("loc")
            and len(body["members"]) > 1
            and all(x.get("loc") for x in body["members"])
        ):

            def rank(x: dict[str, Any]) -> int:
                if x["m"] == "context":
                    return 0
                if x["m"] == "value":
                    return 3 if x.get("dflt") else 2 if x.get("opt") else 1
                if x["m"] == "derived":
                    return 5 if x.get("hidden") else 4
                return 6

            ordered = sorted(enumerate(body["members"]), key=lambda p: (rank(p[1]), p[0]))
            if any(s[0] != i for i, s in enumerate(ordered)):
                lead_n = _indent_width(_line(lines, body["loc"]["sl"]))
                indent = " " * (lead_n + 4)
                close_s = " " * lead_n
                members = [
                    indent + re.sub(r",\s*$", "", _src_of(text, s[1]["loc"])) for s in ordered
                ]
                trailing = f"\n{indent}..." if body.get("open") else ""
                one(
                    "reorder the members canonically",
                    "refactor.rewrite",
                    [
                        {
                            "range": range_of(body["loc"], lines),
                            "newText": "{\n" + "\n".join(members) + trailing + "\n" + close_s + "}",
                        }
                    ],
                )

        # an assert's inline `else error …` into a diagnostic declaration, and back
        def pos_in(src: str, loc: dict[str, Any], offset: int) -> dict[str, Any]:
            # the LSP position `offset` characters into a node's source text
            head = src[:offset]
            nl = head.count("\n")
            if nl:
                return at(loc["sl"] + nl, len(head[head.rfind("\n") + 1 :].encode("utf-8")))
            return at(loc["sl"], loc["sc"] + len(head.encode("utf-8")))

        assert_member = next(
            (n for n in chain if is_member(n) and n["m"] == "assert" and n.get("loc")), None
        )
        if (
            assert_member is not None
            and assert_member.get("tail")
            and type_decl is not None
            and type_decl.get("loc")
        ):
            tail = assert_member["tail"]
            if tail["t"] == "inline":
                # the names the template reads become the parameters, typed as inferred
                names: list[Any] = []
                params: list[Any] = []
                for part in tail["template"]:
                    if isinstance(part, str):
                        continue
                    name_nodes: list[Any] = []
                    _find_all(part, lambda n: is_expr(n) and n["e"] == "name", name_nodes)
                    for nn in name_nodes:
                        if nn["name"] not in names:
                            names.append(nn["name"])
                            ty = t["types"].get(id(nn))
                            params.append(
                                f"{nn['name']}: "
                                f"{type_text(ty['rt']) if ty and ty.get('rt') else 'any'}"
                            )
                template = (
                    "`"
                    + "".join(
                        p if isinstance(p, str) else "${" + expr_text(p) + "}"
                        for p in tail["template"]
                    )
                    + "`"
                )
                src = _src_of(text, assert_member["loc"])
                else_m = re.search(r"\belse\b", src)
                if else_m:
                    tail_start = pos_in(src, assert_member["loc"], else_m.start())
                    one(
                        f"declare a diagnostic for {assert_member['name']}",
                        "refactor.extract",
                        [
                            {
                                "range": {
                                    "start": {"line": type_decl["loc"]["sl"], "character": 0},
                                    "end": {"line": type_decl["loc"]["sl"], "character": 0},
                                },
                                "newText": f"diagnostic {assert_member['name']}"
                                f"({', '.join(params)}) {{\n"
                                f"    severity = {tail['severity']}\n"
                                f"    message = {template}\n}}\n",
                            },
                            {
                                "range": {
                                    "start": tail_start,
                                    "end": at(
                                        assert_member["loc"]["el"], assert_member["loc"]["ec"]
                                    ),
                                },
                                "newText": f"else {assert_member['name']}({', '.join(names)})",
                            },
                        ],
                    )
            elif tail["t"] == "ref":
                dd = m.env.diags.get(tail["name"])
                ddecl = next(
                    (d for d in m.decls if d["d"] == "diagnostic" and d["name"] == tail["name"]),
                    None,
                )
                if dd is not None and ddecl is not None:
                    arg_text = [
                        _src_of(text, a_["loc"]) if a_.get("loc") else expr_text(a_)
                        for a_ in tail["args"]
                    ]

                    def part_text(p: Any) -> str:
                        if isinstance(p, str):
                            return p
                        i = (
                            next(
                                (k for k, q in enumerate(dd["params"]) if q["name"] == p["name"]),
                                -1,
                            )
                            if p["e"] == "name"
                            else -1
                        )
                        return "${" + (arg_text[i] if i >= 0 else expr_text(p)) + "}"

                    message = "`" + "".join(part_text(p) for p in dd["template"]) + "`"
                    src = _src_of(text, assert_member["loc"])
                    else_m = re.search(r"\belse\b", src)
                    if else_m:
                        tail_start = pos_in(src, assert_member["loc"], else_m.start())
                        one(
                            f"inline the diagnostic {tail['name']}",
                            "refactor.inline",
                            [
                                {
                                    "range": {
                                        "start": tail_start,
                                        "end": at(
                                            assert_member["loc"]["el"], assert_member["loc"]["ec"]
                                        ),
                                    },
                                    "newText": f"else {dd['severity']} {message}",
                                }
                            ],
                        )
        # an `if` chain over a discriminant into a `match`, and back
        if_chain = next((n for n in chain if is_expr(n) and n["e"] == "if" and n.get("loc")), None)
        if if_chain is not None:
            # conditions `subject.member == "lit"` on one subject and member, the union's arms
            # telling which record each literal picks
            arms = []
            subject = None
            member = None
            tail_expr = None
            ok = True
            n = if_chain
            while True:
                if n["e"] != "if":
                    tail_expr = n
                    break
                c = n["c"]
                if (
                    c["e"] == "bin"
                    and c["op"] == "=="
                    and c["l"]["e"] == "member"
                    and c["r"]["e"] == "lit"
                    and isinstance(c["r"]["v"], str)
                ):
                    if subject is None:
                        subject, member = c["l"]["x"], c["l"]["name"]
                    elif expr_text(subject) != expr_text(c["l"]["x"]) or member != c["l"]["name"]:
                        ok = False
                        break
                    arms.append((c["r"]["v"], n["t"]))
                else:
                    ok = False
                    break
                n = n["f"]
            sty = t["types"].get(id(subject)) if subject is not None else None
            srt = sty["rt"] if sty else None
            if ok and arms and srt and srt.get("t") == "union":
                srt_arms = srt["arms"]

                def arm_name(lit: str) -> str | None:
                    r = next(
                        (
                            r
                            for r in srt_arms
                            if r.get("t") == "rec"
                            and any(
                                mm["name"] == member
                                and (mm.get("type") or {}).get("t") == "lit"
                                and mm["type"].get("v") == lit
                                for mm in r["members"]
                            )
                        ),
                        None,
                    )
                    return r.get("name") if r else None

                names2 = [arm_name(lit) for lit, _ in arms]
                if all(names2):
                    indent = " " * (_indent_width(_line(lines, if_chain["loc"]["sl"])) + 4)
                    v = "v"
                    if subject is not None and subject["e"] == "name":
                        v = subject["name"][0]
                    subj_re = re.compile(r"\b" + re.escape(expr_text(subject)) + r"\b")
                    cases = [
                        f"{indent}({v}: {names2[i]}) => "
                        f"{subj_re.sub(v, _src_of(text, body_['loc']))}"
                        for i, (_, body_) in enumerate(arms)
                    ]
                    if tail_expr is not None and tail_expr.get("loc"):
                        cases.append(f"{indent}(other) => {_src_of(text, tail_expr['loc'])}")
                    one(
                        "convert to match",
                        "refactor.rewrite",
                        [
                            {
                                "range": range_of(if_chain["loc"], lines),
                                "newText": f"match {expr_text(subject)} {{\n"
                                + "\n".join(cases)
                                + "\n"
                                + indent[4:]
                                + "}",
                            }
                        ],
                    )
        match_expr = next(
            (n for n in chain if is_expr(n) and n["e"] == "match" and n.get("loc")), None
        )
        if match_expr is not None:
            # arms typed by records that a literal member discriminates: `if subject.kind == "lit"
            # then … else …`
            sty = t["types"].get(id(match_expr["subject"]))
            srt = sty["rt"] if sty else None
            recs = (
                [r for r in srt["arms"] if r.get("t") == "rec"]
                if srt and srt.get("t") == "union"
                else []
            )
            disc = (
                next(
                    (
                        mm
                        for mm in recs[0]["members"]
                        if (mm.get("type") or {}).get("t") == "lit"
                        and all(
                            any(
                                x["name"] == mm["name"] and (x.get("type") or {}).get("t") == "lit"
                                for x in r["members"]
                            )
                            for r in recs
                        )
                    ),
                    None,
                )
                if recs
                else None
            )
            if disc is not None:
                parts: list[Any] = []
                fallback = None
                ok = True
                for arm in match_expr["arms"]:
                    body_ = (
                        re.sub(
                            r"\b" + re.escape(arm["v"]) + r"\b",
                            expr_text(match_expr["subject"]),
                            _src_of(text, arm["body"]["loc"]),
                        )
                        if arm.get("body") and arm["body"].get("loc")
                        else None
                    )
                    if body_ is None:
                        ok = False
                        break
                    rec = (
                        next((r for r in recs if r.get("name") == arm["type"]["name"]), None)
                        if arm.get("type") and arm["type"].get("k") == "named"
                        else None
                    )
                    empty: dict[str, Any] = {}
                    lit = (
                        next((x for x in rec["members"] if x["name"] == disc["name"]), empty)
                        .get("type", empty)
                        .get("v")
                        if rec
                        else None
                    )
                    if isinstance(lit, str):
                        parts.append(
                            f"if {expr_text(match_expr['subject'])}.{disc['name']} == "
                            f"{json.dumps(lit, ensure_ascii=False)} then {body_}"
                        )
                    else:
                        fallback = body_
                if ok and parts:
                    one(
                        "convert to if",
                        "refactor.rewrite",
                        [
                            {
                                "range": range_of(match_expr["loc"], lines),
                                "newText": " else ".join(parts)
                                + f" else {fallback if fallback is not None else 'null'}",
                            }
                        ],
                    )
        # inline a derived member into its sibling uses
        derived = next(
            (
                n
                for n in chain
                if is_member(n)
                and n["m"] == "derived"
                and n.get("loc")
                and n.get("expr")
                and n["expr"].get("loc")
            ),
            None,
        )
        if derived is not None and body is not None and body.get("loc"):
            uses: list[Any] = []
            for other in body["members"]:
                if other is derived:
                    continue
                _find_all(
                    other,
                    lambda n: (
                        is_expr(n)
                        and n["e"] == "name"
                        and n["name"] == derived["name"]
                        and n.get("loc")
                    ),
                    uses,
                )
            if uses:
                src = _src_of(text, derived["expr"]["loc"])
                wrapped = (
                    src
                    if is_expr(derived["expr"])
                    and derived["expr"]["e"]
                    in ("name", "lit", "unitlit", "call", "member", "paren")
                    else f"({src})"
                )
                edits = [{"range": range_of(u["loc"], lines), "newText": wrapped} for u in uses]
                edits.append(
                    {
                        "range": {
                            "start": {"line": derived["loc"]["sl"], "character": 0},
                            "end": {"line": derived["loc"]["el"] + 1, "character": 0},
                        },
                        "newText": "",
                    }
                )
                one(f"inline {derived['name']}", "refactor.inline", edits)
        # flip the operands of a comparison
        cmp = next(
            (
                n
                for n in chain
                if is_expr(n)
                and n["e"] == "bin"
                and n["op"] in ("<", ">", "<=", ">=", "==", "!=")
                and n["l"].get("loc")
                and n["r"].get("loc")
            ),
            None,
        )
        if cmp is not None:
            flipped = {"<": ">", ">": "<", "<=": ">=", ">=": "<=", "==": "==", "!=": "!="}
            one(
                "flip the comparison",
                "refactor.rewrite",
                [
                    {
                        "range": range_of(cmp["loc"], lines),
                        "newText": f"{_src_of(text, cmp['r']['loc'])} {flipped[cmp['op']]} "
                        f"{_src_of(text, cmp['l']['loc'])}",
                    }
                ],
            )
        # extract the selected expression: into a constant (a constant expression), or a derived
        # member (inside a record body)
        selected = None
        if rstart["line"] != rend["line"] or rstart["character"] != rend["character"]:
            selected = next(
                (
                    n
                    for n in chain
                    if is_expr(n)
                    and n.get("loc")
                    and n["loc"]["sl"] == rstart["line"]
                    and n["loc"]["sc"] == rstart["character"]
                    and n["loc"]["el"] == rend["line"]
                    and n["loc"]["ec"] == rend["character"]
                ),
                None,
            )
        if selected is not None and not (is_expr(selected) and selected["e"] == "name"):
            src = _src_of(text, selected["loc"])
            enclosing_member = next((n for n in chain if is_member(n) and n.get("loc")), None)
            enclosing_decl = next((n for n in chain if is_decl(n)), None)
            if (
                not _mentions_name(selected)
                and enclosing_decl is not None
                and enclosing_decl.get("loc")
            ):
                one(
                    "extract to a constant",
                    "refactor.extract",
                    [
                        insert_at(
                            {"line": enclosing_decl["loc"]["sl"], "character": 0},
                            f"const extracted = {src}\n",
                        ),
                        {"range": range_of(selected["loc"], lines), "newText": "extracted"},
                    ],
                )
            if enclosing_member is not None and enclosing_member["m"] != "context":
                indent = " " * _indent_width(_line(lines, enclosing_member["loc"]["sl"]))
                one(
                    "extract to a derived member",
                    "refactor.extract",
                    [
                        insert_at(
                            {"line": enclosing_member["loc"]["sl"], "character": 0},
                            f"{indent}extracted = {src}\n",
                        ),
                        {"range": range_of(selected["loc"], lines), "newText": "extracted"},
                    ],
                )
    return out


# ---------------- local variables: linked editing, rename ----------------
# a comprehension variable, a lambda parameter, a match arm's variable, or
# a function parameter: its binding token and its uses in its scope
def binds_name(n: Any, name: str) -> bool:
    if is_expr(n):
        e = n["e"]
        return (
            (e in ("comp", "mapcomp") and any(c["v"] == name for c in n["clauses"]))
            or (e == "lambda" and name in n["params"])
            or (e == "match" and any(arm.get("v") == name for arm in n["arms"]))
        )
    return is_decl(n) and n["d"] == "func" and any(p["name"] == name for p in n["params"])


def binding_locs(text: str, scope: dict[str, Any], name: str) -> list[Any]:
    src = _src_of(text, scope["loc"]).encode("utf-8")
    loc = scope["loc"]
    out: list[Any] = []
    nb = name.encode("utf-8")

    def at(offset: int) -> dict[str, Any]:  # an offset in `src` -> a Loc
        line, col = loc["sl"], loc["sc"]
        for i in range(offset):
            if src[i : i + 1] == b"\n":
                line += 1
                col = 0
            else:
                col += 1
        return {"sl": line, "sc": col, "el": line, "ec": col + len(nb)}

    esc = re.escape(nb)
    if is_decl(scope):
        m = re.search(rb"\(([^)]*)\b(" + esc + rb")\b", src)
        if m:
            out.append(at(m.start() + len(m.group(0)) - len(nb)))
    elif scope["e"] == "lambda":
        m = re.search(rb"\b(" + esc + rb")\b(?=[^=]*=>)", src)
        if m:
            out.append(at(m.start() + len(m.group(0)) - len(nb)))
    elif scope["e"] == "match":
        for m in re.finditer(rb"\(\s*(" + esc + rb")\b", src):
            out.append(at(m.start() + len(m.group(0)) - len(nb)))
    else:
        for m in re.finditer(rb"\bfor\s+(" + esc + rb")\b", src):
            out.append(at(m.start() + len(m.group(0)) - len(nb)))
    return out


def local_ranges(uri: str, pos: dict[str, Any]) -> list[Any] | None:
    """the binding and the uses of the local variable at a position (bytes), or None"""
    text = docs.get(uri)
    if text is None:
        return None
    parsed = parse_source(text)
    if parsed["errors"]:
        return None
    decls = parsed["decls"]
    hit = node_at(decls, pos)
    if not hit:
        return None
    name: str | None = None
    scope: Any = None
    node = hit["node"]
    if is_expr(node) and node["e"] == "name":
        name = node["name"]
        scope = next((p for p in hit["parents"][::-1] if binds_name(p, name)), None)
    else:
        # on a binding token: the scope node itself, the name under the cursor
        line = _line(text.split("\n"), pos["line"]).encode("utf-8")
        for mm in re.finditer(rb"[A-Za-z_][A-Za-z0-9_]*", line):
            if mm.start() <= pos["character"] <= mm.end():
                name = mm.group(0).decode("utf-8")
                break
        if not name:
            return None
        scope = next((p for p in [node, *hit["parents"][::-1]] if binds_name(p, name)), None)
    if not name or scope is None or not scope.get("loc"):
        return None
    locs = binding_locs(text, scope, name)

    def uses(x: Any) -> None:
        if not x or not isinstance(x, (dict, list)):
            return
        if isinstance(x, list):
            for y in x:
                uses(y)
            return
        if x is not scope and binds_name(x, name):
            return  # shadowed below
        if is_expr(x) and x["e"] == "name" and x["name"] == name and x.get("loc"):
            locs.append(x["loc"])
        for k, v in x.items():
            if k != "loc" and v and isinstance(v, (dict, list)):
                uses(v)

    for k, v in scope.items():
        if k != "loc" and v and isinstance(v, (dict, list)):
            uses(v)
    seen: set[Any] = set()
    uniq: list[Any] = []
    for l in locs:
        if (l["sl"], l["sc"]) not in seen:
            seen.add((l["sl"], l["sc"]))
            uniq.append(l)
    return sorted(uniq, key=lambda l: (l["sl"], l["sc"]))


def linked_editing_range(uri: str, pos: dict[str, Any]) -> Any:
    lines = (docs.get(uri) or "").split("\n")
    locs = local_ranges(uri, pos_bytes(pos, lines))
    return (
        {"ranges": [range_of(l, lines) for l in locs], "wordPattern": "[A-Za-z_][A-Za-z0-9_]*"}
        if locs
        else None
    )


# ---------------- the syntax tree ----------------
# ---------------- on-type formatting ----------------
# `\n`: the new line takes the previous line's indentation, one level
# deeper after an opening bracket or a continuation point (§2.9); `}`:
# the line dedents to the opening brace's line
def on_type_formatting(uri: str, pos: dict[str, Any], ch: str) -> list[Any]:
    text = docs.get(uri)
    if text is None:
        return []
    lines = text.split("\n")

    def indent_of(s: str) -> int:
        return _indent_width(s)

    def edit(line: int, have: int, want: int) -> list[Any]:
        if have == want:
            return []
        return [
            {
                "range": {
                    "start": {"line": line, "character": 0},
                    "end": {"line": line, "character": have},
                },
                "newText": " " * want,
            }
        ]

    if ch == "\n":
        prev = _line(lines, pos["line"] - 1) if pos["line"] > 0 else ""
        cur = _line(lines, pos["line"])
        body = re.sub(r"//.*$", "", prev).rstrip()
        want = indent_of(prev)
        if re.search(r"[{\[(]$", body) or re.search(
            r"(?:[+\-*/%<>=!&|?:,]|\bthen|\belse|\bin|\bwith|=>)$", body
        ):
            want += 4
        if re.match(r"^\s*[}\])]", cur):
            want = max(0, want - 4)
        return edit(pos["line"], indent_of(cur), want)
    if ch in ("}", "]", ")"):
        cur = _line(lines, pos["line"])
        if cur.strip() != ch:
            return []
        # the opening bracket's line: scan back with a depth count
        opens = {"}": "{", "]": "[", ")": "("}
        depth = 0
        for l in range(pos["line"], -1, -1):
            s = _line(lines, l)
            start = (cur.find(ch) if l == pos["line"] else len(s)) - 1
            for i in range(start, -1, -1):
                c = s[i]
                if c == ch:
                    depth += 1
                elif c == opens[ch]:
                    if depth == 0:
                        return edit(pos["line"], indent_of(cur), indent_of(s))
                    depth -= 1
        return []
    return []


# ---------------- progress ----------------
# a client that accepts work-done progress sees an analysis of a large
# universe as a progress item (03_lsp.md §14)
progress_supported = False
progress_seq = 0


def with_progress(title: str, f: Callable[[], Any]) -> Any:
    global progress_seq
    if not progress_supported:
        return f()
    progress_seq += 1
    token = f"decl-{progress_seq}"
    # the request's id is the sequence number: an integer, which every client accepts (Neovim
    # rejects a string)
    send(
        {
            "jsonrpc": "2.0",
            "id": progress_seq,
            "method": "window/workDoneProgress/create",
            "params": {"token": token},
        }
    )
    notify(
        "$/progress",
        {"token": token, "value": {"kind": "begin", "title": title, "cancellable": False}},
    )
    try:
        return f()
    finally:
        notify("$/progress", {"token": token, "value": {"kind": "end"}})


def syntax_tree(uri: str) -> Any:
    text = docs.get(uri)
    if text is None:
        return None
    return {"tree": str(Parser(LANGUAGE).parse(text.encode("utf-8")).root_node)}


# ---------------- request handling ----------------
def handle(msg: dict[str, Any]) -> None:
    # a message without a method is the client's response to a request of
    # ours (window/workDoneProgress/create): nothing to answer
    if "method" not in msg:
        return
    id_ = msg.get("id")
    method = msg.get("method")
    params = msg.get("params") or {}
    if method == "initialize":
        global progress_supported
        progress_supported = bool(
            ((params.get("capabilities") or {}).get("window") or {}).get("workDoneProgress")
        )
        reply(
            id_,
            {
                "capabilities": {
                    "textDocumentSync": 1,
                    "hoverProvider": True,
                    "definitionProvider": True,
                    "typeDefinitionProvider": True,
                    "referencesProvider": True,
                    "documentHighlightProvider": True,
                    "documentSymbolProvider": True,
                    "foldingRangeProvider": True,
                    "documentFormattingProvider": True,
                    "renameProvider": {"prepareProvider": True},
                    "completionProvider": {"triggerCharacters": [".", "$", ":"]},
                    "codeLensProvider": {"resolveProvider": False},
                    "signatureHelpProvider": {"triggerCharacters": ["(", ","]},
                    "workspaceSymbolProvider": True,
                    "selectionRangeProvider": True,
                    "semanticTokensProvider": {
                        "legend": {"tokenTypes": TOKEN_TYPES, "tokenModifiers": TOKEN_MODS},
                        "full": True,
                    },
                    "inlayHintProvider": True,
                    "callHierarchyProvider": True,
                    "typeHierarchyProvider": True,
                    "codeActionProvider": {
                        "codeActionKinds": [
                            "quickfix",
                            "refactor.rewrite",
                            "refactor.extract",
                            "refactor.inline",
                        ]
                    },
                    "linkedEditingRangeProvider": True,
                    "documentOnTypeFormattingProvider": {
                        "firstTriggerCharacter": "\n",
                        "moreTriggerCharacter": ["}", "]", ")"],
                    },
                    "executeCommandProvider": {
                        "commands": [
                            "decl.evaluate",
                            "decl.validate",
                            "decl.trace",
                            "decl.showSyntaxTree",
                            "decl.reloadWorkspace",
                        ]
                    },
                },
                "serverInfo": {"name": "decl-lsp", "version": _version()},
            },
        )
    elif method == "initialized":
        pass
    elif method == "decl/files":
        # a browser client's workspace files (the reference's lsp-web.ts): here the host is the
        # file system
        for f in params.get("files") or []:
            try:
                with open(path_of(f["uri"]), "w", encoding="utf-8") as fh:
                    fh.write(f["text"])
            except OSError:
                pass
        for u in params.get("remove") or []:
            with contextlib.suppress(OSError):
                os.remove(path_of(u))
        analyses.clear()
        for u in list(docs):
            analyze(u)
    elif method == "workspace/didChangeConfiguration":
        decl_settings = (params.get("settings") or {}).get("decl") or {}
        config["inputs"] = decl_settings.get("inputs") or {}
        for k in list(hints):
            v = (decl_settings.get("inlayHints") or {}).get(k)
            if isinstance(v, bool):
                hints[k] = v
        analyses.clear()
        for u in list(docs):
            analyze(u)
    elif method == "workspace/didChangeWatchedFiles":
        analyses.clear()
        for u in list(docs):
            analyze(u)
    elif method == "textDocument/didOpen":
        uri = params["textDocument"]["uri"]
        docs[uri] = params["textDocument"]["text"]
        overlay[path_of(uri)] = params["textDocument"]["text"]
        analyses.clear()
        analyze(uri)
    elif method == "textDocument/didChange":
        uri = params["textDocument"]["uri"]
        docs[uri] = params["contentChanges"][0]["text"]
        overlay[path_of(uri)] = params["contentChanges"][0]["text"]
        analyses.clear()
        analyze(uri)
    elif method == "textDocument/didSave":
        pass
    elif method == "textDocument/didClose":
        uri = params["textDocument"]["uri"]
        docs.pop(uri, None)
        overlay.pop(path_of(uri), None)
        analyses.pop(uri, None)
        last_good.pop(uri, None)
        notify("textDocument/publishDiagnostics", {"uri": uri, "diagnostics": []})
    elif method == "textDocument/hover":
        reply(id_, hover(params["textDocument"]["uri"], params["position"]))
    elif method == "textDocument/definition":
        reply(id_, definition(params["textDocument"]["uri"], params["position"]))
    elif method == "textDocument/typeDefinition":
        reply(id_, type_definition(params["textDocument"]["uri"], params["position"]))
    elif method == "textDocument/references":
        a = analysis_of(params["textDocument"]["uri"])
        refs = references(
            params["textDocument"]["uri"],
            params["position"],
            bool((params.get("context") or {}).get("includeDeclaration")),
        )
        reply(id_, [location(a, m, loc) for m, loc in refs] if a is not None else [])
    elif method == "textDocument/documentHighlight":
        uri = params["textDocument"]["uri"]
        path = path_of(uri)
        a = analysis_of(uri)
        refs = references(uri, params["position"], True)
        reply(
            id_,
            [
                {"range": range_of(loc, text_of(a, m).split("\n")), "kind": 1}
                for m, loc in refs
                if m.path == path
            ]
            if a is not None
            else [],
        )
    elif method == "textDocument/completion":
        reply(id_, completion(params["textDocument"]["uri"], params["position"]))
    elif method == "textDocument/documentSymbol":
        reply(id_, document_symbols(params["textDocument"]["uri"]))
    elif method == "textDocument/foldingRange":
        reply(id_, folding_ranges(params["textDocument"]["uri"]))
    elif method == "textDocument/formatting":
        reply(id_, formatting(params["textDocument"]["uri"]))
    elif method == "textDocument/prepareRename":
        reply(id_, prepare_rename(params["textDocument"]["uri"], params["position"]))
    elif method == "textDocument/rename":
        reply(id_, rename(params["textDocument"]["uri"], params["position"], params["newName"]))
    elif method == "textDocument/codeLens":
        reply(id_, code_lenses(params["textDocument"]["uri"]))
    elif method == "textDocument/signatureHelp":
        reply(id_, signature_help(params["textDocument"]["uri"], params["position"]))
    elif method == "workspace/symbol":
        reply(id_, workspace_symbols(params.get("query") or ""))
    elif method == "textDocument/selectionRange":
        reply(id_, selection_ranges(params["textDocument"]["uri"], params.get("positions") or []))
    elif method == "textDocument/semanticTokens/full":
        reply(id_, semantic_tokens(params["textDocument"]["uri"]))
    elif method == "textDocument/inlayHint":
        reply(id_, inlay_hints(params["textDocument"]["uri"], params["range"]))
    elif method == "textDocument/prepareCallHierarchy":
        reply(id_, prepare_hierarchy(params["textDocument"]["uri"], params["position"], "func"))
    elif method == "callHierarchy/incomingCalls":
        reply(id_, incoming_calls(params["item"]))
    elif method == "callHierarchy/outgoingCalls":
        reply(id_, outgoing_calls(params["item"]))
    elif method == "textDocument/prepareTypeHierarchy":
        reply(id_, prepare_hierarchy(params["textDocument"]["uri"], params["position"], "type"))
    elif method == "typeHierarchy/supertypes":
        reply(id_, supertypes(params["item"]))
    elif method == "typeHierarchy/subtypes":
        reply(id_, subtypes(params["item"]))
    elif method == "textDocument/codeAction":
        reply(
            id_,
            code_actions(
                params["textDocument"]["uri"],
                params["range"],
                (params.get("context") or {}).get("diagnostics") or [],
            ),
        )
    elif method == "textDocument/linkedEditingRange":
        reply(id_, linked_editing_range(params["textDocument"]["uri"], params["position"]))
    elif method == "textDocument/onTypeFormatting":
        reply(
            id_,
            on_type_formatting(
                params["textDocument"]["uri"], params["position"], cast(str, params.get("ch"))
            ),
        )
    elif method == "workspace/executeCommand":
        # a refused command (an unknown root, an unreadable binding) answers null, never silence
        try:
            result = execute_command(cast(str, params.get("command")), params.get("arguments"))
        except SessionError:
            result = None
        reply(id_, result)
    elif method == "shutdown":
        reply(id_, None)
    elif method == "exit":
        sys.exit(0)
    elif "id" in msg:
        reply(id_, None)


def _version() -> str:
    from .api import __version__

    return str(__version__)


def main(argv: list[Any] | None = None) -> int:
    global _out
    if argv is None:
        argv = sys.argv[1:]
    # `decl-lsp --version`: the same string as `decl --version`
    if argv and "--version" in argv:
        print(f"decl-lsp {_version()}")
        return 0
    _out = sys.stdout.buffer
    inp = sys.stdin.buffer
    buf = b""
    while True:
        chunk = inp.read1(65536) if hasattr(inp, "read1") else inp.read(65536)
        if not chunk:
            return 0  # stdin closed: exit after the queued messages (all handled synchronously)
        buf += chunk
        while True:
            header_end = buf.find(b"\r\n\r\n")
            if header_end < 0:
                break
            header = buf[:header_end].decode("ascii", "replace")
            m = re.search(r"Content-Length: (\d+)", header, re.IGNORECASE)
            if not m:
                buf = buf[header_end + 4 :]
                continue
            length = int(m.group(1))
            if len(buf) < header_end + 4 + length:
                break
            body = buf[header_end + 4 : header_end + 4 + length]
            buf = buf[header_end + 4 + length :]
            msg = json.loads(body.decode("utf-8"))
            # a request whose handler throws is answered with an error, never left waiting
            try:
                handle(msg)
            except SystemExit:
                raise
            except Exception as e:  # pragma: no cover
                log_err(f"{type(e).__name__}: {e}")
                if msg.get("id") is not None:
                    send(
                        {
                            "jsonrpc": "2.0",
                            "id": msg["id"],
                            "error": {"code": -32603, "message": str(e)},
                        }
                    )


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
