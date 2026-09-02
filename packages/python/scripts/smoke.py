"""Smoke-test the Python package the way a user gets it: build the wheel,
install it into a fresh venv, and drive the installed `decl` console
script and the `decl` API (Node from PATH or from `decl[node]`)."""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import venv
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
passed = failed = 0


def check(name: str, cond: bool, detail: str = "") -> None:
    global passed, failed
    if cond:
        passed += 1
        print(f"  ok   {name}")
    else:
        failed += 1
        print(f"  FAIL {name} {detail}")


def sh(*cmd: str, cwd: str | Path | None = None, env: dict | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(list(cmd), cwd=cwd, env=env, capture_output=True, text=True, check=False)


wheels = sorted((HERE / "dist").glob("decl-*.whl"))
check("wheel built", bool(wheels), "run `python -m build` first")
wheel = wheels[-1]
print(f"  wheel {wheel.name} ({wheel.stat().st_size // 1024} KB)")

with tempfile.TemporaryDirectory(prefix="decl-py-smoke-") as tmp:
    venv.create(tmp, with_pip=True)
    py = Path(tmp) / ("Scripts" if os.name == "nt" else "bin") / "python"
    r = sh(str(py), "-m", "pip", "install", "--quiet", "--disable-pip-version-check", str(wheel))
    check("wheel installs into a fresh venv", r.returncode == 0, r.stderr[-300:])
    bindir = py.parent
    decl_bin = bindir / "decl"
    check("decl console script installed", decl_bin.exists())

    src = Path(tmp) / "t.decl"
    src.write_text("type T = { a: int, const b = a * 2 }\nexport output t: T = { a: 21 }\n")
    r = sh(str(decl_bin), "evaluate", str(src), "--root", "t")
    check("installed decl evaluates", r.returncode == 0 and r.stdout.strip() == '{"a":21,"b":42}', r.stderr[-300:])

    prog = f"""
import json, decl
v = decl.evaluate({str(src)!r}, root='t')
d = decl.check({str(src)!r})
bad = {str(Path(tmp) / 'bad.decl')!r}
open(bad, 'w').write('type Bad = 10..3\\n')
e = decl.check(bad)
f = decl.format_source('const x=1+2\\n')
print(json.dumps({{'v': v, 'clean': d, 'codes': [x['code'] for x in e], 'f': f}}))
"""
    r = sh(str(py), "-c", prog)
    out = json.loads(r.stdout.strip() or "{}") if r.returncode == 0 else {}
    check("decl.evaluate returns the value", out.get("v") == {"a": 21, "b": 42}, r.stderr[-300:])
    check("decl.check returns [] when clean", out.get("clean") == [])
    check("decl.check returns coded diagnostics", out.get("codes") == ["E4011"], str(out.get("codes")))
    check("decl.format_source canonicalizes", out.get("f") == "const x = 1 + 2\n")

    r = sh(str(py), "-c", "import decl; print(decl.__version__)")
    check("package version exposed", r.stdout.strip() == "0.2.0", r.stdout)

print(f"\nTOTAL {passed} ok, {failed} failed")
sys.exit(1 if failed else 0)
