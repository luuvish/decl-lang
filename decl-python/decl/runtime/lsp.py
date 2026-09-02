"""Minimal LSP server over stdio — a port of the reference implementation's
lsp.ts (ROADMAP Phase 4): diagnostics first, then hover, then definition —
module-aware through the same loader the CLI uses, with open buffers
overriding the disk. Messages are handled strictly in order; the server
also exits when its stdin closes."""
from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path
from typing import Any, Optional
from urllib.parse import unquote, urlparse

from tree_sitter import Parser

from .._tree_sitter import LANGUAGE
from .checker import check_module
from .fmt import u16
from .module import load_modules
from .package import open_package_universe
from .parse import parse_source

_parser: Optional[Parser] = None
docs: dict = {}   # uri -> text (insertion-ordered, like the reference's Map)
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


# ---------------- documents & analysis ----------------
def path_of(uri: str) -> str:
    return unquote(urlparse(uri).path)


def uri_of(path: str) -> str:
    return Path(path).as_uri()


def parse_tree(src: str):
    global _parser
    if _parser is None:
        _parser = Parser(LANGUAGE)
    return _parser.parse(src.encode("utf-8"))


def anchor_for(src: str, message: str) -> tuple:
    """find an identifier's position to anchor a position-less diagnostic"""
    lines = src.split("\n")
    for n in re.findall(r"[A-Za-z_][A-Za-z0-9_.]*", message):
        if n in ("error", "in", "the", "a", "is", "not", "std"):
            continue
        pat = re.compile(r"\b" + re.escape(n) + r"\b")
        for i, line in enumerate(lines):
            mm = pat.search(line)
            if mm:
                return (i, u16(line[:mm.start()]), u16(line[:mm.start()]) + u16(n))
    return (0, 0, 1)


def analyze(uri: str) -> None:
    src = docs[uri]
    path = path_of(uri)
    lsp_diags: list = []

    def push(line: int, a: int, b: int, message: str, code: Optional[str]) -> None:
        d: dict = {"range": {"start": {"line": line, "character": a}, "end": {"line": line, "character": b}},
                   "severity": 1, "source": "decl"}
        if code is not None:
            d["code"] = code
        d["message"] = message
        lsp_diags.append(d)

    parsed = parse_source(src)
    if parsed["errors"]:
        for e in parsed["errors"]:
            push(e["row"], e["col"], e["col"] + 1, "syntax error", "E2001")
    else:
        pkg = open_package_universe(path)
        override = {path_of(u): text for u, text in docs.items()}
        r = load_modules(path, override, pkg["resolver"] if pkg else None)
        mine = next((m for m in r["modules"] if m.path == path), None)
        all_ = (pkg["diags"] if pkg else []) + r["diags"] + (check_module(mine.decls, mine.env) if mine else [])
        for d in all_:
            if d["severity"] != "error":
                continue
            line, a, b = anchor_for(src, d["message"])
            push(line, a, b, d["message"], d.get("code"))
    notify("textDocument/publishDiagnostics", {"uri": uri, "diagnostics": lsp_diags})


# ---------------- declarations index (hover / definition) ----------------
def _u16_col(line_bytes: bytes, byte_col: int) -> int:
    return u16(line_bytes[:byte_col].decode("utf-8", "replace"))


def decl_index(path: str, src: str) -> dict:
    out: dict = {}
    tree = parse_tree(src)
    lines = src.split("\n")
    lines_b = src.encode("utf-8").split(b"\n")
    for c in tree.root_node.named_children:
        if not c.type.endswith("_declaration"):
            continue
        name_node = c.child_by_field_name("name")
        if name_node is None:
            continue
        row = name_node.start_point[0]
        out[name_node.text.decode("utf-8")] = {
            "path": path, "row": row,
            "a": _u16_col(lines_b[row], name_node.start_point[1]), "b": _u16_col(lines_b[row], name_node.end_point[1]),
            "line": lines[c.start_point[0]].strip(), "kind": c.type.replace("_declaration", ""),
        }
    return out


def read_src(path: str) -> Optional[str]:
    for u, text in docs.items():
        if path_of(u) == path:
            return text
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return None


def _byte_col(line: str, u16_col: int) -> int:
    """an LSP character offset (UTF-16 units) as a byte offset into the line"""
    units = 0
    for i, ch in enumerate(line):
        if units >= u16_col:
            return len(line[:i].encode("utf-8"))
        units += u16(ch)
    return len(line.encode("utf-8"))


def find_decl(uri: str, pos: dict) -> Optional[dict]:
    """resolve the name under the cursor to its declaration site, following
    one import hop (named, renamed, or namespace member)"""
    src = docs.get(uri)
    if src is None:
        return None
    path = path_of(uri)
    tree = parse_tree(src)
    lines = src.split("\n")
    row = pos["line"]
    col = _byte_col(lines[row], pos["character"]) if row < len(lines) else 0
    node = tree.root_node.descendant_for_point_range((row, col), (row, col))
    if node is None or node.type != "identifier":
        return None
    word = node.text.decode("utf-8")

    local = decl_index(path, src)
    if word in local:
        return local[word]

    # namespace member: ns.word — look at the sibling chain
    prev_sib = node.parent.named_children[0] if (node.parent is not None and node.parent.type == "qualified_name") else None
    ns_name = prev_sib.text.decode("utf-8") if (prev_sib is not None and prev_sib.id != node.id) else None

    for d in parse_source(src)["decls"]:
        if d["d"] != "import":
            continue
        if d["from"].startswith("."):
            target: Optional[str] = os.path.normpath(os.path.join(os.path.dirname(path), d["from"]))
        else:
            p = open_package_universe(path)
            r = p["resolver"](d["from"], os.path.dirname(path)) if p else None
            target = r if isinstance(r, str) else None
        if not target:
            continue
        tsrc = read_src(target)
        if tsrc is None:
            continue
        if d.get("ns") is not None and d["ns"] == ns_name:
            tidx = decl_index(target, tsrc)
            if word in tidx:
                return tidx[word]
        for it in d.get("names") or []:
            if (it.get("as") or it["name"]) != word:
                continue
            tidx = decl_index(target, tsrc)
            if it["name"] in tidx:
                return tidx[it["name"]]
    return None


# ---------------- request handling ----------------
def handle(msg: dict) -> None:
    id_ = msg.get("id")
    method = msg.get("method")
    params = msg.get("params") or {}
    if method == "initialize":
        reply(id_, {
            "capabilities": {"textDocumentSync": 1, "hoverProvider": True, "definitionProvider": True},
            "serverInfo": {"name": "decl-lsp", "version": "0.2.0"},
        })
    elif method == "initialized":
        pass
    elif method == "textDocument/didOpen":
        docs[params["textDocument"]["uri"]] = params["textDocument"]["text"]
        analyze(params["textDocument"]["uri"])
    elif method == "textDocument/didChange":
        docs[params["textDocument"]["uri"]] = params["contentChanges"][0]["text"]
        analyze(params["textDocument"]["uri"])
    elif method == "textDocument/didClose":
        docs.pop(params["textDocument"]["uri"], None)
    elif method == "textDocument/hover":
        site = find_decl(params["textDocument"]["uri"], params["position"])
        reply(id_, {"contents": {"kind": "markdown", "value": f"**{site['kind']}** — `{site['line']}`"}} if site else None)
    elif method == "textDocument/definition":
        site = find_decl(params["textDocument"]["uri"], params["position"])
        reply(id_, {"uri": uri_of(site["path"]),
                    "range": {"start": {"line": site["row"], "character": site["a"]},
                              "end": {"line": site["row"], "character": site["b"]}}} if site else None)
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
