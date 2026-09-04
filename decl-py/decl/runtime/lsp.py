"""decl-lsp (docs/tooling/03_lsp.md): the language server over stdio — a
port of the reference implementation's lsp.ts. Every answer comes from the
same checker, inference, and engine as the command line, driven through
the session object (session.py) with the open buffers overriding the disk;
positions come from the source ranges every AST node carries, and the
types and resolutions recorded while the checker runs (infer.py hooks).
Messages are handled strictly in order; the server exits when its stdin
closes."""
from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path
from typing import Any, Callable, Optional
from urllib.parse import unquote, urlparse

from .checker import check_module
from .fmt import format_source, u16
from .infer import resolve_in, type_text
from .parse import parse_source
from .semantics import parse_path, seg_text
from .session import Session, fmt_diag

_out = None


# ---------------- transport ----------------
def send(msg: dict) -> None:
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
docs: dict = {}          # uri -> text
overlay: dict = {}       # path -> text (open buffers override the disk)
config: dict = {"inputs": {}}


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


def _line(lines: list, i: int) -> str:
    return lines[i] if 0 <= i < len(lines) else ""


def range_of(loc: dict, lines: list) -> dict:
    return {"start": {"line": loc["sl"], "character": _u16_col(_line(lines, loc["sl"]), loc["sc"])},
            "end": {"line": loc["el"], "character": _u16_col(_line(lines, loc["el"]), loc["ec"])}}


def pos_bytes(pos: dict, lines: list) -> dict:
    return {"line": pos["line"], "character": _byte_col(_line(lines, pos["line"]), pos["character"])}


def contains(loc: dict, p: dict) -> bool:
    return ((loc["sl"] < p["line"] or (loc["sl"] == p["line"] and loc["sc"] <= p["character"]))
            and (p["line"] < loc["el"] or (p["line"] == loc["el"] and p["character"] <= loc["ec"])))


def span(loc: dict) -> int:
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
    __slots__ = ("path", "text", "session", "run", "tables")

    def __init__(self, path: str, text: str, session: Session, run) -> None:
        self.path, self.text, self.session, self.run = path, text, session, run
        self.tables: dict = {}


analyses: dict = {}
last_good: dict = {}     # the last analysis of a document that parsed (completion while typing)


def analysis_of(uri: str) -> Optional[Analysis]:
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
    run = session.run(None, "full")
    a = Analysis(path, text, session, run)
    analyses[uri] = a
    last_good[uri] = a
    return a


def tables_of(a: Analysis, m) -> dict:
    have = a.tables.get(m.path)
    if have is not None:
        return have
    t: dict = {"types": {}, "res": {}}
    check_module(m.decls, m.env, {"record": lambda e, ty: t["types"].__setitem__(id(e), ty),
                                  "resolve_hook": lambda e, target: t["res"].__setitem__(id(e), target)})
    a.tables[m.path] = t
    return t


def module_of(a: Analysis, path: str):
    return next((m for m in a.run.modules if m.path == path), None)


def read_text(path: str) -> str:
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return ""


def text_of(a: Analysis, m) -> str:
    return overlay[m.path] if m.path in overlay else read_text(m.path)


# ---------------- diagnostics ----------------
_SKIP = ("error", "in", "the", "a", "is", "not", "std", "module", "import", "type", "name")


def anchor_for(src: str, message: str) -> dict:
    lines = src.split("\n")
    for n in re.findall(r"[A-Za-z_][A-Za-z0-9_.]*", message):
        if n in _SKIP:
            continue
        pat = re.compile(r"\b" + re.escape(n) + r"\b")
        for i, line in enumerate(lines):
            mm = pat.search(line)
            if mm:
                a = len(line[:mm.start()].encode("utf-8"))
                return {"sl": i, "sc": a, "el": i, "ec": a + len(n.encode("utf-8"))}
    return {"sl": 0, "sc": 0, "el": 0, "ec": max(1, len((lines[0] if lines else "").encode("utf-8")))}


def loc_of_path(decls: list, segs: list) -> Optional[dict]:
    """the source position of a document path: the literal the path leads to
    in the root's declaration, or the deepest literal on the way"""
    root = segs[0]
    decl = next((d for d in decls if d["d"] in ("output", "input") and d["name"] == root), None)
    if decl is None or not decl.get("loc"):
        return None
    e: Optional[dict] = decl.get("expr") if decl["d"] == "output" else decl.get("fallback")
    best = decl["loc"]
    for s in segs[1:]:
        if not e:
            break
        nxt: Optional[dict] = None
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
    return 1 if s == "error" else 2 if s == "warning" else 3


def analyze(uri: str) -> None:
    src = docs[uri]
    path = path_of(uri)
    lines = src.split("\n")
    out: list = []

    def push(loc: dict, d: dict) -> None:
        item: dict = {"range": range_of(loc, lines), "severity": severity_of(d["severity"]), "source": "decl"}
        if d.get("code") or d.get("id"):
            item["code"] = d["id"] if d.get("id") is not None else d.get("code")
        item["message"] = f"{d['message']} (at {d['path']})" if d.get("path") else d["message"]
        out.append(item)

    parsed = parse_source(src)
    errors, decls = parsed["errors"], parsed["decls"]
    if errors:
        for e in errors:
            out.append({"range": {"start": {"line": e["row"], "character": _u16_col(_line(lines, e["row"]), e["col"])},
                                  "end": {"line": e["row"], "character": _u16_col(_line(lines, e["row"]), e["col"]) + 1}},
                        "severity": 1, "source": "decl", "code": "E2001", "message": "syntax error"})
    else:
        a = analysis_of(uri)
        r = a.run
        for d in r.load_diags:
            # a loading problem is anchored to the import it concerns when one is named
            imp = next((x for x in decls if x["d"] in ("import", "re_export") and x.get("loc")
                        and re.sub(r"\.decl$", "", re.sub(r"^\./", "", x["from"])) in d["message"]), None)
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
                continue   # a root declared elsewhere: its own module's business
            push(loc, d)
    notify("textDocument/publishDiagnostics", {"uri": uri, "diagnostics": out})


# ---------------- positions -> nodes ----------------
def node_at(decls: list, pos: dict) -> Optional[dict]:
    """the innermost AST node (declaration, member, type, or expression) at a position (bytes)"""
    best: dict = {"hit": None}

    def visit(x: Any, parents: list) -> None:
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
            visit(v, parents + [x] if own else parents)

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


def name_range(text: str, decl: dict, name: str) -> dict:
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


def member_range(text: str, member: dict, name: str) -> dict:
    loc = member["loc"]
    line = _line(text.split("\n"), loc["sl"])
    i = _bfind(line, name, loc["sc"])
    return {"sl": loc["sl"], "sc": i, "el": loc["sl"], "ec": i + len(name.encode("utf-8"))} if i >= 0 else loc


# ---------------- what is under the cursor ----------------
class Site:
    __slots__ = ("kind", "module", "decl", "member", "range", "name")

    def __init__(self, kind: str, module, decl: Optional[dict], member: Optional[dict], range_: dict, name: str) -> None:
        self.kind, self.module, self.decl, self.member, self.range, self.name = kind, module, decl, member, range_, name


def site_of_target(a: Analysis, t: Optional[dict]) -> Optional[Site]:
    """the declaration a target denotes, as a site in its module"""
    if not t or t.get("env") is None:
        return None
    m = next((x for x in a.run.modules if x.env is t["env"]), None)
    if m is None:
        return None
    text = text_of(a, m)
    decl = next((d for d in m.decls if d.get("name") == t["name"] and d.get("loc") and d["d"] != "import"), None)
    if decl is not None:
        return Site(decl["d"], m, decl, None, name_range(text, decl, t["name"]), t["name"])
    return None


def _record_members(body: dict) -> list:
    if body.get("k") == "record":
        return body["members"]
    if body.get("k") == "named" and body.get("ext") and body["ext"].get("k") == "record":
        return body["ext"]["members"]
    return []


def member_site(a: Analysis, m, rt: Optional[dict], member: str) -> Optional[Site]:
    """the member's declaring type, extension chains followed (§4)"""
    seen: set = set()
    type_name = rt.get("name") if rt and rt.get("t") == "rec" else (rt["base"].get("name") if rt and rt.get("t") == "pred" and rt.get("base") else None)
    while type_name and type_name not in seen:
        seen.add(type_name)
        site = site_of_target(a, resolve_in(m.env, type_name))
        decl = site.decl if site is not None else None
        if decl is None or decl["d"] != "type":
            return None
        body = decl["type"]
        mem = next((x for x in _record_members(body) if x.get("name") == member), None)
        if mem is not None and mem.get("loc"):
            return Site("member", site.module, decl, mem, member_range(text_of(a, site.module), mem, member), member)
        type_name = body.get("name") if body.get("k") == "named" else None
    return None


def site_at(a: Analysis, uri: str, pos: dict) -> Optional[dict]:
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
                return {"site": site_of_target(a, resolve_in(ex["env"], ex["name"])) if ex else None, "type": ty, "hit": hit, "module": m}
            xt = t["types"].get(id(x))
            return {"site": member_site(a, m, xt["rt"] if xt else None, n["name"]), "type": ty, "hit": hit, "module": m}
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
        site = Site("member", m, decl, n, member_range(text_of(a, m), n, n["name"]), n["name"]) if decl is not None else None
        return {"site": site, "type": None, "hit": hit, "module": m}
    if is_decl(n) and isinstance(n.get("name"), str):
        r = name_range(text_of(a, m), n, n["name"])
        if contains(r, pos):
            return {"site": Site(n["d"], m, n, None, r, n["name"]), "type": None, "hit": hit, "module": m}
    return {"site": None, "type": None, "hit": hit, "module": m}


def _pos_of(uri: str, pos: dict) -> dict:
    return pos_bytes(pos, docs.get(uri, "").split("\n"))


# ---------------- hover ----------------
_DOC = re.compile(r"^\s*///")


def decl_text(a: Analysis, site: Site) -> list:
    text = text_of(a, site.module)
    lines = text.split("\n")
    if site.member is not None and site.member.get("loc"):
        l = site.member["loc"]
        doc_lines: list = []
        frm = l["sl"]
        while frm > 0 and _DOC.match(lines[frm - 1]):
            frm -= 1
            doc_lines.insert(0, lines[frm].strip())
        if l["sl"] == l["el"]:
            body = [_bslice(lines[l["sl"]], l["sc"], l["ec"])]
        else:
            body = [_bslice(lines[l["sl"]], l["sc"], len(lines[l["sl"]].encode("utf-8")))] + lines[l["sl"] + 1:l["el"]] + [_bslice(lines[l["el"]], 0, l["ec"])]
        return doc_lines + [x for x in (b.strip() for b in body) if x]
    l = site.decl["loc"]
    doc_lines = []
    frm = l["sl"]
    while frm > 0 and _DOC.match(lines[frm - 1]):
        frm -= 1
        doc_lines.insert(0, lines[frm].strip())
    body = lines[l["sl"]:l["el"] + 1]
    if len(body) > 12:
        body = body[:11] + ["    …", body[-1]]
    return doc_lines + body


def hover(uri: str, pos: dict) -> Any:
    a = analysis_of(uri)
    if a is None:
        return None
    s = site_at(a, uri, _pos_of(uri, pos))
    if s is None:
        return None
    parts: list = []
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
        return {"contents": {"kind": "markdown", "value": "\n\n".join(parts)},
                "range": range_of(node["loc"], text_of(a, s["module"]).split("\n"))}
    return {"contents": {"kind": "markdown", "value": "\n\n".join(parts)}}


# ---------------- navigation ----------------
def location(a: Analysis, m, loc: dict) -> dict:
    return {"uri": uri_of(m.path), "range": range_of(loc, text_of(a, m).split("\n"))}


def definition(uri: str, pos: dict) -> Any:
    a = analysis_of(uri)
    s = site_at(a, uri, _pos_of(uri, pos)) if a else None
    return location(a, s["site"].module, s["site"].range) if s and s["site"] is not None else None


def type_definition(uri: str, pos: dict) -> Any:
    a = analysis_of(uri)
    s = site_at(a, uri, _pos_of(uri, pos)) if a else None
    if not s:
        return None
    rt = s["type"]["rt"] if s["type"] else None
    name = rt.get("name") if rt and rt.get("t") == "rec" else (rt["base"].get("name") if rt and rt.get("t") == "pred" and rt.get("base") else None)
    if not name:
        return None
    site = site_of_target(a, resolve_in(s["module"].env, name))
    return location(a, site.module, site.range) if site is not None else None


def member_token_loc(text: str, e: dict) -> dict:
    l = e["loc"]
    line = _line(text.split("\n"), l["el"])
    i = _brfind(line, e["name"], l["ec"])
    return {"sl": l["el"], "sc": i, "el": l["el"], "ec": i + len(e["name"].encode("utf-8"))} if i >= 0 else l


def type_name_loc(t: dict, offset: int, name: str) -> dict:
    return {"sl": t["loc"]["sl"], "sc": t["loc"]["sc"] + offset, "el": t["loc"]["sl"], "ec": t["loc"]["sc"] + offset + len(name.encode("utf-8"))}


def import_item_loc(text: str, d: dict, name: str) -> dict:
    l = d["loc"]
    line = _line(text.split("\n"), l["sl"])
    i = _bfind(line, name, l["sc"])
    return {"sl": l["sl"], "sc": i, "el": l["sl"], "ec": i + len(name.encode("utf-8"))} if i >= 0 else l


def references(uri: str, pos: dict, include_declaration: bool) -> list:
    """every reference to a site across the universe: name and member nodes
    that resolve to the same declaration, plus the declaration itself"""
    a = analysis_of(uri)
    s = site_at(a, uri, _pos_of(uri, pos)) if a else None
    if a is None or not s or s["site"] is None:
        return []
    target: Site = s["site"]
    out: list = []

    def same(x: Optional[Site]) -> bool:
        return (x is not None and x.module is target.module and x.name == target.name and x.kind == target.kind
                and (x.kind != "member" or x.decl is target.decl))

    for m in a.run.modules:
        t = tables_of(a, m)
        text = text_of(a, m)

        def visit(x: Any) -> None:
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
                    site: Optional[Site] = None
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
    seen: set = set()
    kept: list = []
    for r in out:
        key = f"{r[0].path}:{r[1]['sl']}:{r[1]['sc']}"
        if key in seen:
            continue
        seen.add(key)
        kept.append(r)
    kept.sort(key=lambda r: (r[0].path, r[1]["sl"], r[1]["sc"]))
    return kept


# ---------------- completion ----------------
def completion(uri: str, pos: dict) -> dict:
    a = analysis_of(uri)
    text = docs.get(uri)
    if text is None:
        return {"isIncomplete": False, "items": []}
    line = _line(text.split("\n"), pos["line"])
    prefix = line.encode("utf-8")[:_byte_col(line, pos["character"])].decode("utf-8", "replace")
    # while the text does not parse, the scope is the last one that did
    session = a.session if a is not None else (last_good[uri].session if uri in last_good else Session(path_of(uri), overlay))
    items: list = []
    for c in session.complete(prefix, []):
        parts = c.split("  ")
        label = parts[0]
        detail = parts[1] if len(parts) > 1 else None
        if detail:
            kind = 5 if detail.startswith(("derived", "required", "optional", "defaulted")) else 6
        else:
            kind = 7 if re.match(r"^[A-Z]", label) else 14 if label.startswith("$") else 6
        item: dict = {"label": label, "kind": kind}
        if detail:
            item["detail"] = detail
        items.append(item)
    return {"isIncomplete": False, "items": items}


# ---------------- symbols, folding, formatting ----------------
SYMBOL_KIND = {"type": 5, "const": 14, "func": 12, "output": 13, "input": 13, "diagnostic": 24, "dimension": 13, "unit": 13}


def document_symbols(uri: str) -> list:
    text = docs.get(uri)
    if text is None:
        return []
    parsed = parse_source(text)
    if parsed["errors"]:
        return []
    lines = text.split("\n")
    out: list = []
    for d in parsed["decls"]:
        if not d.get("loc") or not isinstance(d.get("name"), str) or d["d"] not in SYMBOL_KIND:
            continue
        sym: dict = {"name": d["name"], "kind": SYMBOL_KIND[d["d"]], "range": range_of(d["loc"], lines),
                     "selectionRange": range_of(name_range(text, d, d["name"]), lines)}
        if d["d"] == "type":
            body = d["type"] if d["type"].get("k") == "record" else (d["type"]["ext"] if d["type"].get("k") == "named" and d["type"].get("ext") and d["type"]["ext"].get("k") == "record" else None)
            if body:
                children = []
                for m in body["members"]:
                    if not m.get("loc") or not isinstance(m.get("name"), str):
                        continue
                    children.append({
                        "name": f"assert {m['name']}" if m["m"] == "assert" else (f"{m['name']}$" if m.get("hidden") else m["name"]),
                        "kind": 24 if m["m"] == "assert" else 7,
                        "range": range_of(m["loc"], lines), "selectionRange": range_of(member_range(text, m, m["name"]), lines),
                    })
                if children:
                    sym["children"] = children
        out.append(sym)
    return out


def folding_ranges(uri: str) -> list:
    text = docs.get(uri)
    if text is None:
        return []
    parsed = parse_source(text)
    if parsed["errors"]:
        return []
    out: list = []

    def visit(x: Any) -> None:
        if not x or not isinstance(x, (dict, list)):
            return
        if isinstance(x, list):
            for y in x:
                visit(y)
            return
        loc = x.get("loc")
        if loc and loc["el"] > loc["sl"] and (is_decl(x) or (is_type(x) and x["k"] == "record")
                                              or (is_expr(x) and x["e"] in ("obj", "arr", "match"))
                                              or (is_member(x) and x["m"] == "when")):
            out.append({"startLine": loc["sl"], "endLine": loc["el"], "kind": "region"})
        for k, v in x.items():
            if k != "loc" and v and isinstance(v, (dict, list)):
                visit(v)

    visit(parsed["decls"])
    seen: set = set()
    kept = []
    for r in out:
        key = f"{r['startLine']}-{r['endLine']}"
        if key not in seen:
            seen.add(key)
            kept.append(r)
    return kept


def formatting(uri: str) -> list:
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
    return [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": len(lines) - 1, "character": u16(lines[-1])}}, "newText": out}]


# ---------------- rename ----------------
def prepare_rename(uri: str, pos: dict) -> Any:
    a = analysis_of(uri)
    s = site_at(a, uri, _pos_of(uri, pos)) if a else None
    if not s or s["site"] is None or not s["hit"] or not s["hit"]["node"].get("loc"):
        return None
    n = s["hit"]["node"]
    text = text_of(a, s["module"])
    if is_expr(n) and n["e"] == "member":
        loc = member_token_loc(text, n)
    elif is_type(n):
        loc = type_name_loc(n, n["name"].index(".") + 1 if "." in n["name"] else 0, n["name"].split(".")[-1])
    elif is_decl(n) or is_member(n):
        loc = s["site"].range
    else:
        loc = n["loc"]
    return {"range": range_of(loc, text.split("\n")), "placeholder": s["site"].name}


def rename(uri: str, pos: dict, new_name: str) -> Any:
    refs = references(uri, pos, True)
    if not refs:
        return None
    a = analysis_of(uri)
    changes: dict = {}
    for m, loc in refs:
        changes.setdefault(uri_of(m.path), []).append({"range": range_of(loc, text_of(a, m).split("\n")), "newText": new_name})
    return {"changes": changes}


# ---------------- lenses and commands ----------------
def code_lenses(uri: str) -> list:
    text = docs.get(uri)
    if text is None:
        return []
    parsed = parse_source(text)
    if parsed["errors"]:
        return []
    lines = text.split("\n")
    out: list = []
    for d in parsed["decls"]:
        if not d.get("loc"):
            continue
        loc = d["loc"]
        head = {"sl": loc["sl"], "sc": loc["sc"], "el": loc["sl"], "ec": loc["sc"]}
        if d["d"] == "output":
            out.append({"range": range_of(head, lines), "command": {"title": "evaluate", "command": "decl.evaluate", "arguments": [uri, d["name"]]}})
        if d["d"] == "input":
            out.append({"range": range_of(head, lines), "command": {"title": "validate", "command": "decl.validate", "arguments": [uri, d["name"]]}})
    return out


def execute_command(command: str, args: Any) -> Any:
    args = args or []
    uri = args[0] if len(args) > 0 else None
    root = args[1] if len(args) > 1 else None
    if not isinstance(uri, str):
        return None
    session = Session(path_of(uri), overlay)
    for name, file in config["inputs"].items():
        try:
            session.apply({"op": "bind", "name": name, "src": {"kind": "file", "file": file,
                           "text": read_text(os.path.join(os.path.dirname(path_of(uri)), file))}})
        except Exception:
            pass   # reported by :validate
    if command == "decl.evaluate":
        r = session.evaluate([root] if root else [])
        run, ds = r["run"], r["docs"]
        diags = [fmt_diag(d) for d in run.load_diags + [c["diag"] for c in run.checks] + run.diags]
        if root:
            return {"root": root, "document": ds[0]["json"] if ds else None, "diagnostics": diags}
        all_ = "{" + ",".join(f"{json.dumps(d['name'])}:{d['json']}" for d in ds) + "}" if run.eng is not None and all(d["json"] is not None for d in ds) else None
        return {"root": None, "document": all_, "diagnostics": diags}
    if command == "decl.validate":
        r = session.validate([root] if root else [])
        run = r["run"]
        return {"verdicts": r["verdicts"], "diagnostics": [fmt_diag(d) for d in run.load_diags + [c["diag"] for c in run.checks] + r["diags"]]}
    if command == "decl.trace":
        return {"lines": session.trace(root)} if root else None
    if command == "decl.reloadWorkspace":
        analyses.clear()
        for u in list(docs):
            analyze(u)
        return None
    return None


# ---------------- request handling ----------------
def handle(msg: dict) -> None:
    id_ = msg.get("id")
    method = msg.get("method")
    params = msg.get("params") or {}
    if method == "initialize":
        reply(id_, {
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
                "executeCommandProvider": {"commands": ["decl.evaluate", "decl.validate", "decl.trace", "decl.reloadWorkspace"]},
            },
            "serverInfo": {"name": "decl-lsp", "version": "0.3.0"},
        })
    elif method == "initialized":
        pass
    elif method == "workspace/didChangeConfiguration":
        config["inputs"] = ((params.get("settings") or {}).get("decl") or {}).get("inputs") or {}
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
        refs = references(params["textDocument"]["uri"], params["position"], bool((params.get("context") or {}).get("includeDeclaration")))
        reply(id_, [location(a, m, loc) for m, loc in refs])
    elif method == "textDocument/documentHighlight":
        uri = params["textDocument"]["uri"]
        path = path_of(uri)
        a = analysis_of(uri)
        refs = references(uri, params["position"], True)
        reply(id_, [{"range": range_of(loc, text_of(a, m).split("\n")), "kind": 1} for m, loc in refs if m.path == path])
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
    elif method == "workspace/executeCommand":
        reply(id_, execute_command(params.get("command"), params.get("arguments")))
    elif method == "shutdown":
        reply(id_, None)
    elif method == "exit":
        sys.exit(0)
    elif "id" in msg:
        reply(id_, None)


def main(argv: Optional[list] = None) -> int:
    global _out
    _out = sys.stdout.buffer
    inp = sys.stdin.buffer
    buf = b""
    while True:
        chunk = inp.read1(65536) if hasattr(inp, "read1") else inp.read(65536)
        if not chunk:
            return 0   # stdin closed: exit after the queued messages (all handled synchronously)
        buf += chunk
        while True:
            header_end = buf.find(b"\r\n\r\n")
            if header_end < 0:
                break
            header = buf[:header_end].decode("ascii", "replace")
            m = re.search(r"Content-Length: (\d+)", header, re.IGNORECASE)
            if not m:
                buf = buf[header_end + 4:]
                continue
            length = int(m.group(1))
            if len(buf) < header_end + 4 + length:
                break
            body = buf[header_end + 4:header_end + 4 + length]
            buf = buf[header_end + 4 + length:]
            try:
                handle(json.loads(body.decode("utf-8")))
            except SystemExit:
                raise
            except Exception as e:   # pragma: no cover
                log_err(f"{type(e).__name__}: {e}")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
