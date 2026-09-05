"""The REPL (docs/tooling/02_repl.md) — a port of the reference
implementation's repl.ts: an interactive session over a universe —
expressions evaluated partially, session outputs and declarations,
documents bound and edited with exact undo, and the command-line verbs
root for root. Everything it prints goes to standard output; a scripted
session (`--script`) prints the transcript the terminal would show, so
the three implementations can be diffed."""

from __future__ import annotations

import contextlib
import os
import re
import sys
from collections.abc import Callable
from typing import Any

from .session import Session, SessionError, fmt_diag, parse_decl, parse_expr, pretty_json

COMMANDS: list[Any] = [
    # the universe
    (":load file.decl", "open the universe from an entry module (a new session)", "universe"),
    (":reload", "re-read every module of the universe from disk", "universe"),
    (":roots", "the roots of the universe and of the session", "universe"),
    # documents
    (":bind name=doc.json", "bind a JSON file to an input", "documents"),
    (":bind name { … }", "bind an inline JSON document", "documents"),
    (":bind name = expr", "bind the value of an expression as the document", "documents"),
    (":unbind name", "drop the binding", "documents"),
    (":create path = expr", "add a member, entry, or element at a path of a document", "documents"),
    (":update path = expr", "replace the value at a path of a document", "documents"),
    (":remove path", "remove the value at a path of a document", "documents"),
    (":diff name", "the document against what it started from", "documents"),
    (":save name=file", "write the document of a root to a file", "documents"),
    # session declarations
    (":drop name", "remove a session declaration", "declarations"),
    (":write file.decl", "write the scratch module to a file", "declarations"),
    (":session", "the session's declarations and documents", "declarations"),
    (":reset", "drop every binding, edit, and declaration", "declarations"),
    # evaluation and validation
    (":check", "static diagnostics of every module", "evaluation"),
    (":evaluate [root…]", "full evaluation: the documents of the roots", "evaluation"),
    (
        ":validate [root…]",
        "full validation: every diagnostic, then a verdict per root",
        "evaluation",
    ),
    (":fmt", "the scratch module, canonically formatted", "evaluation"),
    # inspection
    (":type expr", "the static type of an expression", "inspection"),
    (":doc name", "a declaration and its documentation", "inspection"),
    (":path expr", "the canonical path of a place", "inspection"),
    (":trace path", "the derivation of a place, or its root cause", "inspection"),
    (":complete text", "the completions offered at the end of the text", "inspection"),
    # history
    (":undo [n]", "step the log back", "history"),
    (":redo [n]", "step forward again", "history"),
    (":history [file]", "the log, or write it as a session file", "history"),
    # the session
    (":time", "wall time of the last evaluation", "session"),
    (":set pretty|compact", "value printing", "session"),
    (":help [command]", "these commands", "session"),
    (":quit", "end the session", "session"),
]
COMMAND_NAMES: list[Any] = list(dict.fromkeys(c[0].split(" ")[0] for c in COMMANDS))

_DECL_HEAD = re.compile(
    r"^\s*(?:export\s+)?(type|const|func|output|input|diagnostic|dimension|unit|import)\b"
)
_OUTPUT_HEAD = re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*([^=]+?))?\s*=(?!=)\s*([\s\S]+)$")
_KEYWORDS = {
    "if",
    "then",
    "else",
    "for",
    "in",
    "match",
    "with",
    "matches",
    "true",
    "false",
    "null",
    "export",
}
_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def needs_more(text: str) -> bool:
    """does the input so far leave an expression open (§2.9)?"""
    depth = 0
    in_str: str | None = None
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if in_str:
            if c == "\\":
                i += 1
            elif c == in_str:
                in_str = None
            i += 1
            continue
        if c in ('"', "`"):
            in_str = c
        elif c == "/" and text[i + 1 : i + 2] == "/":
            nl = text.find("\n", i)
            if nl < 0:
                break
            i = nl
        elif c in "{[(":
            depth += 1
        elif c in "}])":
            depth -= 1
        i += 1
    if depth > 0 or in_str == "`":
        return True
    if text.lstrip().startswith(":"):
        return False  # a command: only an open bracket continues it
    tail = re.sub(r"//[^\n]*$", "", text, count=1).rstrip()
    return re.search(r"(?:[+\-*/%<>=!&|?:,]|\bthen|\belse|\bin|\bwith|=>)$", tail) is not None


class Repl:
    def __init__(self, out: Callable[[str], None], entry: str | None = None):
        self._out = out
        self.session = Session(entry)
        self.compact = False
        self.errors = 0
        self._buffer: list[Any] = []
        self.quit_requested = False

    def line(self, text: str) -> bool:
        """feed one line; returns True when the input is complete and was handled"""
        self._buffer.append(text)
        whole = "\n".join(self._buffer)
        if needs_more(whole):
            return False
        self._buffer = []
        self.input(whole)
        return True

    def pending(self) -> bool:
        return len(self._buffer) > 0

    def _error(self, msg: str) -> None:
        self.errors += 1
        self._out(f"error: {msg}")

    def _diag(self, d: dict[str, Any], in_file: str | None = None) -> None:
        self._out(fmt_diag(d, in_file))

    def _value(self, json_text: str) -> None:
        self._out(json_text if self.compact else pretty_json(json_text))

    def input(self, text: str) -> None:
        t = text.strip()
        if not t or re.match(r"^//(?!/)", t):
            return
        try:
            if t.startswith(":"):
                self._command(t)
            elif _DECL_HEAD.match(t):
                self._add_declaration(t)
            else:
                m = _OUTPUT_HEAD.match(t)
                if m and m.group(1) not in _KEYWORDS:
                    self._session_output(
                        m.group(1), m.group(2).strip() if m.group(2) else None, m.group(3).strip()
                    )
                else:
                    self._expression(t)
        except SessionError as e:
            self._error(str(e))

    def _expression(self, text: str) -> None:
        parse_expr(text)
        r = self.session.evaluate_expr(text)
        for d in r["diags"]:
            self._diag(d)
        err = r["error"]
        if err is not None:
            if err["message"]:
                self._out(
                    f"error{' [' + err['code'] + ']' if err.get('code') else ''}: {err['message']}"
                )
            self._out("(invalid)")
        else:
            self._value(r["value"])
        self._out("(partial)")

    def _add_declaration(self, text: str) -> None:
        r = parse_decl(text)
        self.session.apply({"op": "declare", "name": r["name"], "text": text.strip()})

    def _session_output(self, name: str, type_: str | None, expr: str) -> None:
        parse_expr(expr)
        if type_:
            parse_decl(f"output {name}: {type_} = 0")
        self.session.apply({"op": "output", "name": name, "type": type_, "expr": expr})

    def _command(self, t: str) -> None:
        m0 = re.search(r"\s", t)
        sp = m0.start() if m0 else -1
        cmd = t if sp < 0 else t[:sp]
        rest = "" if sp < 0 else t[sp + 1 :].strip()
        s = self.session

        def no_args() -> None:
            if rest:
                raise SessionError(f"{cmd} takes no argument")

        def one_name() -> str:
            if not _NAME.match(rest):
                raise SessionError(f"{cmd} expects a name")
            return rest

        if cmd == ":load":
            if not rest:
                raise SessionError(":load expects a file")
            self.session = Session(rest)
            return
        if cmd == ":reload":
            no_args()
            s.apply(s.reload_op())
            return
        if cmd == ":roots":
            no_args()
            rs = s.roots()
            if not rs:
                self._out("(no roots)")
                return
            for r in rs:
                if r.session:
                    status = "session"
                elif r.kind == "output":
                    status = (
                        "detached"
                        if r.binding == "detached"
                        else "exported"
                        if r.exported
                        else "local"
                    )
                else:
                    status = r.binding
                self._out(
                    f"{r.kind:<7} {r.name:<16} {status:<12} {r.module:<16} {r.detail}"
                    f"{' (edited)' if r.edited else ''}".rstrip()
                )
            return
        if cmd == ":bind":
            m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([\s\S]+)$", rest)
            if (
                m
                and not re.match(r"^[\[{]", m.group(2).strip())
                and not re.match(r"^\s", rest[len(m.group(1)) :])
            ):
                # name=file (no spaces around =)
                file = m.group(2).strip()
                try:
                    with open(file, encoding="utf-8") as f:
                        text = f.read()
                except OSError:
                    raise SessionError(f"cannot read {file}") from None
                s.apply(
                    {
                        "op": "bind",
                        "name": m.group(1),
                        "src": {"kind": "file", "file": file, "text": text},
                    }
                )
                return
            if m:
                s.apply(
                    {
                        "op": "bind",
                        "name": m.group(1),
                        "src": {"kind": "expr", "text": m.group(2).strip()},
                    }
                )
                return
            m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s+([\[{][\s\S]*)$", rest)
            if m:
                s.apply(
                    {
                        "op": "bind",
                        "name": m.group(1),
                        "src": {"kind": "inline", "text": m.group(2)},
                    }
                )
                return
            raise SessionError(":bind expects name=doc.json, name { … }, or name = expr")
        if cmd == ":unbind":
            s.apply({"op": "unbind", "name": one_name()})
            return
        if cmd in (":create", ":update"):
            m = re.match(r"^(\S+)\s*=\s*([\s\S]+)$", rest)
            if not m:
                raise SessionError(f"{cmd} expects path = expr")
            s.apply({"op": "edit", "kind": cmd[1:], "path": m.group(1), "expr": m.group(2).strip()})
            return
        if cmd == ":remove":
            if not rest or re.search(r"\s", rest):
                raise SessionError(":remove expects a path")
            s.apply({"op": "edit", "kind": "remove", "path": rest})
            return
        if cmd == ":diff":
            for l in s.diff(one_name()):
                self._out(l)
            return
        if cmd == ":save":
            m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)=(\S+)$", rest)
            if not m:
                raise SessionError(":save expects name=file")
            s.save(m.group(1), m.group(2))
            return
        if cmd == ":drop":
            s.apply({"op": "drop", "name": one_name()})
            return
        if cmd == ":write":
            if not rest:
                raise SessionError(":write expects a file")
            s.write(rest)
            return
        if cmd == ":session":
            no_args()
            ls = s.session_lines()
            if not ls:
                self._out("(empty session)")
            for l in ls:
                self._out(l)
            return
        if cmd == ":reset":
            no_args()
            s.apply({"op": "reset"})
            return
        if cmd == ":check":
            no_args()
            cs = s.check()
            for c in cs:
                self._diag(c["diag"], None if c["file"] == s.entry_abs else c["file"])
            if not cs:
                self._out("ok")
            return
        if cmd == ":evaluate":
            names = [n for n in re.split(r"[\s,]+", rest) if n] if rest else []
            r = s.evaluate(names)
            run, docs, exported = r["run"], r["docs"], r["exported"]
            for d in run.load_diags:
                self._diag(d)
            for c in run.checks:
                self._diag(c["diag"], None if c["file"] == s.entry_abs else c["file"])
            for c in run.session_checks:
                self._diag(c)
            for d in run.diags:
                self._diag(d)
            if run.entry is None:
                return
            if run.eng is None:
                self._out("(not evaluated)")
                return
            if exported:
                if any(d["json"] is None for d in docs):
                    self._out("(invalid)")
                    return
                from .semantics import json_str

                self._value(
                    "{" + ",".join(f"{json_str(d['name'])}:{d['json']}" for d in docs) + "}"
                )
                return
            for d in docs:
                if len(docs) > 1:
                    self._out(f"{d['name']}:")
                if d["json"] is None:
                    self._out("(invalid)")
                else:
                    self._value(d["json"])
            return
        if cmd == ":validate":
            names = [n for n in re.split(r"[\s,]+", rest) if n] if rest else []
            r = s.validate(names)
            run, verdicts, diags = r["run"], r["verdicts"], r["diags"]
            for d in run.load_diags:
                self._diag(d)
            for c in run.checks:
                self._diag(c["diag"], None if c["file"] == s.entry_abs else c["file"])
            for c in run.session_checks:
                self._diag(c)
            if run.eng is None:
                self._out("(not evaluated)")
                return
            for d in diags:
                self._diag(d)
            if not verdicts:
                self._out("(no roots)")
            for v in verdicts:

                def n(k: int, w: str) -> str:
                    return f"{k} {w}{'' if k == 1 else 's'}"

                if v["errors"] == 0 and v["warnings"] == 0:
                    self._out(f"{v['name']}: ok")
                else:
                    parts = [
                        n(v["errors"], "error") if v["errors"] else "",
                        n(v["warnings"], "warning") if v["warnings"] else "",
                    ]
                    self._out(f"{v['name']}: {', '.join(p for p in parts if p)}")
            return
        if cmd == ":fmt":
            no_args()
            t = s.fmt()
            if t:
                self._out(t[:-1] if t.endswith("\n") else t)
            else:
                self._out("(empty session)")
            return
        if cmd == ":type":
            if not rest:
                raise SessionError(":type expects an expression")
            r = s.type_of(rest)
            for d in r["diags"]:
                self._diag(d)
            self._out(f"{r['type']}{'  (maybe absent)' if r['maybe_absent'] else ''}")
            return
        if cmd == ":doc":
            if not rest:
                raise SessionError(":doc expects a name")
            for l in s.doc_of(rest):
                self._out(l)
            return
        if cmd == ":path":
            if not rest:
                raise SessionError(":path expects an expression")
            self._out(s.path_of(rest))
            return
        if cmd == ":trace":
            if not rest or re.search(r"\s", rest):
                raise SessionError(":trace expects a path")
            for l in s.trace(rest):
                self._out(l)
            return
        if cmd == ":complete":
            cs = s.complete(rest, COMMAND_NAMES)
            if not cs:
                self._out("(no completions)")
            for c in cs:
                self._out(c)
            return
        if cmd in (":undo", ":redo"):
            try:
                n_ = int(rest) if rest else 1
            except ValueError:
                n_ = 0
            if not n_ >= 1:
                raise SessionError(f"{cmd} expects a count")
            k = s.undo(n_) if cmd == ":undo" else s.redo(n_)
            if k == 0:
                self._out("nothing to undo" if cmd == ":undo" else "nothing to redo")
            return
        if cmd == ":history":
            if rest:
                try:
                    with open(rest, "w", encoding="utf-8") as f:
                        f.write("\n".join(s.script_lines()) + "\n")
                except OSError:
                    raise SessionError(f"cannot write {rest}") from None
                return
            for l in s.history_lines():
                self._out(l)
            return
        if cmd == ":time":
            no_args()
            tm = s.last_timing
            if tm is None:
                self._out("nothing evaluated yet")
                return

            def ms(x: float) -> str:
                return f"{x:.1f} ms"

            step = ""
            if "recomputed" in tm:
                step = f", recomputed {tm['recomputed']} of {tm['slots']} slots"
            self._out(
                f"total {ms(tm['total'])} (load {ms(tm['load'])}, check {ms(tm['check'])}, bind "
                f"{ms(tm['bind'])}, evaluate {ms(tm['evaluate'])}){step}"
            )
            return
        if cmd == ":set":
            if rest == "pretty":
                self.compact = False
            elif rest == "compact":
                self.compact = True
            else:
                raise SessionError(":set expects pretty or compact")
            return
        if cmd == ":help":
            if rest:
                want = rest if rest.startswith(":") else ":" + rest
                rows = [c for c in COMMANDS if c[0].split(" ")[0] == want]
            else:
                rows = COMMANDS
            if not rows:
                raise SessionError(f"unknown command {rest}")
            cat = ""
            for form, what, c in rows:
                if not rest and c != cat:
                    cat = c
                    self._out(f"{cat}:")
                self._out(f"  {form:<24} {what}")
            return
        if cmd == ":quit":
            no_args()
            self.quit_requested = True
            return
        raise SessionError(f"unknown command {cmd}")


# ---------------- the command ----------------
def _save_history(readline: Any, path: str) -> None:
    with contextlib.suppress(OSError):
        readline.write_history_file(path)


def run_repl(args: list[Any]) -> int:
    entry: str | None = None
    script: str | None = None
    compact = False
    inputs: list[Any] = []
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--script":
            i += 1
            script = args[i] if i < len(args) else None
        elif a == "--input":
            i += 1
            inputs.append(args[i] if i < len(args) else "")
        elif a == "--compact":
            compact = True
        elif a.startswith("--"):
            print(f"unknown option {a}", file=sys.stderr)
            return 2
        elif entry is None:
            entry = a
        else:
            print("decl repl takes one entry file", file=sys.stderr)
            return 2
        i += 1
    if script is None and entry is None and inputs:
        print("--input needs an entry file", file=sys.stderr)
        return 2
    for spec in inputs:
        if "=" not in spec:
            print(f"--input expects name=doc.json, got {spec}", file=sys.stderr)
            return 2

    def out(l: str) -> None:
        sys.stdout.write(l + "\n")

    repl = Repl(out, entry)
    repl.compact = compact
    for spec in inputs:
        repl.input(f":bind {spec}")

    if script is not None:
        try:
            if script == "-":
                text = sys.stdin.read()
            else:
                with open(script, encoding="utf-8") as f:
                    text = f.read()
        except OSError:
            print(f"cannot read {script}", file=sys.stderr)
            return 2
        for l in re.sub(r"\n$", "", text, count=1).split("\n"):
            out(f"{'. ' if repl.pending() else '> '}{l}")
            repl.line(l)
            if repl.quit_requested:
                break
        if repl.pending():
            repl.line("")
        sys.stdout.flush()
        return 1 if repl.errors else 0

    # interactive: the line editor, with history and completion
    try:
        import readline

        def completer(text: str, state: int) -> Any:
            # readline replaces the word from the last delimiter (`site.la`) -> Any:
            # the candidates keep the token's head, the session completes its tail
            line = readline.get_line_buffer()
            cs = [c.split("  ")[0] for c in repl.session.complete(line, COMMAND_NAMES)]
            m = re.search(r"([A-Za-z_$:][A-Za-z0-9_$.\[\]\"]*)$", line)
            tok = m.group(1) if m else ""
            head, tail = (
                (tok[: tok.rfind(".") + 1], tok[tok.rfind(".") + 1 :]) if "." in tok else ("", tok)
            )
            matches = [head + c for c in cs if c.startswith(tail)]
            return matches[state] if state < len(matches) else None

        readline.set_completer(completer)
        readline.set_completer_delims(" \t\n")
        # macOS ships libedit behind the readline module: its binding syntax differs
        backend = getattr(readline, "backend", None)
        libedit = backend == "editline" if backend else "libedit" in (readline.__doc__ or "")
        readline.parse_and_bind("bind ^I rl_complete" if libedit else "tab: complete")
        # the history is kept across sessions
        history = os.path.join(os.path.expanduser("~"), ".decl_history")
        with contextlib.suppress(OSError):
            readline.read_history_file(history)
        readline.set_history_length(1000)
        import atexit

        atexit.register(lambda: _save_history(readline, history))
    except ImportError:
        pass
    while True:
        try:
            l = input(". " if repl.pending() else "> ")
        except EOFError:
            sys.stdout.write("\n")
            break
        except KeyboardInterrupt:
            sys.stdout.write("\n")
            repl._buffer = []
            continue
        repl.line(l)
        if repl.quit_requested:
            break
    return 0
