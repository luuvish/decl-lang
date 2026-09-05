"""Replay a language-server session of the corpus (tests/lsp/<case>/) over
one server and return its transcript: the list of [label, answer] pairs the
session records, normalized so a transcript is machine-independent. The
parity harness and decl-py's suite both import this; the TypeScript and
Rust suites carry the same driver in their own language (tests/lsp/README.md
fixes the session format).

    python tests/lsp/replay.py <case dir> -- <server command...>   # prints the transcript
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

# the fields the servers spell differently by nature: the version they report
VERSION = "<version>"


class Server:
    """one server over stdio; every message it sends is logged"""

    def __init__(self, cmd: list[str], cwd: str | None = None):
        self.p = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, cwd=cwd
        )
        self.next_id = 0
        self.log: list[dict[str, Any]] = []

    def send(self, msg: dict[str, Any]) -> None:
        assert self.p.stdin is not None
        body = json.dumps(msg).encode("utf-8")
        self.p.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        self.p.stdin.flush()

    def recv(self) -> dict[str, Any]:
        assert self.p.stdout is not None
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            ch = self.p.stdout.read(1)
            if not ch:
                raise RuntimeError("the server closed its output")
            header += ch
        m = re.search(rb"Content-Length: (\d+)", header)
        assert m, header
        msg = json.loads(self.p.stdout.read(int(m.group(1))).decode("utf-8"))
        self.log.append(msg)
        return msg

    def request(self, method: str, params: Any) -> tuple[Any, list[str]]:
        """the answer (a result, or {"error": …}) and the methods of what arrived before it"""
        self.next_id += 1
        my = self.next_id
        self.send({"jsonrpc": "2.0", "id": my, "method": method, "params": params})
        between: list[str] = []
        while True:
            m = self.recv()
            # a response carries no method; a server's own request (window/workDoneProgress/create) does
            if "method" not in m and m.get("id") == my:
                return (m["result"] if "result" in m else {"error": m.get("error")}), between
            between.append(m.get("method", "response"))

    def notify(self, method: str, params: Any) -> None:
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def diagnostics(self, uri: str) -> tuple[list[Any], list[dict[str, Any]]]:
        """the next publishDiagnostics for the document, and every message seen until it"""
        seen: list[dict[str, Any]] = []
        while True:
            m = self.recv()
            seen.append(m)
            if m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == uri:
                return m["params"]["diagnostics"], seen

    def pending_request(self, method: str) -> Any:
        """the id of the server's own request of that method, the latest one"""
        for m in reversed(self.log):
            if m.get("method") == method and "id" in m:
                return m["id"]
        return None

    def close(self) -> None:
        assert self.p.stdin is not None
        self.p.stdin.close()
        self.p.wait(timeout=10)


def _find(text: str, needle: str, nth: int, offset: int) -> dict[str, int]:
    i = -1
    for _ in range(nth + 1):
        i = text.index(needle, i + 1)
    line = text.count("\n", 0, i)
    col = i - (text.rfind("\n", 0, i) + 1) + offset
    return {"line": line, "character": col}


class Session:
    def __init__(self, case_dir: Path, cmd: list[str]):
        self.case_dir = case_dir
        self.ws = (case_dir / "ws").resolve()
        self.server = Server(cmd)
        self.texts: dict[str, str] = {}
        self.versions: dict[str, int] = {}
        self.diags: dict[str, list[Any]] = {}
        self.answers: dict[str, Any] = {}
        self.open_files: list[str] = []
        self.transcript: list[list[Any]] = []

    def uri(self, file: str) -> str:
        return (self.ws / file).as_uri()

    def file_of(self, uri: str) -> str | None:
        for f in self.texts:
            if self.uri(f) == uri:
                return f
        return None

    # ---- placeholders (tests/lsp/README.md): {"$uri": file}, {"$pos"|"$at"|"$span": needle, nth, offset},
    # {"$diagnostics": file}, {"$answer": label, index}
    def resolve(self, v: Any, doc: str | None) -> Any:
        if isinstance(v, dict):
            key = next((k for k in v if k.startswith("$")), None)
            if key == "$uri":
                return self.uri(v["$uri"])
            if key in ("$pos", "$at", "$span"):
                assert doc is not None, "a position placeholder needs a textDocument"
                p = _find(self.texts[doc], v[key], v.get("nth", 0), v.get("offset", 0))
                if key == "$pos":
                    return p
                q = dict(p)
                if key == "$span":
                    q["character"] += len(v[key])
                return {"start": p, "end": q}
            if key == "$diagnostics":
                return self.diags.get(v["$diagnostics"], [])
            if key == "$answer":
                return self.answers[v["$answer"]][v.get("index", 0)]
            return {k: self.resolve(x, doc) for k, x in v.items()}
        if isinstance(v, list):
            return [self.resolve(x, doc) for x in v]
        return v

    def params_of(self, step: dict[str, Any]) -> Any:
        params = step.get("params", {})
        # the document the request addresses: its textDocument, resolved first
        doc = None
        td = params.get("textDocument") if isinstance(params, dict) else None
        if isinstance(td, dict) and "uri" in td:
            doc = self.file_of(self.resolve(td["uri"], None))
        return self.resolve(params, doc)

    def norm(self, v: Any) -> Any:
        """temp paths and URI encodings normalized; the server's version too"""
        if isinstance(v, str):
            return v.replace(str(self.ws), "<ws>").replace("%2F", "/")
        if isinstance(v, list):
            return [self.norm(x) for x in v]
        if isinstance(v, dict):
            out = {self.norm(k): self.norm(x) for k, x in v.items()}
            if "serverInfo" in out and isinstance(out["serverInfo"], dict) and "version" in out["serverInfo"]:
                out["serverInfo"] = dict(out["serverInfo"], version=VERSION)
            return out
        return v

    def record(self, label: str | None, value: Any) -> None:
        if label is not None:
            self.answers[label] = value
            self.transcript.append([label, self.norm(value)])

    @staticmethod
    def observed(seen: list[dict[str, Any]]) -> dict[str, Any]:
        rows = [
            [
                m.get("method", "response"),
                type(m["id"]).__name__ if "id" in m else None,
                ((m.get("params") or {}).get("value") or {}).get("kind") if isinstance(m.get("params"), dict) else None,
            ]
            for m in seen
        ]
        create = next((m["id"] for m in seen if m.get("method") == "window/workDoneProgress/create"), None)
        return {"seen": rows, "create id is an integer": isinstance(create, int)}

    def run(self) -> list[list[Any]]:
        steps = json.loads((self.case_dir / "session.json").read_text(encoding="utf-8"))["steps"]
        s = self.server
        for step in steps:
            label = step.get("label")
            if "open" in step or "change" in step:
                file = step.get("open") or step["change"]
                self.texts[file] = step["text"]
                if "open" in step:
                    self.versions[file] = 1
                    self.open_files.append(file)
                    s.notify(
                        "textDocument/didOpen",
                        {"textDocument": {"uri": self.uri(file), "languageId": "decl", "version": 1, "text": step["text"]}},
                    )
                else:
                    self.versions[file] += 1
                    s.notify(
                        "textDocument/didChange",
                        {"textDocument": {"uri": self.uri(file), "version": self.versions[file]}, "contentChanges": [{"text": step["text"]}]},
                    )
                diags, seen = s.diagnostics(self.uri(file))
                self.diags[file] = diags
                self.record(label, self.observed(seen) if step.get("observe") else diags)
            elif "request" in step:
                answer, between = s.request(step["request"], self.params_of(step))
                if step.get("between"):
                    self.record(label, {"answered": not (isinstance(answer, dict) and "error" in answer), "between": between})
                else:
                    self.record(label, answer)
            elif "notify" in step:
                s.notify(step["notify"], self.params_of(step))
            elif "config" in step:
                s.notify("workspace/didChangeConfiguration", {"settings": step["config"]})
                for file in self.open_files:
                    self.diags[file], _ = s.diagnostics(self.uri(file))
            elif "respond" in step:
                rid = s.pending_request(step["respond"])
                s.send({"jsonrpc": "2.0", "id": rid, "result": step.get("result")})
            else:
                raise ValueError(f"unknown step: {step}")
        s.close()
        return self.transcript


def replay(case_dir: Path, cmd: list[str]) -> list[list[Any]]:
    return Session(case_dir, cmd).run()


def cases(root: Path) -> list[Path]:
    return sorted(p for p in (root / "tests/lsp").iterdir() if (p / "session.json").exists())


if __name__ == "__main__":
    sep = sys.argv.index("--")
    print(json.dumps(replay(Path(sys.argv[1]), sys.argv[sep + 1 :]), indent=2, ensure_ascii=False))
