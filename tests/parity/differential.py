"""Parity across the three implementations.

Decl ships a TypeScript reference implementation (decl-typescript), a Rust
runtime (decl-rust), and a Python runtime (decl-python). They must be
indistinguishable: over every module in the fixture corpus (valid and
invalid), the documentation examples, and the domain examples, each
implementation's `check --json` must report the same static diagnostics
(codes and messages); over every module with outputs, `evaluate --json`
must carry the same `ok`, byte-identical canonical output, and the same
diagnostics; and binding documents to input roots (`validate --input`)
must yield the same root-cause diagnostics; `fmt` must produce the same
bytes for every parseable module; packages (manifests, the resolver, the
lock) must report the same diagnostics; and the language servers must
answer one scripted editor session identically. The reference is the
oracle; both natives are diffed against it, which makes the three
pairwise identical.

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


def run(cmd: list[str], cwd: Path | None = None) -> str:
    r = subprocess.run(cmd, capture_output=True, text=True, check=False, cwd=str(cwd) if cwd else None)
    return r.stdout.strip() or ("" if r.returncode == 0 else f"<<exit {r.returncode}: {r.stderr[-300:].strip()}>>")


def report(cmd_prefix: list[str], args: list[str]) -> dict:
    """{ok, value, diagnostics} of `evaluate --json`, or a marker when the run produced no report"""
    out = run(cmd_prefix + args, cwd=ROOT)
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return {"ok": None, "value": None, "diagnostics": [], "stderr": out[-300:]}


def diagnostics(cmd_prefix: list[str], args: list[str]) -> list:
    out = run(cmd_prefix + args, cwd=ROOT)
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return [{"code": "<no report>", "message": out[-300:]}]


def codes(diags: list) -> list:
    return sorted((d.get("code") or "", d.get("id") or "", d.get("path") or "") for d in diags)


def canonical(v) -> str:
    return json.dumps(v, separators=(",", ":"), ensure_ascii=False)


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


# ---------------------------------------------------------------- check
def check_key(diags: list) -> list:
    return sorted((d.get("code") or "", d.get("message") or "") for d in diags)


check_files: list[Path] = sorted((ROOT / "tests/validation").rglob("*.decl")) + sorted((ROOT / "docs/examples").glob("*.decl"))
for d in sorted((ROOT / "examples").iterdir()):
    if d.is_dir():
        mods = sorted(d.glob("*.decl"))
        entry = d / "main.decl" if (d / "main.decl").exists() else (mods[0] if len(mods) == 1 else None)
        if entry:
            check_files.append(entry)

print(f"== check: {len(check_files)} modules, reference vs {', '.join(names)} (codes and messages)")
for p in check_files:
    ref = diagnostics(REF, ["check", str(p), "--json"])
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = diagnostics(prefix, ["check", str(p), "--json"])
        verdicts[n] = check_key(ref) == check_key(nat)
        detail[n] = f"ref={check_key(ref)[:3]} | {n}={check_key(nat)[:3]}"
    row(str(p.relative_to(ROOT)), verdicts, detail)

# ---------------------------------------------------------------- evaluate
files: list[Path] = []
for d in ("tests/validation", "docs/examples", "examples"):
    for p in sorted((ROOT / d).rglob("*.decl")):
        if "output " in p.read_text(encoding="utf-8") and "/invalid/" not in str(p):
            files.append(p)

print(f"== evaluate: {len(files)} modules, reference vs {', '.join(names)}")
for p in files:
    ref = report(REF, ["evaluate", str(p), "--json"])
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = report(prefix, ["evaluate", str(p), "--json"])
        ok = ref.get("ok") == nat.get("ok") and codes(ref.get("diagnostics", [])) == codes(nat.get("diagnostics", []))
        if ok and ref.get("ok"):
            ok = canonical(ref["value"]) == canonical(nat["value"])
        verdicts[n] = ok
        detail[n] = (f"ref ok={ref.get('ok')} codes={codes(ref.get('diagnostics', []))[:3]} value={canonical(ref.get('value'))[:120]} | "
                     f"{n} ok={nat.get('ok')} codes={codes(nat.get('diagnostics', []))[:3]} value={canonical(nat.get('value'))[:120]} {nat.get('stderr', '')}")
    row(str(p.relative_to(ROOT)), verdicts, detail)

# ---------------------------------------------------------------- validate --input
tmp = Path(tempfile.mkdtemp(prefix="decl-parity-"))
cases: list[tuple[str, Path, str, Path]] = []
cfg = ROOT / "docs/examples/02_config.decl"
bad = tmp / "deployed.json"
bad.write_text('{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}', encoding="utf-8")
cases.append(("config: invalid deployment", cfg, "deployed", bad))
ic = ROOT / "docs/examples/01_interconnect.decl"
ser = run(REF + ["evaluate", str(ic), "--root", "xbar"])
good = tmp / "xbar.json"
good.write_text(ser, encoding="utf-8")
cases.append(("interconnect: round trip", ic, "doc", good))
probe = '"mi1":{"kind":"ext","mode":"mi","width":64}'
assert probe in ser, "corruption probe no longer matches the serialized form"
corrupt = tmp / "xbar_corrupt.json"
corrupt.write_text(ser.replace(probe, probe.replace("64", "32"), 1), encoding="utf-8")
cases.append(("interconnect: corrupted width", ic, "doc", corrupt))

print(f"== validate --input: {len(cases)} documents")
for label, decl, name, doc in cases:
    ref = diagnostics(REF, ["validate", str(decl), "--input", f"{name}={doc}", "--json"])
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = diagnostics(prefix, ["validate", str(decl), "--input", f"{name}={doc}", "--json"])
        verdicts[n] = codes(ref) == codes(nat)
        detail[n] = f"ref={codes(ref)} | {n}={codes(nat)}"
    row(f"{label} ({len(ref)} diagnostic(s))", verdicts, detail)

# ---------------------------------------------------------------- evaluate --input
# the same documents bound and emitted: the completed value of the bound
# root (--root names the input) must be byte-identical, and so must the
# verdict and the diagnostic codes when the document is invalid
print(f"== evaluate --input: {len(cases)} documents")
for label, decl, name, doc in cases:
    args = ["evaluate", str(decl), "--input", f"{name}={doc}", "--root", name, "--json"]
    ref = report(REF, args)
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = report(prefix, args)
        ok = ref.get("ok") == nat.get("ok") and codes(ref.get("diagnostics", [])) == codes(nat.get("diagnostics", []))
        if ok and ref.get("ok"):
            ok = canonical(ref["value"]) == canonical(nat["value"])
        verdicts[n] = ok
        detail[n] = (f"ref ok={ref.get('ok')} codes={codes(ref.get('diagnostics', []))[:3]} | "
                     f"{n} ok={nat.get('ok')} codes={codes(nat.get('diagnostics', []))[:3]} {nat.get('stderr', '')}")
    row(f"{label} (evaluate --input)", verdicts, detail)

# ---------------------------------------------------------------- fmt
fmt_files: list[Path] = []
for d in ("tests/validation", "tests/modules", "tests/packages", "docs/examples", "examples"):
    fmt_files += sorted((ROOT / d).rglob("*.decl"))
fmt_tmp = Path(tempfile.mkdtemp(prefix="decl-parity-fmt-"))


def fmt_with(cmd: list[str], src: str) -> tuple:
    p = fmt_tmp / "x.decl"
    p.write_text(src, encoding="utf-8")
    r = subprocess.run(cmd + ["fmt", str(p)], capture_output=True, text=True, check=False, cwd=str(ROOT))
    return (r.returncode, p.read_text(encoding="utf-8"))


print(f"== fmt: {len(fmt_files)} modules, byte-identical output")
for p in fmt_files:
    src = p.read_text(encoding="utf-8")
    ref = fmt_with(REF, src)
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = fmt_with(prefix, src)
        verdicts[n] = ref == nat
        detail[n] = f"ref exit {ref[0]} | {n} exit {nat[0]}" + ("" if ref[1] == nat[1] else " (text differs)")
    row(str(p.relative_to(ROOT)), verdicts, detail)

# ---------------------------------------------------------------- packages
print("== packages: manifests, resolver, lock (check + evaluate)")
for entry in sorted((ROOT / "tests/packages").glob("*/main.decl")):
    for cmd_name in ("check", "evaluate"):
        ref = report(REF, [cmd_name, str(entry), "--json"]) if cmd_name == "evaluate" else diagnostics(REF, [cmd_name, str(entry), "--json"])
        verdicts, detail = {}, {}
        for n, prefix in RUNTIMES.items():
            nat = report(prefix, [cmd_name, str(entry), "--json"]) if cmd_name == "evaluate" else diagnostics(prefix, [cmd_name, str(entry), "--json"])
            if cmd_name == "evaluate":
                ok = ref.get("ok") == nat.get("ok") and check_key(ref.get("diagnostics", [])) == check_key(nat.get("diagnostics", []))
                if ok and ref.get("ok"):
                    ok = canonical(ref["value"]) == canonical(nat["value"])
                detail[n] = f"ref={check_key(ref.get('diagnostics', []))[:2]} | {n}={check_key(nat.get('diagnostics', []))[:2]}"
            else:
                ok = check_key(ref) == check_key(nat)
                detail[n] = f"ref={check_key(ref)[:2]} | {n}={check_key(nat)[:2]}"
            verdicts[n] = ok
        row(f"{entry.relative_to(ROOT)} ({cmd_name})", verdicts, detail)

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
