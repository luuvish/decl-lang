"""Differential test: the native runtimes must produce byte-identical
canonical output and the same diagnostic codes as the TypeScript
reference implementation, over every module with outputs in the corpus
and the examples. Usage:

    python scripts/differential.py            # Python runtime vs reference
    python scripts/differential.py --rust     # Rust runtime (cargo build first) vs reference
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
PYTHON = sys.executable
RUST_BIN = ROOT / "rust" / "target" / "release" / "decl"
use_rust = "--rust" in sys.argv

files: list[Path] = []
for d in ("tests/validation", "docs/examples", "examples"):
    for p in sorted((ROOT / d).rglob("*.decl")):
        txt = p.read_text(encoding="utf-8")
        if "output " in txt and "/invalid/" not in str(p):
            files.append(p)


def run_ref(p: Path) -> dict:
    r = subprocess.run(["node", str(ROOT / "impl/src/cli.ts"), "evaluate", str(p), "--json"],
                       capture_output=True, text=True, check=False)
    try:
        return json.loads(r.stdout.strip() or "{}")
    except json.JSONDecodeError:
        return {"ok": False, "value": None, "diagnostics": [], "stderr": r.stderr[-200:]}


def run_native(p: Path) -> dict:
    cmd = [str(RUST_BIN), "evaluate", str(p), "--json"] if use_rust \
        else [PYTHON, "-m", "decl.runtime", "evaluate", str(p), "--json"]
    r = subprocess.run(cmd, capture_output=True, text=True, check=False, cwd=str(ROOT / "python"))
    try:
        return json.loads(r.stdout.strip() or "{}")
    except json.JSONDecodeError:
        return {"ok": False, "value": None, "diagnostics": [], "stderr": r.stderr[-400:]}


def codes(rep: dict) -> list:
    return sorted((d.get("code") or "", d.get("id") or "", d.get("path") or "") for d in rep.get("diagnostics", []))


same = diff = 0
for p in files:
    ref, nat = run_ref(p), run_native(p)
    rel = str(p.relative_to(ROOT))
    ok = ref.get("ok") == nat.get("ok") and json.dumps(ref.get("value"), sort_keys=False) == json.dumps(nat.get("value"), sort_keys=False) \
        and codes(ref) == codes(nat)
    # byte-identical serialization: compare the raw value text
    if ok and ref.get("ok"):
        ok = json.dumps(ref["value"], separators=(",", ":"), ensure_ascii=False) == json.dumps(nat["value"], separators=(",", ":"), ensure_ascii=False)
    if ok:
        same += 1
        print(f"  same {rel}")
    else:
        diff += 1
        print(f"  DIFF {rel}")
        print(f"       ref: ok={ref.get('ok')} codes={codes(ref)[:3]} value={str(ref.get('value'))[:160]}")
        print(f"       nat: ok={nat.get('ok')} codes={codes(nat)[:3]} value={str(nat.get('value'))[:160]} {nat.get('stderr','')[:300]}")


# --- documents bound to input roots: same root-cause diagnostics ---
def native_cmd() -> list[str]:
    return [str(RUST_BIN)] if use_rust else [PYTHON, "-m", "decl.runtime"]


def run_validate(cmd: list[str], decl: Path, name: str, doc: Path) -> list:
    r = subprocess.run(cmd + ["validate", str(decl), "--input", f"{name}={doc}", "--json"],
                       capture_output=True, text=True, check=False, cwd=str(ROOT / "python"))
    try:
        return json.loads(r.stdout.strip() or "[]")
    except json.JSONDecodeError:
        return [{"code": "<no report>", "message": r.stderr[-300:]}]


def run_evaluate_ref(decl: Path, root: str) -> str:
    r = subprocess.run(["node", str(ROOT / "impl/src/cli.ts"), "evaluate", str(decl), "--root", root],
                       capture_output=True, text=True, check=True)
    return r.stdout.strip()


import tempfile
tmp = Path(tempfile.mkdtemp(prefix="decl-diff-"))
cases: list[tuple[str, Path, str, Path]] = []
cfg = ROOT / "docs/examples/02_config.decl"
bad = tmp / "deployed.json"
bad.write_text('{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}', encoding="utf-8")
cases.append(("config: invalid deployment", cfg, "deployed", bad))
ic = ROOT / "docs/examples/01_interconnect.decl"
ser = run_evaluate_ref(ic, "xbar")
good = tmp / "xbar.json"
good.write_text(ser, encoding="utf-8")
cases.append(("interconnect: round trip", ic, "doc", good))
corrupt = tmp / "xbar_corrupt.json"
probe = '"mi1":{"kind":"ext","mode":"mi","width":64}'
assert probe in ser, "corruption probe no longer matches the serialized form"
corrupt.write_text(ser.replace(probe, probe.replace("64", "32"), 1), encoding="utf-8")
cases.append(("interconnect: corrupted width", ic, "doc", corrupt))

for label, decl, name, doc in cases:
    ref = run_validate(["node", str(ROOT / "impl/src/cli.ts")], decl, name, doc)
    nat = run_validate(native_cmd(), decl, name, doc)
    if codes({"diagnostics": ref}) == codes({"diagnostics": nat}):
        same += 1
        print(f"  same {label} ({len(ref)} diagnostic(s))")
    else:
        diff += 1
        print(f"  DIFF {label}\n       ref: {codes({'diagnostics': ref})}\n       nat: {codes({'diagnostics': nat})}")

print(f"\n{same} identical, {diff} different ({'rust' if use_rust else 'python'} vs reference)")
sys.exit(1 if diff else 0)
