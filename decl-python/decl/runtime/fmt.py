"""Canonical formatter — a port of the reference implementation's fmt.ts
(ROADMAP Phase 4; §2.1/D1): LF, 4-space indentation, no tabs, normalized
intra-line spacing. The original line structure is preserved — §2.9
makes newlines separators, so where a construct breaks lines is the
author's statement — and the formatter re-derives indentation and token
spacing deterministically, which makes it idempotent by construction."""
from __future__ import annotations

import re
from typing import Optional

from tree_sitter import Node, Parser

from .._tree_sitter import LANGUAGE

_parser: Optional[Parser] = None


class Leaf:
    __slots__ = ("text", "type", "parent", "row", "end_row", "col")

    def __init__(self, text: str, type_: str, parent: str, row: int, end_row: int, col: int) -> None:
        self.text, self.type, self.parent, self.row, self.end_row, self.col = text, type_, parent, row, end_row, col


# atoms: leaves kept verbatim, including their internal whitespace
ATOMS = {"string", "template_string", "pattern", "unit_literal", "doc_comment", "line_comment", "block_comment"}
KEYWORDY = re.compile(r"^[A-Za-z_$][A-Za-z0-9_]*$")
BIN_OPS = {"=", "==", "!=", "<=", ">=", "+", "*", "/", "%", "&&", "||", "??",
           "|>", "=>", "<<", ">>", "in", "matches", "with", "then", "else", "for", "if", "as", "from"}
CONT_STARTERS = {"else", "=", "for", "if", "&&", "||", "|>", "??", ".", "?.",
                 "+", "-", "*", "/", "==", "!=", "<=", ">=", "<", ">", "=>", "then"}
# a line whose last token leaves an expression open (`=`, `=>`, a binary
# operator, `then`/`else`) makes the next line a continuation too
CONT_ENDERS = {"=", "=>", "&&", "||", "|>", "??", "+", "-", "*", "/", "%",
               "==", "!=", "<=", ">=", "<", ">", "&", "|", "^", "<<", ">>", "..", "..<",
               "then", "else", "in", "with", "matches"}
KEYWORDS = {"type", "const", "func", "output", "input", "export", "import",
            "diagnostic", "dimension", "unit", "assert", "when", "if", "then", "else", "match", "for",
            "in", "with", "as", "from", "true", "false", "null", "error", "warn", "info", "matches"}


def u16(s: str) -> int:
    """a JavaScript string's length: UTF-16 code units"""
    return len(s.encode("utf-16-le")) // 2


def _collect(n: Node, lines: list, out: list) -> None:
    if n.type in ATOMS or n.child_count == 0:
        text = n.text.decode("utf-8")
        if not text:
            return   # zero-width externals (NEWLINE)
        row = n.start_point[0]
        col = u16(lines[row][:n.start_point[1]].decode("utf-8", "replace")) if row < len(lines) else 0
        out.append(Leaf(text, n.type, n.parent.type if n.parent else "", row, n.end_point[0], col))
        return
    for c in n.children:
        _collect(c, lines, out)


def _is_type_angle(l: Leaf) -> bool:
    return l.text in ("<", ">") and l.parent in ("type_arguments", "type_parameters")


def _is_keyword(t: str) -> bool:
    return t in KEYWORDS


def _spaced(a: Leaf, b: Leaf, prev: Optional[Leaf]) -> bool:
    """spacing decision: does a space go between a and b on one line?"""
    at, bt = a.text, b.text
    # comments keep at least one space before them (handled by caller)
    if b.type.endswith("comment"):
        return True
    if _is_type_angle(a):
        if at == "<":
            return False   # '>' falls through
    if _is_type_angle(b):
        return False   # Vec<...>, no space before either angle
    if at in ("(", "["):
        return False
    if bt in (")", "]", ",", ":"):
        return False
    if bt == "?" or at == "?":
        return False   # int?, name?:
    if at in (".", "?.") or bt in (".", "?."):
        return False
    if bt == ";":
        return False
    if at in ("..", "..<") or bt in ("..", "..<"):
        return False
    if bt == "(":
        # call/parameter parens attach to a name or closing bracket; grouping parens do not
        return not (KEYWORDY.match(at) and not _is_keyword(at)) and at != ")" and at != "]" and not _is_type_angle(a)
    if bt == "[":
        # index/size brackets attach (also after a record type or literal: `{...}[]`); array literals stand off
        return not (KEYWORDY.match(at) or at == ")" or at == "]" or at == "}" or _is_type_angle(a))
    if at == "{" or bt == "}":
        return True   # { a: 1 }
    if bt == "{" or at == "}":
        return True
    if at in ("!", "~"):
        return False   # unary
    if at in ("-", "+"):
        # unary sign: previous token is an operator, opener, or keyword
        p = prev.text if prev else None
        unary = p is None or p in BIN_OPS or p in ("(", "[", "{", ",", ":", "<", "..", "..<", "-", "+", "!", "~") \
            or (bool(KEYWORDY.match(p)) and _is_keyword(p))
        if unary:
            return False
    return True


def format_source(src: str) -> str:
    global _parser
    if _parser is None:
        _parser = Parser(LANGUAGE)
    data = src.encode("utf-8")
    tree = _parser.parse(data)
    if tree.root_node.has_error:
        raise ValueError("cannot format: file has parse errors")
    lines_b = data.split(b"\n")
    leaves: list = []
    _collect(tree.root_node, lines_b, leaves)

    # group leaves by their original starting row
    lines: list = []
    row_of: dict = {}
    for l in leaves:
        bucket = row_of.get(l.row)
        if bucket is None:
            bucket = []
            row_of[l.row] = bucket
            lines.append(bucket)
        bucket.append(l)

    out: list = []
    depth = 0
    last_row_end = -1   # last original row consumed (multiline atoms span rows)
    last_code: Optional[Leaf] = None   # the previous line's last non-comment token
    for line in lines:
        first = line[0]
        if first.row <= last_row_end:
            continue   # inside a multiline atom
        # one blank line max between constructs
        if out and first.row > last_row_end + 1:
            out.append("")
        # indentation: bracket depth, closers on the line start dedent first
        closers = 0
        for l in line:
            if l.text in (")", "]", "}"):
                closers += 1
            else:
                break
        indent = max(0, depth - closers)
        # a line starting with a continuation token, or following a line that
        # left an expression open, hangs one level deeper
        if closers == 0 and (first.text in CONT_STARTERS
                             or (last_code is not None and last_code.type not in ATOMS and last_code.text in CONT_ENDERS
                                 and not _is_type_angle(last_code))):   # `ref<...>` closes a type, it opens nothing
            indent = depth + 1
        text = "    " * indent
        prev: Optional[Leaf] = None
        prev2: Optional[Leaf] = None
        for l in line:
            if prev is not None:
                if l.type.endswith("comment"):
                    # inline comment: keep the author's alignment (min one space)
                    text += " " * max(1, l.col - (prev.col + u16(prev.text)))
                elif _spaced(prev, l, prev2):
                    text += " "
            text += l.text
            if l.type not in ATOMS:
                for ch in l.text:
                    if ch in "{[(":
                        depth += 1
                    elif ch in "}])":
                        depth = max(0, depth - 1)
            prev2, prev = prev, l
            if not l.type.endswith("comment"):
                last_code = l
            last_row_end = max(last_row_end, l.end_row)
        out.append(re.sub(r"[ \t]+$", "", text))
    return "\n".join(out) + "\n"
