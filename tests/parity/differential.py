"""Parity across the three implementations.

Decl ships a TypeScript reference implementation (packages/typescript), a Rust
runtime (packages/rust), and a Python runtime (packages/python). They must be
indistinguishable: over every module with outputs in the fixture corpus,
the documentation examples, and the domain examples, each runtime's
`evaluate --json` report must carry the same `ok`, byte-identical
canonical output, and the same diagnostics; and binding documents to
input roots (`validate --input`) must yield the same root-cause
diagnostics. The reference is the oracle; both natives are diffed
against it, which makes the three pairwise identical.

    python tests/parity/differential.py                 # rust and python vs reference
    python tests/parity/differential.py --only rust     # one runtime
    DECL_PYTHON=packages/python/.venv/bin/python ...             # the interpreter that has `decl` installed

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
TS_CLI = ROOT / "packages/typescript/src/cli.ts"
RUST_BIN = ROOT / "target/release/decl"
PYTHON = os.environ.get("DECL_PYTHON") or sys.executable

only = None
if "--only" in sys.argv:
    only = sys.argv[sys.argv.index("--only") + 1]
RUNTIMES: dict[str, list[str]] = {}
if only in (None, "rust"):
    RUNTIMES["rust"] = [str(RUST_BIN)]
if only in (None, "python"):
    RUNTIMES["python"] = [PYTHON, "-m", "decl.runtime"]
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

print(f"\n{same} identical, {diff} different (reference vs {', '.join(names)})")
sys.exit(1 if diff else 0)
