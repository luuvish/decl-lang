"""YAML for documents (docs/tooling/05_render.md §2, §4): a reader of
YAML 1.2 with the core schema into the JSON data model — the values
`read_json` produces, so that a document written in YAML is
indistinguishable from the same document written in JSON from the
reader on — and a writer of the block-style form that every YAML 1.2
reader (and no YAML 1.1 reader) reads back as the canonical JSON
document. Beside them, the JSON layouts of §4.1. The reader accepts
exactly what the document says and refuses the rest with the reason
and the line, so that the three implementations refuse the same texts
with the same words. A port of the reference's yaml.ts."""

from __future__ import annotations

import re
from typing import Any, NoReturn

from .semantics import JObj, is_bool, is_int, js_num_str, json_str


class YamlError(Exception):
    """a document the reader refuses: `<reason> at line L`"""

    def __init__(self, reason: str, line: int):
        super().__init__(f"{reason} at line {line}")
        self.reason, self.line = reason, line


def is_yaml_path(p: str) -> bool:
    """a document path names YAML by its extension (§2); anything else is JSON"""
    return p.lower().endswith((".yaml", ".yml"))


# ---------------- the core schema (§2): what a plain scalar means ----------------
# null, bool, int (decimal, octal, hexadecimal), float — everything else
# is a string. YAML 1.1's spellings (yes/no/on/off, sexagesimals,
# timestamps, `1_000`) are strings.
_RE_INT = re.compile(r"^[-+]?[0-9]+$")
_RE_OCT = re.compile(r"^0o[0-7]+$")
_RE_HEX = re.compile(r"^0x[0-9a-fA-F]+$")
_RE_FLOAT = re.compile(r"^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$")
_RE_NONFINITE = re.compile(r"^([-+]?\.(inf|Inf|INF)|\.(nan|NaN|NAN))$")
NONFINITE = object()


def resolve_plain(s: str) -> Any:
    """the core schema's reading of a plain scalar: the value, or NONFINITE"""
    if s in ("", "~", "null", "Null", "NULL"):
        return None
    if s in ("true", "True", "TRUE"):
        return True
    if s in ("false", "False", "FALSE"):
        return False
    if _RE_INT.match(s):
        return int(s, 10)
    if _RE_OCT.match(s):
        return int(s[2:], 8)
    if _RE_HEX.match(s):
        return int(s[2:], 16)
    if _RE_FLOAT.match(s):
        return float(s)
    if _RE_NONFINITE.match(s):
        return NONFINITE
    return s


# ---------------- the reader ----------------
def _is_space(c: str | None) -> bool:
    return c in (" ", "\t")


def _is_break_or_end(c: str | None) -> bool:
    return c is None or c == "\n"


_FLOW_END = frozenset(",[]{}")


class _Reader:
    def __init__(self, src: str):
        text = src[1:] if src.startswith("\ufeff") else src
        self.s = text.replace("\r\n", "\n").replace("\r", "\n")
        self.i = 0
        self.anchors: dict[str, Any] = {}

    # ---- positions ----
    def line(self, at: int) -> int:
        return 1 + self.s.count("\n", 0, min(at, len(self.s)))

    def fail(self, reason: str, at: int | None = None) -> NoReturn:
        raise YamlError(reason, self.line(self.i if at is None else at))

    def at(self, k: int) -> str | None:
        return self.s[k] if 0 <= k < len(self.s) else None

    def peek(self, o: int = 0) -> str | None:
        return self.at(self.i + o)

    def at_end(self, k: int | None = None) -> bool:
        return _is_break_or_end(self.at(self.i if k is None else k))

    def line_start(self, k: int) -> int:
        while k > 0 and self.s[k - 1] != "\n":
            k -= 1
        return k

    def col(self) -> int:
        return self.i - self.line_start(self.i)

    def indent_of(self, k: int) -> int:
        """the indentation of the line holding k: its leading spaces (a tab there is refused)"""
        j = self.line_start(k)
        n = 0
        while self.at(j) == " ":
            j += 1
            n += 1
        if self.at(j) == "\t":
            self.fail("tab in indentation", j)
        return n

    def skip_inline(self) -> None:
        """skip spaces and a comment on the current line; stays before its break"""
        while _is_space(self.peek()):
            self.i += 1
        if self.peek() == "#":
            while not self.at_end():
                self.i += 1

    def end_line(self, what: str) -> None:
        """the rest of the current line must be empty (a comment allowed)"""
        self.skip_inline()
        if not self.at_end():
            self.fail(f"unexpected content after {what}")

    def next_content(self) -> int:
        """advance over line breaks, blank lines, and comment lines to the next
        content character, and return its column; -1 at the end. Idempotent:
        at a content character with only indentation before it, stays."""
        while True:
            c = self.peek()
            if c is None:
                return -1
            if c == "\n":
                self.i += 1
                continue
            start = self.line_start(self.i)
            j = start
            while self.at(j) == " ":
                j += 1
            if j < self.i:
                # mid-line: the caller left content behind — it belongs to no node
                self.fail("unexpected content")
            if self.at(j) == "\t":
                self.fail("tab in indentation", j)
            self.i = j
            c = self.peek()
            if c is None:
                return -1
            if c == "\n":
                continue
            if c == "#":
                while not self.at_end():
                    self.i += 1
                continue
            return j - start

    def indicator_at(self, k: int) -> bool:
        """is a `-`, `?`, or `:` at k an indicator (followed by a space or the line's end)?"""
        return _is_space(self.at(k + 1)) or _is_break_or_end(self.at(k + 1))

    def at_marker(self) -> bool:
        """a document marker (`---` or `...`) at column 0 ends every node"""
        return self.line_is("---") or self.line_is("...")

    def line_is(self, text: str) -> bool:
        return (
            self.s.startswith(text, self.i)
            and self.col() == 0
            and self.indicator_at(self.i + len(text) - 1)
        )

    def dash_here(self) -> bool:
        return self.peek() == "-" and self.indicator_at(self.i)

    # ---- the stream ----
    def document(self) -> Any:
        # directives and the start marker
        while True:
            if self.peek() == "%" and self.col() == 0:
                end = self.s.find("\n", self.i)
                text = self.s[self.i : (len(self.s) if end < 0 else end)]
                if re.match(r"^%YAML[ \t]+1\.2[ \t]*(#.*)?$", text):
                    self.i += len(text)
                    continue
                if text.startswith("%TAG"):
                    self.fail("uses a tag")
                if text.startswith("%YAML"):
                    self.fail("unsupported YAML version")
                word = re.split(r"[ \t]", text)[0]
                self.fail(f"unsupported directive {word}")
            n = self.next_content()
            if n == -1:
                return None
            if self.peek() == "%" and n == 0:
                continue
            break
        value: Any = None
        parsed = False
        if self.line_is("---"):
            self.i += 3
            self.skip_inline()
            if not self.at_end():
                value = self.node(-1, "seq", -1)
                parsed = True
        elif self.line_is("..."):
            self.fail("unexpected end marker")
        if not parsed:
            value = self.node(-1, "none", -1)
        # the tail: blank lines and comments, an end marker, nothing else
        ended = False
        while True:
            if self.next_content() == -1:
                return value
            if self.line_is("..."):
                self.i += 3
                self.end_line("the end marker")
                ended = True
                continue
            if self.line_is("---"):
                self.fail("stream holds more than one document")
            self.fail("unexpected content after the end marker" if ended else "unexpected content")

    # ---- block nodes ----
    def node(self, parent: int, where: str, seq_at: int) -> Any:
        """the node whose text starts at the cursor (inline: on the line of the
        `- ` or `key: ` it follows) or on the following lines (indented more
        than `parent`; a sequence may sit at `seq_at` = the parent mapping's
        own indentation). `where` says what the inline position follows.
        None for an empty node, leaving the cursor where it was."""
        self.skip_inline()
        if not self.at_end():
            return self.node_at(self.col(), where, parent)
        save = self.i
        n = self.next_content()
        if n == -1:
            return None
        dash = self.dash_here()
        if (n <= parent and not (dash and seq_at == n)) or self.at_marker():
            self.i = save
            return None
        return self.node_at(n, "none", parent)

    def node_at(self, ind: int, where: str, parent: int) -> Any:
        c = self.peek()
        if c == "&":
            name = self.anchor_name()
            self.skip_inline()
            if self.at_end():
                v = self.node(parent, where, -1)
            else:
                v = self.node_at(self.col(), where, parent)
            self.anchors[name] = v
            return v
        if c == "*":
            at = self.i
            name = self.anchor_name()
            if name not in self.anchors:
                self.fail(f"unknown alias *{name}", at)
            self.end_line("an alias")
            return _copy_of(self.anchors[name])
        if c == "!":
            self.fail("uses a tag")
        if c in ("@", "`"):
            self.fail(f"reserved indicator {c}")
        if c == "%":
            self.fail("unexpected directive")
        if c in ("[", "{"):
            v = self.flow_node()
            self.end_line("a flow collection")
            return v
        if c in ("|", ">"):
            return self.block_scalar(parent)
        if c in ('"', "'"):
            start = self.i
            text = self.quoted()
            while _is_space(self.peek()):
                self.i += 1
            if self.peek() == ":" and self.indicator_at(self.i):
                if where == "map":
                    self.fail("unexpected mapping value")
                self.i = start
                return self.mapping(ind)
            self.end_line("a scalar")
            return text
        if c == "-" and self.indicator_at(self.i):
            if where == "map":
                self.fail("unexpected sequence")
            return self.sequence(ind)
        if c == "?" and self.indicator_at(self.i):
            self.fail("mapping key is not a string")
        if c == ":" and self.indicator_at(self.i):
            self.fail("unexpected ':'")
        # a plain scalar — a mapping when `: ` follows it on the line
        if self.plain_is_key():
            if where == "map":
                self.fail("unexpected mapping value")
            return self.mapping(ind)
        return self.plain_scalar(parent)

    def anchor_name(self) -> str:
        self.i += 1  # & or *
        start = self.i
        while not self.at_end() and not _is_space(self.peek()) and self.peek() not in _FLOW_END:
            self.i += 1
        if self.i == start:
            self.fail("missing anchor name", start - 1)
        return self.s[start : self.i]

    def plain_is_key(self) -> bool:
        """does the plain text at the cursor end in a `: ` on this line (before any comment)?"""
        k = self.i
        while True:
            c = self.at(k)
            if _is_break_or_end(c):
                return False
            if c == "#" and k > self.i and _is_space(self.at(k - 1)):
                return False
            if c == ":" and self.indicator_at(k):
                return True
            k += 1

    def plain_line(self) -> str:
        """the plain text on the current line up to a comment or the line's end (not a key)"""
        start = self.i
        end = self.i
        while True:
            c = self.at(end)
            if _is_break_or_end(c):
                break
            if c == "#" and end > start and _is_space(self.at(end - 1)):
                break
            if c == ":" and self.indicator_at(end):
                self.fail("unexpected ':'", end)
            end += 1
        self.i = end
        text = self.s[start:end].rstrip(" \t")
        self.skip_inline()
        return text

    def plain_scalar(self, parent: int) -> Any:
        at = self.i
        text = self.plain_line()
        # continuation lines: indented more than the parent, folded with a
        # space; blank lines between fold to line breaks
        while True:
            save = self.i
            blanks = 0
            k = self.i
            if self.at(k) != "\n":
                break
            found = -1
            while self.at(k) == "\n":
                k += 1
                j = k
                while self.at(j) in (" ", "\t"):
                    j += 1
                if self.at(j) == "\n":
                    blanks += 1
                    k = j
                    continue
                if self.at(j) is None:
                    break
                found = j
                break
            if found < 0:
                break
            ind = self.indent_of(found)
            c = self.at(found)
            if ind <= parent or c == "#":
                break
            if c in ("-", "?", ":") and self.indicator_at(found):
                break
            self.i = found
            if self.at_marker() or self.plain_is_key():
                self.i = save
                break
            more = self.plain_line()
            text += ("\n" * blanks if blanks else " ") + more
            if self.i == save:
                break
        r = resolve_plain(text)
        if r is NONFINITE:
            self.fail("non-finite float", at)
        return r

    def mapping(self, ind: int) -> Any:
        entries: list[Any] = []
        seen: set[str] = set()
        while True:
            at = self.i
            key = self.key()
            if key in seen:
                self.fail(f"mapping repeats the key {json_str(key)}", at)
            seen.add(key)
            value = self.node(ind, "map", ind)
            entries.append((key, value))
            n = self.next_content()
            if n == -1 or n < ind or self.at_marker():
                break
            if n > ind:
                self.fail("bad indentation")
            if self.dash_here():
                self.fail("unexpected sequence")
        return JObj(entries)

    def key(self) -> str:
        """a mapping key at the cursor, and the `:` after it"""
        c = self.peek()
        if c in ('"', "'"):
            key = self.quoted()
        elif (c == "?" and self.indicator_at(self.i)) or c in ("&", "*"):
            self.fail("mapping key is not a string")
        elif c == "!":
            self.fail("uses a tag")
        elif c in ("[", "{"):
            self.fail("mapping key is not a string")
        else:
            start = self.i
            end = self.i
            while True:
                ch = self.at(end)
                if _is_break_or_end(ch):
                    self.fail("missing ':' after a mapping key", start)
                if ch == "#" and end > start and _is_space(self.at(end - 1)):
                    self.fail("missing ':' after a mapping key", start)
                if ch == ":" and self.indicator_at(end):
                    break
                end += 1
            text = self.s[start:end].rstrip(" \t")
            r = resolve_plain(text)
            if not isinstance(r, str):
                self.fail("mapping key is not a string", start)
            key = text
            self.i = end
        while _is_space(self.peek()):
            self.i += 1
        if not (self.peek() == ":" and self.indicator_at(self.i)):
            self.fail("missing ':' after a mapping key")
        self.i += 1
        return key

    def sequence(self, ind: int) -> Any:
        items: list[Any] = []
        while True:
            self.i += 1  # the dash
            items.append(self.node(ind, "seq", -1))
            n = self.next_content()
            if n == -1 or n < ind or self.at_marker():
                break
            if n > ind:
                self.fail("bad indentation")
            if not self.dash_here():
                break
        return items

    # ---- scalars ----
    def quoted(self) -> str:
        """a single- or double-quoted scalar at the cursor, folded over lines"""
        q = self.peek()
        at = self.i
        self.i += 1
        out = ""
        while True:
            c = self.peek()
            if c is None:
                self.fail("unterminated quoted scalar", at)
            if c == q:
                if q == "'" and self.peek(1) == "'":
                    out += "'"
                    self.i += 2
                    continue
                self.i += 1
                return out
            if c == "\n":
                # folding: one break is a space, further breaks are kept
                breaks = 0
                while self.peek() == "\n" or _is_space(self.peek()):
                    if self.peek() == "\n":
                        breaks += 1
                    self.i += 1
                out = out.rstrip(" \t") + ("\n" * (breaks - 1) if breaks > 1 else " ")
                continue
            if q == '"' and c == "\\":
                self.i += 1
                out += self.escape()
                continue
            out += c
            self.i += 1

    def escape(self) -> str:
        c = self.peek()
        at = self.i - 1
        self.i += 1
        simple = {
            "0": "\0",
            "a": "\x07",
            "b": "\b",
            "t": "\t",
            "\t": "\t",
            "n": "\n",
            "v": "\v",
            "f": "\f",
            "r": "\r",
            "e": "\x1b",
            " ": " ",
            '"': '"',
            "/": "/",
            "\\": "\\",
            "N": "\x85",
            "_": "\xa0",
            "L": "\u2028",
            "P": "\u2029",
        }
        if c is not None and c in simple:
            return simple[c]
        if c in ("x", "u", "U"):
            length = 2 if c == "x" else 4 if c == "u" else 8
            hexs = self.s[self.i : self.i + length]
            if not re.match(rf"^[0-9a-fA-F]{{{length}}}$", hexs):
                self.fail("bad escape", at)
            self.i += length
            cp = int(hexs, 16)
            if cp > 0x10FFFF or 0xD800 <= cp <= 0xDFFF:
                self.fail("bad escape", at)
            return chr(cp)
        if c == "\n":
            # an escaped line break joins the lines; leading white space is dropped
            while _is_space(self.peek()):
                self.i += 1
            return ""
        self.fail("bad escape", at)

    def block_scalar(self, parent: int) -> str:
        """a block scalar (`|` or `>`) with its indicators; `parent` is the enclosing indentation"""
        at = self.i
        folded = self.peek() == ">"
        self.i += 1
        chomp = "clip"
        explicit = 0
        for _ in range(2):
            c = self.peek()
            if c in ("-", "+"):
                if chomp != "clip":
                    self.fail("bad block scalar header", at)
                chomp = "strip" if c == "-" else "keep"
                self.i += 1
            elif c is not None and "1" <= c <= "9":
                if explicit:
                    self.fail("bad block scalar header", at)
                explicit = ord(c) - 48
                self.i += 1
        self.end_line("a block scalar header")
        # the content lines: those indented at least the content indentation
        # (explicit, or the first non-blank line's), until a lesser one
        lines: list[list[Any]] = []  # [text, blank]
        indent = max(parent, 0) + explicit if explicit else -1
        k = self.i
        end_at = self.i
        while self.at(k) == "\n":
            start = k + 1
            j = start
            while self.at(j) == " ":
                j += 1
            blank = self.at(j) == "\n" or self.at(j) is None
            line_indent = j - start
            if blank:
                e = j
                while self.at(e) != "\n" and self.at(e) is not None:
                    e += 1
                lines.append(
                    [
                        " " * (line_indent - indent)
                        if indent >= 0 and line_indent > indent
                        else "",
                        True,
                    ]
                )
                k = e
                end_at = e
                if self.at(e) is None:
                    break
                continue
            if indent < 0:
                if line_indent <= parent:
                    break
                indent = line_indent
                # blank lines before the first content line carry no spaces
                for ln in lines:
                    ln[0] = ""
            if line_indent < indent:
                break
            if self.at(j) == "\t" and line_indent == indent:
                self.fail("tab in indentation", j)
            e = j
            while self.at(e) != "\n" and self.at(e) is not None:
                e += 1
            lines.append([self.s[start + indent : e], False])
            k = e
            end_at = e
            if self.at(e) is None:
                break
        self.i = end_at
        # trailing blank lines are the chomping's business
        last = len(lines)
        while last > 0 and lines[last - 1][1]:
            last -= 1
        body = lines[:last]
        trailing = len(lines) - last
        text = ""
        if not folded:
            text = "\n".join(ln[0] for ln in body)
        else:
            # folding: a break between two normal lines is a space, blank lines
            # are kept as breaks, more-indented lines are kept as written
            def more_indented(x: list[Any]) -> bool:
                return not x[1] and (x[0].startswith(" ") or x[0].startswith("\t"))

            for n, ln in enumerate(body):
                if n == 0:
                    text = ln[0]
                    continue
                prev = body[n - 1]
                if ln[1] or prev[1] or more_indented(prev) or more_indented(ln):
                    text += "\n" + ln[0]
                else:
                    text += " " + ln[0]
        if not body:
            return "\n" * trailing if chomp == "keep" else ""
        if chomp == "strip":
            return text
        if chomp == "clip":
            return text + "\n"
        return text + "\n" * (trailing + 1)

    # ---- flow nodes ----
    def flow_ws(self) -> None:
        while True:
            c = self.peek()
            if c in (" ", "\t", "\n"):
                self.i += 1
                continue
            if c == "#" and (
                self.i == 0 or _is_space(self.at(self.i - 1)) or self.at(self.i - 1) == "\n"
            ):
                while not self.at_end():
                    self.i += 1
                continue
            return

    def flow_node(self) -> Any:
        self.flow_ws()
        c = self.peek()
        if c is None:
            self.fail("unterminated flow collection")
        if c == "&":
            name = self.anchor_name()
            v = self.flow_node()
            self.anchors[name] = v
            return v
        if c == "*":
            at = self.i
            name = self.anchor_name()
            if name not in self.anchors:
                self.fail(f"unknown alias *{name}", at)
            return _copy_of(self.anchors[name])
        if c == "!":
            self.fail("uses a tag")
        if c == "[":
            at = self.i
            self.i += 1
            items: list[Any] = []
            while True:
                self.flow_ws()
                if self.peek() == "]":
                    self.i += 1
                    return items
                if self.peek() is None:
                    self.fail("unterminated flow collection", at)
                if self.peek() == ",":
                    self.fail("unexpected ','")
                items.append(self.flow_node())
                self.flow_ws()
                if self.peek() == ":":
                    self.fail("unexpected ':'")
                if self.peek() == ",":
                    self.i += 1
                    continue
                if self.peek() == "]":
                    continue
                self.fail("expected ',' or ']'")
        if c == "{":
            at = self.i
            self.i += 1
            entries: list[Any] = []
            seen: set[str] = set()
            while True:
                self.flow_ws()
                if self.peek() == "}":
                    self.i += 1
                    return JObj(entries)
                if self.peek() is None:
                    self.fail("unterminated flow collection", at)
                if self.peek() == ",":
                    self.fail("unexpected ','")
                key_at = self.i
                key = self.flow_node()
                if not isinstance(key, str):
                    self.fail("mapping key is not a string", key_at)
                if key in seen:
                    self.fail(f"mapping repeats the key {json_str(key)}", key_at)
                seen.add(key)
                self.flow_ws()
                value: Any = None
                if self.peek() == ":":
                    self.i += 1
                    self.flow_ws()
                    if self.peek() not in (",", "}"):
                        value = self.flow_node()
                    self.flow_ws()
                entries.append((key, value))
                if self.peek() == ",":
                    self.i += 1
                    continue
                if self.peek() == "}":
                    continue
                self.fail("expected ',' or '}'")
        if c in ('"', "'"):
            return self.quoted()
        if c in ("]", "}"):
            self.fail(f"unexpected '{c}'")
        # a plain scalar in flow context: ends at an indicator, folded over lines
        at = self.i
        text = ""
        while True:
            start = self.i
            end = self.i
            while True:
                ch = self.at(end)
                if _is_break_or_end(ch) or ch in _FLOW_END:
                    break
                if ch == "#" and end > start and _is_space(self.at(end - 1)):
                    break
                if ch == ":" and (
                    _is_space(self.at(end + 1))
                    or _is_break_or_end(self.at(end + 1))
                    or self.at(end + 1) in _FLOW_END
                ):
                    break
                end += 1
            text += self.s[start:end].rstrip(" \t")
            self.i = end
            # a line break inside the scalar folds to a space
            k = end
            while _is_space(self.at(k)):
                k += 1
            if self.at(k) == "#":
                break
            if self.at(k) != "\n":
                break
            blanks = 0
            while self.at(k) == "\n" or _is_space(self.at(k)):
                if self.at(k) == "\n":
                    blanks += 1
                k += 1
            ch = self.at(k)
            if ch is None or ch in _FLOW_END or ch == "#" or (ch == ":" and self.indicator_at(k)):
                self.i = k
                break
            text += "\n" * (blanks - 1) if blanks > 1 else " "
            self.i = k
        if text == "":
            self.fail("unexpected content", at)
        r = resolve_plain(text)
        if r is NONFINITE:
            self.fail("non-finite float", at)
        return r


def _copy_of(v: Any) -> Any:
    """an alias is a copy of the anchored value"""
    if isinstance(v, list):
        return [_copy_of(x) for x in v]
    if isinstance(v, JObj):
        return JObj([(k, _copy_of(x)) for k, x in v.entries])
    return v


def read_yaml(src: str) -> Any:
    """read one YAML document (§2) into the JSON data model; raises YamlError"""
    return _Reader(src).document()


# ---------------- the writer (§4.2) ----------------
def _fmt_float(n: float) -> str:
    s = js_num_str(n)
    return s if ("." in s or "e" in s or "E" in s) else s + ".0"


# the YAML 1.1 spellings a 1.1 reader would take for a bool or a null
_YAML11_WORDS = frozenset(
    [
        "y",
        "Y",
        "yes",
        "Yes",
        "YES",
        "n",
        "N",
        "no",
        "No",
        "NO",
        "on",
        "On",
        "ON",
        "off",
        "Off",
        "OFF",
    ]
)
_INDICATORS = frozenset("[]{},&*!|>'\"%@`#")


def plain_safe(s: str) -> bool:
    """plain only when a YAML 1.2 reader reads it back as exactly this string
    and a YAML 1.1 reader has nothing to reinterpret: it starts with a
    letter or `_`, holds no indicator that could open a collection, an
    anchor, a tag, or a comment, no `: `, no `#`, no break, tab, or
    unprintable character, does not end in `:` or a space, and is not a
    word either schema reads as a bool or a null"""
    if not re.match(r"^[A-Za-z_]", s):
        return False
    if s in _YAML11_WORDS or not isinstance(resolve_plain(s), str):
        return False
    if s.endswith(":") or s.endswith(" "):
        return False
    if ": " in s or " #" in s:
        return False
    for ch in s:
        cp = ord(ch)
        if ch in _INDICATORS:
            return False
        if cp < 0x20 or cp == 0x7F or 0x80 <= cp <= 0x9F:
            return False
        if cp in (0xFEFF, 0xFFFE, 0xFFFF) or 0xD800 <= cp <= 0xDFFF:
            return False
    return True


def _yaml_str(s: str) -> str:
    return s if plain_safe(s) else json_str(s)


def _is_empty_coll(v: Any) -> bool:
    return (isinstance(v, list) and not v) or (isinstance(v, JObj) and not v.entries)


def _is_block(v: Any) -> bool:
    return isinstance(v, (list, JObj)) and not _is_empty_coll(v)


def _scalar_text(v: Any) -> str:
    if v is None:
        return "null"
    if is_bool(v):
        return "true" if v else "false"
    if is_int(v):
        return str(v)
    if isinstance(v, float):
        return _fmt_float(v)
    if isinstance(v, str):
        return _yaml_str(v)
    if isinstance(v, list):
        return "[]"
    if isinstance(v, JObj):
        return "{}"
    raise TypeError("to_yaml: unexpected value")


def _block_lines(v: Any, ind: str, step: str) -> list[str]:
    """the lines of a block node: the first without its indentation (the
    caller places it after `- ` or on a line of its own), the rest with
    `ind` in front of them"""
    out: list[str] = []
    if isinstance(v, list):
        for item in v:
            sub = _block_lines(item, ind + "  ", step) if _is_block(item) else [_scalar_text(item)]
            out.append((ind if out else "") + "- " + sub[0])
            out.extend(sub[1:])
        return out
    for k, x in v.entries:
        key = _yaml_str(k)
        if not _is_block(x):
            out.append((ind if out else "") + f"{key}: {_scalar_text(x)}")
            continue
        out.append((ind if out else "") + f"{key}:")
        sub = _block_lines(x, ind + step, step)
        out.append(ind + step + sub[0])
        out.extend(sub[1:])
    return out


def to_yaml(v: Any, indent: int = 2) -> str:
    """the YAML text of a JSON value (read_json's shape), block style, no trailing newline"""
    step = " " * indent
    return "\n".join(_block_lines(v, "", step)) if _is_block(v) else _scalar_text(v)


# ---------------- the JSON layouts (§4.1) ----------------
def to_json(v: Any, indent: int = 0) -> str:
    """the JSON text of a value (read_json's shape): canonical for indent 0,
    laid out with `indent` spaces per level otherwise"""

    def go(x: Any, ind: str) -> str:
        if x is None:
            return "null"
        if is_bool(x):
            return "true" if x else "false"
        if is_int(x):
            return str(x)
        if isinstance(x, float):
            return _fmt_float(x)
        if isinstance(x, str):
            return json_str(x)
        inner = ind + " " * indent
        if isinstance(x, list):
            if not x:
                return "[]"
            if indent == 0:
                return "[" + ",".join(go(e, ind) for e in x) + "]"
            return "[\n" + ",\n".join(inner + go(e, inner) for e in x) + f"\n{ind}]"
        if isinstance(x, JObj):
            if not x.entries:
                return "{}"
            if indent == 0:
                return "{" + ",".join(f"{json_str(k)}:{go(e, ind)}" for k, e in x.entries) + "}"
            return (
                "{\n"
                + ",\n".join(f"{inner}{json_str(k)}: {go(e, inner)}" for k, e in x.entries)
                + f"\n{ind}}}"
            )
        raise TypeError("to_json: unexpected value")

    return go(v, "")
