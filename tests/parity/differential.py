"""Parity across the three implementations.

Decl ships a TypeScript reference implementation (decl-ts), a Rust
runtime (decl-rs), and a Python runtime (decl-py). They must be
indistinguishable at the command line: for every module in the fixture
corpus (valid and invalid), the documentation examples, and the domain
examples, `check` and `evaluate` — with and without `--json` — must produce
the same exit code, the same standard output (the diagnostic report, the
serialized values), and the same standard error, byte for byte; binding
documents to input roots (`validate --input`, `evaluate --input`) likewise;
`fmt` must produce the same bytes for every parseable module; packages
(manifests, the resolver, the lock) the same reports; and the language
servers must answer one scripted editor session identically. The reference
is the oracle; both natives are diffed against it, which makes the three
pairwise identical. The only normalization is of temporary directories the
harness itself creates.

    python tests/parity/differential.py                 # rust and python vs reference
    python tests/parity/differential.py --only rust     # one runtime
    DECL_PYTHON=decl-py/.venv/bin/python ...             # the interpreter that has `decl` installed

Prerequisites: `npm ci` at the repository root, `cargo build --release`
(the Cargo workspace), and the Python package importable (`make python-env`). A missing
runtime is a failure, not a skip — `make verify` is the gate.
"""
from __future__ import annotations

import json
import re
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TS_CLI = ROOT / "decl-ts/src/cli.ts"
RUST_BIN = ROOT / "target/release/decl"
PYTHON = os.environ.get("DECL_PYTHON") or sys.executable

only = None
if "--only" in sys.argv:
    only = sys.argv[sys.argv.index("--only") + 1]
RUNTIMES: dict[str, list[str]] = {}
LSP_SERVERS: dict[str, list[str]] = {}
if only in (None, "rust"):
    RUNTIMES["rust"] = [str(RUST_BIN)]
    LSP_SERVERS["rust"] = [str(ROOT / "target/release/decl-lsp")]
if only in (None, "python"):
    RUNTIMES["python"] = [PYTHON, "-m", "decl.runtime"]
    LSP_SERVERS["python"] = [PYTHON, "-m", "decl.runtime.lsp"]
if not RUNTIMES:
    sys.exit(f"unknown runtime {only!r} (rust | python)")

# ---------------------------------------------------------------- preflight
missing = []
if not TS_CLI.exists() or not (ROOT / "node_modules").exists():
    missing.append("node_modules (run `npm ci` at the repository root)")
if "rust" in RUNTIMES and not RUST_BIN.exists():
    missing.append("target/release/decl (run `cargo build --release`)")
if "python" in RUNTIMES:
    probe = subprocess.run([PYTHON, "-c", "import decl.runtime"], capture_output=True, text=True, cwd=str(ROOT))
    if probe.returncode:
        missing.append(f"python package not importable by {PYTHON} (run `make python-env`): {probe.stderr.strip().splitlines()[-1] if probe.stderr.strip() else ''}")
if missing:
    print("parity: prerequisites missing —\n  " + "\n  ".join(missing))
    sys.exit(2)

REF = ["node", str(TS_CLI)]
names = list(RUNTIMES)
width = max(len(n) for n in names)
same = diff = 0

# temporary directories this harness creates: the one thing normalized away
tmp = Path(tempfile.mkdtemp(prefix="decl-parity-"))
NORMALIZE: list[tuple[str, str]] = [(str(tmp), "<tmp>")]


def outcome(cmd: list[str], args: list[str]) -> tuple[int, str, str]:
    """(exit code, stdout, stderr) of one command line, temp paths normalized;
    a `--script -` reads the session from standard input"""
    stdin = "deployed\n:roots\n" if args[-2:] == ["--script", "-"] else ""
    r = subprocess.run(cmd + args, capture_output=True, text=True, check=False, cwd=str(ROOT), input=stdin)
    out, err = r.stdout, r.stderr
    for a, b in NORMALIZE:
        out, err = out.replace(a, b), err.replace(a, b)
    return (r.returncode, out, err)


def canonical(v) -> str:
    return json.dumps(v, separators=(",", ":"), ensure_ascii=False)


def describe(o: tuple[int, str, str]) -> str:
    return f"exit {o[0]} out={o[1][-160:]!r} err={o[2][-160:]!r}"


def row(label: str, verdicts: dict[str, bool], detail: dict[str, str]) -> None:
    global same, diff
    ok = all(verdicts.values())
    same += ok
    diff += not ok
    cells = "  ".join(f"{n:<{width}} {'same' if verdicts[n] else 'DIFF'}" for n in names)
    print(f"  {cells}  {label}")
    for n in names:
        if not verdicts[n]:
            print(f"      {n}: {detail[n]}")


def cli_row(label: str, args: list[str]) -> None:
    """one command line, byte-identical outcome required of every runtime"""
    ref = outcome(REF, args)
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = outcome(prefix, args)
        verdicts[n] = ref == nat
        if ref != nat:
            what = "exit code" if ref[0] != nat[0] else "stdout" if ref[1] != nat[1] else "stderr"
            detail[n] = f"{what} differs — ref {describe(ref)} | {n} {describe(nat)}"
    row(label, verdicts, detail)


# ---------------------------------------------------------------- check
check_files: list[Path] = sorted((ROOT / "tests/validation").rglob("*.decl")) + sorted((ROOT / "docs/examples").glob("*.decl"))
for d in sorted((ROOT / "examples").iterdir()):
    if d.is_dir():
        mods = sorted(d.glob("*.decl"))
        entry = d / "main.decl" if (d / "main.decl").exists() else (mods[0] if len(mods) == 1 else None)
        if entry:
            check_files.append(entry)

print(f"== check: {len(check_files)} modules, reference vs {', '.join(names)} (exit, stdout, stderr — with and without --json)")
for p in check_files:
    rel = str(p.relative_to(ROOT))
    cli_row(f"{rel} (--json)", ["check", rel, "--json"])
    cli_row(rel, ["check", rel])

# ---------------------------------------------------------------- evaluate
files: list[Path] = []
for d in ("tests/validation", "docs/examples", "examples"):
    for p in sorted((ROOT / d).rglob("*.decl")):
        if "output " in p.read_text(encoding="utf-8") and "/invalid/" not in str(p):
            files.append(p)

print(f"== evaluate: {len(files)} modules (exit, stdout, stderr — with and without --json; then each output by --output)")
for p in files:
    rel = str(p.relative_to(ROOT))
    cli_row(f"{rel} (--json)", ["evaluate", rel, "--json"])
    cli_row(rel, ["evaluate", rel])
    # every output the module declares, exported or not, as the one document on stdout
    for name in re.findall(r"^(?:export\s+)?output\s+([A-Za-z_][A-Za-z0-9_]*)", p.read_text(encoding="utf-8"), re.M):
        cli_row(f"{rel} (--output {name})", ["evaluate", rel, "--output", name])


def file_row(label: str, args_of, out_of) -> None:
    """one `--output name=file` command line: outcome and the written bytes identical"""
    ref_file = out_of("ref")
    ref = outcome(REF, args_of(ref_file)) + (ref_file.read_text(encoding="utf-8") if ref_file.exists() else None,)
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        f = out_of(n)
        nat = outcome(prefix, args_of(f)) + (f.read_text(encoding="utf-8") if f.exists() else None,)
        verdicts[n] = ref == nat
        if ref != nat:
            what = "exit code" if ref[0] != nat[0] else "stdout" if ref[1] != nat[1] else "stderr" if ref[2] != nat[2] else "written file"
            detail[n] = f"{what} differs — ref {describe(ref[:3])} | {n} {describe(nat[:3])}"
    row(label, verdicts, detail)


ic0 = ROOT / "docs/examples/02_config.decl"
file_row("evaluate --output name=file (two roots to two files, one to stdout)",
         lambda f: ["evaluate", str(ic0.relative_to(ROOT)), "--output", f"prod={f}", "--output", f"dev={f}.dev", "--output", "base"],
         lambda n: tmp / f"cfg-{n}.json")

# ---------------------------------------------------------------- documents bound to input roots
cases: list[tuple[str, Path, str, Path]] = []
cfg = ROOT / "docs/examples/02_config.decl"
bad = tmp / "deployed.json"
bad.write_text('{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}', encoding="utf-8")
cases.append(("config: invalid deployment", cfg, "deployed", bad))
ic = ROOT / "docs/examples/01_interconnect.decl"
ser = outcome(REF, ["evaluate", str(ic.relative_to(ROOT)), "--output", "xbar"])[1].strip()
good = tmp / "xbar.json"
good.write_text(ser, encoding="utf-8")
cases.append(("interconnect: round trip", ic, "doc", good))
probe = '"mi1":{"kind":"ext","mode":"mi","width":64}'
assert probe in ser, "corruption probe no longer matches the serialized form"
corrupt = tmp / "xbar_corrupt.json"
corrupt.write_text(ser.replace(probe, probe.replace("64", "32"), 1), encoding="utf-8")
cases.append(("interconnect: corrupted width", ic, "doc", corrupt))
malformed = tmp / "malformed.json"
malformed.write_text('{"host": ', encoding="utf-8")
cases.append(("config: malformed document", cfg, "deployed", malformed))
trailing = tmp / "trailing.json"
trailing.write_text('{"host": "a"} x', encoding="utf-8")
cases.append(("config: document with trailing characters", cfg, "deployed", trailing))
cases.append(("config: unreadable document", cfg, "deployed", tmp / "missing.json"))

print(f"== validate --input / evaluate --input: {len(cases)} documents (exit, stdout, stderr)")
for label, decl, name, doc in cases:
    rel = str(decl.relative_to(ROOT))
    cli_row(f"{label} (validate --json)", ["validate", rel, "--input", f"{name}={doc}", "--json"])
    cli_row(f"{label} (validate)", ["validate", rel, "--input", f"{name}={doc}"])
    cli_row(f"{label} (evaluate --output {name} --json)", ["evaluate", rel, "--input", f"{name}={doc}", "--output", name, "--json"])
    cli_row(f"{label} (evaluate --output {name})", ["evaluate", rel, "--input", f"{name}={doc}", "--output", name])
cli_row("evaluate: --output names nothing", ["evaluate", str(cfg.relative_to(ROOT)), "--output", "nope", "--json"])
cli_row("evaluate: --output without a name", ["evaluate", str(cfg.relative_to(ROOT)), "--output", "=x.json"])
cli_row("evaluate: two --output to stdout", ["evaluate", str(cfg.relative_to(ROOT)), "--output", "prod", "--output", "dev"])
cli_row("evaluate: a module exporting no output", ["evaluate", "tests/validation/declarations/valid/output_from_input_fallback.decl"])
cli_row("evaluate: --output of an unwritable file", ["evaluate", str(cfg.relative_to(ROOT)), "--output", f"prod={tmp}/no/such/dir/x.json"])
cli_row("--version", ["--version"])
cli_row("validate: a module with imports, nothing bound", ["validate", "tests/modules/basic/main.decl"])
cli_row("validate: a module with imports (--json)", ["validate", "tests/modules/basic/main.decl", "--json"])
cli_row("validate: a file that does not parse", ["validate", "tests/validation/lexical/invalid/semicolon.decl"])
cli_row("validate: a missing file", ["validate", f"{tmp}/missing.decl"])
cli_row("fmt: a missing file", ["fmt", f"{tmp}/missing.decl"])
cli_row("fmt --check: a missing file", ["fmt", "--check", f"{tmp}/missing.decl"])
cli_row("evaluate: --input without name=", ["evaluate", str(cfg.relative_to(ROOT)), "--input", "nope"])
cli_row("evaluate: --input of an unknown input", ["evaluate", str(cfg.relative_to(ROOT)), "--input", f"nope={bad}"])

# ---------------------------------------------------------------- fmt
fmt_files: list[Path] = []
for d in ("tests/validation", "tests/modules", "tests/packages", "docs/examples", "examples"):
    fmt_files += sorted((ROOT / d).rglob("*.decl"))
fmt_tmp = tmp / "fmt"
fmt_tmp.mkdir()


def fmt_with(cmd: list[str], src: str) -> tuple:
    p = fmt_tmp / "x.decl"
    p.write_text(src, encoding="utf-8")
    r = subprocess.run(cmd + ["fmt", str(p)], capture_output=True, text=True, check=False, cwd=str(ROOT))
    return (r.returncode, p.read_text(encoding="utf-8"), r.stderr.replace(str(tmp), "<tmp>"))


print(f"== fmt: {len(fmt_files)} modules, byte-identical output (and exit, stderr)")
for p in fmt_files:
    src = p.read_text(encoding="utf-8")
    ref = fmt_with(REF, src)
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = fmt_with(prefix, src)
        verdicts[n] = ref == nat
        detail[n] = f"ref exit {ref[0]} err={ref[2]!r} | {n} exit {nat[0]} err={nat[2]!r}" + ("" if ref[1] == nat[1] else " (text differs)")
    row(str(p.relative_to(ROOT)), verdicts, detail)

# ---------------------------------------------------------------- packages
print("== packages: manifests, resolver, lock (check + evaluate, exit, stdout, stderr)")
for entry in sorted((ROOT / "tests/packages").glob("*/main.decl")):
    rel = str(entry.relative_to(ROOT))
    for cmd_name in ("check", "evaluate"):
        cli_row(f"{rel} ({cmd_name} --json)", [cmd_name, rel, "--json"])
        cli_row(f"{rel} ({cmd_name})", [cmd_name, rel])
# ---------------------------------------------------------------- lsp
import re as _re


class LspSession:
    """one scripted editor session; every message the server sends back is recorded"""

    def __init__(self, cmd: list[str]):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, cwd=str(ROOT))
        self.next_id = 0
        self.log: list = []

    def _send(self, msg: dict) -> None:
        body = json.dumps(msg).encode("utf-8")
        self.p.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        self.p.stdin.flush()

    def _recv(self) -> dict:
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            ch = self.p.stdout.read(1)
            if not ch:
                raise RuntimeError("server closed")
            header += ch
        n = int(_re.search(rb"Content-Length: (\d+)", header).group(1))
        return json.loads(self.p.stdout.read(n).decode("utf-8"))

    def request(self, method: str, params: dict):
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        while True:
            m = self._recv()
            self.log.append(m)
            # a response carries no method; a server's own request (window/workDoneProgress/create) does
            if "method" not in m and m.get("id") == self.next_id:
                # a server error is an answer too (and a parity difference if only one server gives it)
                return m["result"] if "result" in m else {"error": m.get("error")}

    def notify(self, method: str, params: dict) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def diagnostics(self, uri: str) -> dict:
        while True:
            m = self._recv()
            self.log.append(m)
            if m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == uri:
                return m["params"]

    def close(self) -> None:
        self.p.stdin.close()
        self.p.wait(timeout=10)


def lsp_transcript(cmd: list[str]) -> list:
    d = Path(tempfile.mkdtemp(prefix="decl-parity-lsp-"))
    (d / "lib.decl").write_text("export type Service = { name: string, port: 1..65535 = 8080 }\nexport const MAX = 16\nexport func cap(n: int): int = std.math.min(n, MAX)\nexport type Public = Service { public: bool }\nexport type Level = \"low\" | \"high\"\n")
    main = d / "main.decl"
    main.write_text("")
    uri = main.as_uri()
    s = LspSession(cmd)
    out = []
    init = s.request("initialize", {"processId": None, "rootUri": None, "capabilities": {}})
    out.append(("initialize", init))
    s.notify("initialized", {})
    for i, text in enumerate(["const x = \n", "type Bad = 10..3\n", 'import { Service, MAX as LIMIT } from "./lib.decl"\nconst top = LIMIT\nexport output s: Service = { name: "a" }\n', "const dup = 1\nconst dup = 2\n"], 1):
        if i == 1:
            s.notify("textDocument/didOpen", {"textDocument": {"uri": uri, "languageId": "decl", "version": 1, "text": text}})
        else:
            s.notify("textDocument/didChange", {"textDocument": {"uri": uri, "version": i}, "contentChanges": [{"text": text}]})
        ds = s.diagnostics(uri)["diagnostics"]
        for x in ds:
            x["message"] = x["message"].replace(str(d), "<dir>")
        out.append((f"diagnostics {i}", ds))
    MAIN = 'import { Service, MAX as LIMIT, cap, Level } from "./lib.decl"\nconst top = LIMIT\nexport output s: Service = { name: "a" }\nexport output t: Service = {\n    name: "b"\n}\nconst first = s.name\nconst c = cap(top)\nconst d = 250ms\ntype Local = Service { extra = name, level?: Level = "low" }\nconst m2 = std.math.min(top, 3)\n'

    def norm(v):
        """temp paths and URI encodings normalized (servers encode file URIs differently)"""
        if isinstance(v, str):
            return v.replace(str(d), "<dir>").replace("%2F", "/")
        if isinstance(v, list):
            return [norm(x) for x in v]
        if isinstance(v, dict):
            return {norm(k): norm(x) for k, x in v.items()}
        return v

    def change(version: int, text: str):
        s.notify("textDocument/didChange", {"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]})
        return norm(s.diagnostics(uri)["diagnostics"])

    def ask(label: str, method: str, params: dict):
        out.append((label, norm(s.request(method, params))))

    at = lambda line, ch: {"textDocument": {"uri": uri}, "position": {"line": line, "character": ch}}
    change(9, MAIN)
    for label, pos in [("hover top", (1, 7)), ("hover LIMIT", (1, 13)), ("hover Service", (2, 19)), ("hover nothing", (0, 0)), ("hover s.name", (6, 16)), ("hover a literal", (2, 36))]:
        ask(label, "textDocument/hover", at(*pos))
    for label, pos in [("definition Service", (2, 19)), ("definition LIMIT", (1, 13)), ("definition top", (1, 7)), ("definition s.name", (6, 16))]:
        ask(label, "textDocument/definition", at(*pos))
    ask("type definition of s", "textDocument/typeDefinition", at(6, 14))
    ask("type definition on the output declaration s", "textDocument/typeDefinition", at(2, 14))
    ask("type definition on the constant top", "textDocument/typeDefinition", at(1, 7))
    ask("type definition on a member typed by a literal-union alias", "textDocument/typeDefinition", at(9, 37))
    ask("type definition on a member of a primitive type", "textDocument/typeDefinition", at(6, 16))
    ask("references of Service", "textDocument/references", dict(at(2, 19), context={"includeDeclaration": True}))
    ask("references of s (uses only)", "textDocument/references", dict(at(6, 14), context={"includeDeclaration": False}))
    ask("highlight of s", "textDocument/documentHighlight", at(6, 14))
    ask("completion of a name prefix", "textDocument/completion", at(1, 15))
    out.append(("diagnostics while typing", change(10, MAIN + "const e = s.\nconst f = std.arr\n")))
    ask("member completion while typing", "textDocument/completion", at(11, 12))
    ask("std completion", "textDocument/completion", at(12, 17))
    change(11, MAIN)
    ask("signature help in cap(top)", "textDocument/signatureHelp", at(7, 15))
    ask("signature help in a std call, second argument", "textDocument/signatureHelp", at(10, 27))
    ask("signature help outside a call", "textDocument/signatureHelp", at(1, 7))
    ask("workspace symbols", "workspace/symbol", {"query": "c"})
    ask("selection ranges", "textDocument/selectionRange", {"textDocument": {"uri": uri}, "positions": [{"line": 6, "character": 16}, {"line": 4, "character": 11}]})
    ask("semantic tokens", "textDocument/semanticTokens/full", {"textDocument": {"uri": uri}})
    ask("inlay hints", "textDocument/inlayHint", {"textDocument": {"uri": uri}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 20, "character": 0}}})
    ch = norm(s.request("textDocument/prepareCallHierarchy", at(7, 11)))
    out.append(("call hierarchy: prepare cap", ch))
    if ch:
        item = s.request("textDocument/prepareCallHierarchy", at(7, 11))[0]
        ask("call hierarchy: incoming calls of cap", "callHierarchy/incomingCalls", {"item": item})
        ask("call hierarchy: outgoing calls of cap", "callHierarchy/outgoingCalls", {"item": item})
    th = norm(s.request("textDocument/prepareTypeHierarchy", at(2, 19)))
    out.append(("type hierarchy: prepare Service", th))
    if th:
        item = s.request("textDocument/prepareTypeHierarchy", at(2, 19))[0]
        ask("type hierarchy: supertypes of Service", "typeHierarchy/supertypes", {"item": item})
        ask("type hierarchy: subtypes of Service", "typeHierarchy/subtypes", {"item": item})
    ask("code action: annotate a derived member", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": {"line": 9, "character": 24}, "end": {"line": 9, "character": 24}}, "context": {"diagnostics": []}})
    ds = change(20, "const z = cap(1)\n")
    out.append(("diagnostics of an unknown name", ds))
    ask("code action: import the unknown name", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": {"line": 0, "character": 10}, "end": {"line": 0, "character": 13}}, "context": {"diagnostics": ds}})
    ds = change(21, MAIN + "export output u: Service = { }\n")
    out.append(("diagnostics of a missing member", ds))
    ask("code action: add the missing member", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": {"line": 11, "character": 27}, "end": {"line": 11, "character": 30}}, "context": {"diagnostics": ds}})
    change(22, MAIN)
    ask("decl.showSyntaxTree", "workspace/executeCommand", {"command": "decl.showSyntaxTree", "arguments": [uri]})
    # linked editing, local rename, value hints, the remaining quick fixes and the assists
    ACTIONS = ('import { Service, MAX as LIMIT } from "./lib.decl"\nimport * as lib from "./lib.decl"\n'
               'type Pair = {\n    a: int,\n    r: string,\n    b?: int,\n    s?: Service,\n    c = a + 1,\n    d$ = a * 2,\n    e?: int = 3\n}\n'
               'const xs = [x * 2 for x in [1, 2] if x > 0]\nconst f = (y) => y + 1\n'
               'export output p: Pair = { a: 1, r: "x", zz: true }\nconst cmp = LIMIT < 3\nconst k = cap(1)\n'
               'const q1 = p.s.name\nconst q2 = p.b + 1\nconst mixed = true && null ?? false\n')
    def pos(needle: str, nth: int = 0, offset: int = 0):
        i = -1
        for _ in range(nth + 1):
            i = ACTIONS.index(needle, i + 1)
        line = ACTIONS.count("\n", 0, i)
        col = i - (ACTIONS.rfind("\n", 0, i) + 1) + offset
        return {"line": line, "character": col}
    def span(needle: str, nth: int = 0):
        a = pos(needle, nth); b = dict(a); b["character"] += len(needle)
        return {"start": a, "end": b}
    ads = change(50, ACTIONS)
    out.append(("diagnostics of the actions text", ads))
    ask("linked editing of a comprehension variable", "textDocument/linkedEditingRange", {"textDocument": {"uri": uri}, "position": pos("x * 2")})
    ask("linked editing of a lambda parameter", "textDocument/linkedEditingRange", {"textDocument": {"uri": uri}, "position": pos("y + 1")})
    ask("prepare rename of a local", "textDocument/prepareRename", {"textDocument": {"uri": uri}, "position": pos("x * 2")})
    ask("rename of a local", "textDocument/rename", {"textDocument": {"uri": uri}, "position": pos("x * 2"), "newName": "n"})
    change(51, MAIN)
    s.notify("workspace/didChangeConfiguration", {"settings": {"decl": {"inlayHints": {"values": True}}}})
    s.diagnostics(uri)
    ask("inlay hints with values", "textDocument/inlayHint", {"textDocument": {"uri": uri}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 30, "character": 0}}})
    s.notify("workspace/didChangeConfiguration", {"settings": {"decl": {"inlayHints": {"values": False}}}})
    s.diagnostics(uri)
    ads = change(52, ACTIONS)
    ctx = {"diagnostics": ads}
    for label, needle, nth in [("derived member c", "c = a + 1", 0), ("hidden member d$", "d$ = a * 2", 0), ("optional member b", "b?: int", 0), ("required member a", "a: int", 0), ("defaulted member e", "e?: int = 3", 0), ("the type Pair", "type Pair", 0), ("the comparison", "LIMIT < 3", 0), ("the unknown name cap", "cap(1)", 0), ("the undeclared member zz", "zz: true", 0), ("the maybe-absent access", "p.s.name", 0), ("the maybe-absent operand", "p.b + 1", 0), ("the mixed ?? expression", "true && null", 0)]:
        ask(f"code actions at {label}", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": pos(needle, nth), "end": pos(needle, nth)}, "context": ctx})
    ask("code actions on a selected constant expression", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": span("[1, 2]"), "context": {"diagnostics": []}})
    ask("code actions on a selected member expression", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": span("a + 1"), "context": {"diagnostics": []}})
    ask("code actions in the literal (fill)", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": pos("a: 1"), "end": pos("a: 1")}, "context": ctx})
    # the remaining quick fixes (E4094, E4030/E4032, E4013, E4005), the inline / extract-type / unit / reorder assists, the context-variable hints
    ACTIONS2 = ('type Parent = { name: string, port: 1..65535, x?: int }\n'
                'type Child = Parent { port: int, x = 1 }\n'
                'type Item = { label = $parent.name }\n'
                'type Bad = { $parent: Owner, t = $parent.name }\n'
                'type Owner = { name: string, items: Item[], bad: Bad }\n'
                'type A = { a: int }\ntype B = { b: int }\ntype U = A | B\n'
                'const K = 2\nconst twice = K + K\n'
                'type W = { inner: { q: int } }\n'
                'const dur = 250ms\n'
                'type R = {\n    d = 1,\n    a: int,\n    b?: int\n}\n'
                'type Ctx = { $parent: ref<Owner2>, tag = $parent.name }\ntype Owner2 = { name: string, c: Ctx }\n')
    ACTIONS = ACTIONS2
    ads = change(60, ACTIONS2)
    out.append(("diagnostics of the second actions text", ads))
    ctx = {"diagnostics": ads}
    for label, needle in [("the widened override", "port: int"), ("the kind transition", "x = 1"), ("the undeclared $parent", "$parent.name"), ("the non-ref $parent declaration", "$parent: Owner"), ("the union U", "type U"), ("the constant K", "const K"), ("the inline record type", "inner: {"), ("the unit literal", "250ms"), ("the type R", "type R")]:
        ask(f"code actions at {label}", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": pos(needle), "end": pos(needle)}, "context": ctx})
    s.notify("workspace/didChangeConfiguration", {"settings": {"decl": {"inlayHints": {"contextVariables": True}}}})
    s.diagnostics(uri)
    ask("inlay hints with context variables", "textDocument/inlayHint", {"textDocument": {"uri": uri}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 30, "character": 0}}})
    s.notify("workspace/didChangeConfiguration", {"settings": {"decl": {"inlayHints": {"contextVariables": False}}}})
    s.diagnostics(uri)
    ACTIONS = 'type T = { n: int, m: int = n + 1 }\nexport output o: T = { n: 1, m: 5 }\n'
    ads = change(61, ACTIONS)
    out.append(("diagnostics of a restated derived member", ads))
    ask("code actions at the restated member", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": pos("m: 5"), "end": pos("m: 5")}, "context": {"diagnostics": ads}})
    # the conversions (if-chain <-> match, else error <-> diagnostic), inlining a derived member, on-type formatting
    ACTIONS = ('type Circle = { kind: "circle", r: int }\ntype Rect = { kind: "rect", w: int, h: int }\ntype Shape = Circle | Rect\ninput shape: Shape\n'
               'const area = if shape.kind == "circle" then shape.r * 2 else if shape.kind == "rect" then shape.w * shape.h else 0\n'
               'const area2 = match shape {\n    (c: Circle) => c.r * 2\n    (r: Rect) => r.w * r.h\n}\n'
               'diagnostic wide(w: int) {\n    severity = error\n    message = `too wide: ${w}`\n}\n'
               'type Box = {\n    w: int,\n    h: int,\n    area = w * h,\n    big = area > 10,\n    assert fits: w <= 100 else error `too wide: ${w}`,\n    assert fits2: h <= 100 else wide(h)\n}\n')
    ads = change(70, ACTIONS)
    out.append(("diagnostics of the conversions text", ads))
    for label, needle in [("the if chain", "if shape.kind"), ("the match", "match shape"), ("the derived member area", "area = w * h"), ("the inline else error", "assert fits:"), ("the diagnostic reference", "assert fits2")]:
        ask(f"code actions at {label}", "textDocument/codeAction", {"textDocument": {"uri": uri}, "range": {"start": pos(needle), "end": pos(needle)}, "context": {"diagnostics": []}})
    change(71, "type T = {\nx: int,\n    }\nconst v = 1 +\n2\n")
    ask("on-type formatting: a new line after an opening brace", "textDocument/onTypeFormatting", {"textDocument": {"uri": uri}, "position": {"line": 1, "character": 0}, "ch": "\n", "options": {"tabSize": 4, "insertSpaces": True}})
    ask("on-type formatting: a closing brace", "textDocument/onTypeFormatting", {"textDocument": {"uri": uri}, "position": {"line": 2, "character": 5}, "ch": "}", "options": {"tabSize": 4, "insertSpaces": True}})
    ask("on-type formatting: a continuation line", "textDocument/onTypeFormatting", {"textDocument": {"uri": uri}, "position": {"line": 4, "character": 0}, "ch": "\n", "options": {"tabSize": 4, "insertSpaces": True}})
    change(72, MAIN)
    ask("document symbols", "textDocument/documentSymbol", {"textDocument": {"uri": uri}})
    ask("folding ranges", "textDocument/foldingRange", {"textDocument": {"uri": uri}})
    out.append(("diagnostics of an unformatted text", change(12, "const x=1\nconst y = [s for s in [1, 2]]\n")))
    ask("formatting", "textDocument/formatting", {"textDocument": {"uri": uri}, "options": {"tabSize": 4, "insertSpaces": True}})
    change(13, MAIN)
    ask("prepare rename top", "textDocument/prepareRename", at(1, 7))
    ask("prepare rename a literal", "textDocument/prepareRename", at(2, 36))
    ask("rename Service", "textDocument/rename", dict(at(2, 19), newName="Svc"))
    ask("rename top", "textDocument/rename", dict(at(1, 7), newName="head"))
    ask("code lenses", "textDocument/codeLens", {"textDocument": {"uri": uri}})
    ask("decl.evaluate s", "workspace/executeCommand", {"command": "decl.evaluate", "arguments": [uri, "s"]})
    ask("decl.evaluate all", "workspace/executeCommand", {"command": "decl.evaluate", "arguments": [uri]})
    ask("decl.validate", "workspace/executeCommand", {"command": "decl.validate", "arguments": [uri]})
    ask("decl.trace", "workspace/executeCommand", {"command": "decl.trace", "arguments": [uri, "s.port"]})
    out.append(("diagnostics of an evaluation error", change(30, MAIN + 'export output u: Service = { name: "c", port: 70000 }\n')))
    out.append(("diagnostics of a missing import", change(31, 'import { Nothing } from "./lib.decl"\nconst z = 1\n')))
    change(32, MAIN)
    out.append(("shutdown", s.request("shutdown", {})))
    s.notify("exit", {})
    s.close()

    # a client that accepts work-done progress (03_lsp.md §14): the analysis of
    # an opened document is wrapped in a create request — its id an integer,
    # which every client accepts — and begin/end notifications before the
    # diagnostics; the client's response to the create request is not
    # answered (a stray response breaks Neovim's client)
    s2 = LspSession(cmd)
    s2.request("initialize", {"processId": None, "rootUri": None, "capabilities": {"window": {"workDoneProgress": True}}})
    s2.notify("initialized", {})
    mark = len(s2.log)
    s2.notify("textDocument/didOpen", {"textDocument": {"uri": uri, "languageId": "decl", "version": 1, "text": MAIN}})
    s2.diagnostics(uri)
    seen = [(m["method"], type(m["id"]).__name__ if "id" in m else None, ((m.get("params") or {}).get("value") or {}).get("kind")) for m in s2.log[mark:]]
    create_id = next((m["id"] for m in s2.log[mark:] if m.get("method") == "window/workDoneProgress/create"), None)
    out.append(("progress: the messages around the analysis of an opened document", {"seen": seen, "create id is an integer": isinstance(create_id, int)}))
    s2._send({"jsonrpc": "2.0", "id": create_id, "result": None})
    mark = len(s2.log)
    hover = s2.request("textDocument/hover", {"textDocument": {"uri": uri}, "position": {"line": 2, "character": 19}})
    out.append(("progress: after the client's response, hover is answered and nothing else arrives",
                {"hover": hover is not None and "error" not in (hover or {}), "in between": [m.get("method", "response") for m in s2.log[mark:-1]]}))
    s2.request("shutdown", {})
    s2.notify("exit", {})
    s2.close()
    return out


# ---------------------------------------------------------------- golden
# the evaluation of every example and module entry against the committed
# expected document (tests/golden/): every implementation — the reference
# included — must print exactly those bytes, so the expected outputs are
# reviewed data, not whatever the reference happens to print
golden_manifest = json.loads((ROOT / "tests/golden/manifest.json").read_text())
print(f"== golden: {len(golden_manifest)} evaluations against tests/golden (every implementation, the reference included)")
for g in golden_manifest:
    # a golden is the evaluation's stdout; a `rejected` document's golden is
    # validate's exit 1 and its stderr — the diagnostics, in canonical order
    rejected = g.get("rejected", False)
    args = ["validate" if rejected else "evaluate", g["module"]] + [x for spec in g.get("inputs", []) for x in ("--input", spec)] + ([] if "output" not in g else ["--output", g["output"]])
    expected = (ROOT / g["golden"]).read_text()
    want_exit, stream = (1, 2) if rejected else (0, 1)
    ref = outcome(REF, args)
    verdicts, detail = {}, {}
    ref_ok = ref[0] == want_exit and ref[stream] == expected
    for n, prefix in RUNTIMES.items():
        nat = outcome(prefix, args)
        verdicts[n] = ref_ok and nat[0] == want_exit and nat[stream] == expected
        if not verdicts[n]:
            detail[n] = ("the reference differs from the golden — " if not ref_ok else "") + f"ref {describe(ref)} | {n} {describe(nat)}"
    row(f"golden {g['golden']}", verdicts, detail)

# ---------------------------------------------------------------- repl
repl_cases = sorted(p for p in (ROOT / "tests/repl").iterdir() if (p / "session.txt").exists())
print(f"== repl: {len(repl_cases)} scripted sessions (the transcript, byte for byte, and the exit status)")
for case in repl_cases:
    rel = str(case.relative_to(ROOT))
    entry = [f"{rel}/main.decl"] if (case / "main.decl").exists() else []
    cli_row(f"repl {case.name}", ["repl", *entry, "--script", f"{rel}/session.txt"])
cli_row("repl: --input binds before the first line, --script - reads stdin", ["repl", "tests/repl/documents/main.decl", "--input", "deployed=tests/repl/documents/doc.json", "--script", "-"])
cli_row("repl: a missing script is a usage error", ["repl", "--script", f"{tmp}/nope.txt"])
cli_row("repl: an unknown option is a usage error", ["repl", "--nope"])

print("== lsp: one scripted editor session (diagnostics, hover, navigation, completion, symbols, formatting, rename, lenses, commands)")
ref_t = lsp_transcript(["node", str(ROOT / "decl-ts/src/lsp.ts")])
nat_t = {n: lsp_transcript(cmd) for n, cmd in LSP_SERVERS.items()}
# what the specification fixes beyond "the same as the reference": the reference must satisfy these too
LSP_EXPECT = {
    "progress: the messages around the analysis of an opened document":
        lambda v: v["create id is an integer"] and [x[0] for x in v["seen"]] == ["window/workDoneProgress/create", "$/progress", "$/progress", "textDocument/publishDiagnostics"] and [x[2] for x in v["seen"]] == [None, "begin", "end", None],
    "progress: after the client's response, hover is answered and nothing else arrives":
        lambda v: v["hover"] is True and v["in between"] == [],
}
for i, (label, ref_v) in enumerate(ref_t):
    verdicts, detail = {}, {}
    ref_ok = label not in LSP_EXPECT or LSP_EXPECT[label](ref_v)
    for n in names:
        nat_v = nat_t[n][i][1] if i < len(nat_t[n]) else None
        verdicts[n] = ref_ok and canonical(ref_v) == canonical(nat_v)
        detail[n] = ("the reference violates 03_lsp.md §14 — " if not ref_ok else "") + f"ref={canonical(ref_v)[:160]} | {n}={canonical(nat_v)[:160]}"
    row(f"lsp: {label}", verdicts, detail)

print(f"\n{same} identical, {diff} different (reference vs {', '.join(names)})")
sys.exit(1 if diff else 0)
