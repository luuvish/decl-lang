"""Parity across the three implementations.

Decl ships a TypeScript reference implementation (decl-typescript), a Rust
runtime (decl-rust), and a Python runtime (decl-python). They must be
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
    DECL_PYTHON=decl-python/.venv/bin/python ...             # the interpreter that has `decl` installed

Prerequisites: `npm ci` at the repository root, `cargo build --release`
(the Cargo workspace), and the Python package importable (`make python-env`). A missing
runtime is a failure, not a skip — `make verify` is the gate.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TS_CLI = ROOT / "decl-typescript/src/cli.ts"
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
    """(exit code, stdout, stderr) of one command line, temp paths normalized"""
    r = subprocess.run(cmd + args, capture_output=True, text=True, check=False, cwd=str(ROOT))
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

print(f"== evaluate: {len(files)} modules (exit, stdout, stderr — with and without --json)")
for p in files:
    rel = str(p.relative_to(ROOT))
    cli_row(f"{rel} (--json)", ["evaluate", rel, "--json"])
    cli_row(rel, ["evaluate", rel])

# ---------------------------------------------------------------- documents bound to input roots
cases: list[tuple[str, Path, str, Path]] = []
cfg = ROOT / "docs/examples/02_config.decl"
bad = tmp / "deployed.json"
bad.write_text('{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}', encoding="utf-8")
cases.append(("config: invalid deployment", cfg, "deployed", bad))
ic = ROOT / "docs/examples/01_interconnect.decl"
ser = outcome(REF, ["evaluate", str(ic.relative_to(ROOT)), "--root", "xbar"])[1].strip()
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
    cli_row(f"{label} (evaluate --root {name} --json)", ["evaluate", rel, "--input", f"{name}={doc}", "--root", name, "--json"])
    cli_row(f"{label} (evaluate --root {name})", ["evaluate", rel, "--input", f"{name}={doc}", "--root", name])
cli_row("evaluate: --root names nothing", ["evaluate", str(cfg.relative_to(ROOT)), "--root", "nope", "--json"])
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
            if m.get("id") == self.next_id:
                return m.get("result")

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
    (d / "lib.decl").write_text("export type Service = { name: string, port: 1..65535 = 8080 }\nexport const MAX = 16\n")
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
    s.notify("textDocument/didChange", {"textDocument": {"uri": uri, "version": 9}, "contentChanges": [{"text": 'import { Service, MAX as LIMIT } from "./lib.decl"\nconst top = LIMIT\nexport output s: Service = { name: "a" }\n'}]})
    s.diagnostics(uri)
    for label, pos in [("hover top", (1, 7)), ("hover LIMIT", (1, 13)), ("hover Service", (2, 19)), ("hover nothing", (0, 0))]:
        out.append((label, s.request("textDocument/hover", {"textDocument": {"uri": uri}, "position": {"line": pos[0], "character": pos[1]}})))
    for label, pos in [("definition Service", (2, 19)), ("definition LIMIT", (1, 13)), ("definition top", (1, 7))]:
        r = s.request("textDocument/definition", {"textDocument": {"uri": uri}, "position": {"line": pos[0], "character": pos[1]}})
        if r and "uri" in r:
            r = dict(r, uri=r["uri"].replace(str(d), "<dir>").replace("%2F", "/"))   # servers encode file URIs differently
        out.append((label, r))
    out.append(("shutdown", s.request("shutdown", {})))
    s.notify("exit", {})
    s.close()
    return out


print("== lsp: one scripted editor session (diagnostics, hover, definition)")
ref_t = lsp_transcript(["node", str(ROOT / "decl-typescript/src/lsp.ts")])
nat_t = {n: lsp_transcript(cmd) for n, cmd in LSP_SERVERS.items()}
for i, (label, ref_v) in enumerate(ref_t):
    verdicts, detail = {}, {}
    for n in names:
        nat_v = nat_t[n][i][1] if i < len(nat_t[n]) else None
        verdicts[n] = canonical(ref_v) == canonical(nat_v)
        detail[n] = f"ref={canonical(ref_v)[:160]} | {n}={canonical(nat_v)[:160]}"
    row(f"lsp: {label}", verdicts, detail)

print(f"\n{same} identical, {diff} different (reference vs {', '.join(names)})")
sys.exit(1 if diff else 0)
