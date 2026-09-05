"""The golden manifest (tests/golden/manifest.json) through the native
command line: every entry's bytes, exactly (tests/golden/README.md)."""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = json.loads((ROOT / "tests/golden/manifest.json").read_text(encoding="utf-8"))


def module_of(entry: dict, tmp_path: Path) -> str:
    """a markdown entry's module: its ```decl blocks in order, in a temporary file"""
    if "module" in entry:
        return entry["module"]
    md = (ROOT / entry["markdown"]).read_text(encoding="utf-8")
    src = "\n".join(re.findall(r"```decl\n([\s\S]*?)```", md))
    p = tmp_path / "guide.decl"
    p.write_text(src, encoding="utf-8")
    return str(p)


@pytest.mark.parametrize("entry", MANIFEST, ids=[e["golden"] for e in MANIFEST])
def test_golden(entry: dict, python: str, tmp_path: Path) -> None:
    rejected = entry.get("rejected", False)
    args = ["validate" if rejected else "evaluate", module_of(entry, tmp_path)]
    for spec in entry.get("inputs", []):
        args += ["--input", spec]
    if "output" in entry:
        args += ["--output", entry["output"]]
    r = subprocess.run([python, "-m", "decl", *args], cwd=ROOT, capture_output=True, text=True)
    expected = (ROOT / entry["golden"]).read_text(encoding="utf-8")
    assert r.returncode == (1 if rejected else 0), r.stderr
    assert (r.stderr if rejected else r.stdout) == expected
