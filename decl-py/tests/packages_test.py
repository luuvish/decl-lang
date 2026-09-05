"""Packages, manifests, and the lock file (§8.6-8.7) through the package
corpus (tests/packages/cases.json): the manifest and resolution errors as
the command line reports them, and the lock — written by the API into a
copy of the package, bit for bit the committed text, verified clean, then
failing closed on content drift, version drift, and a missing entry."""

from __future__ import annotations

import json
import re
import shutil
import subprocess
from pathlib import Path
from typing import Any

import pytest

from decl.package import open_package_universe, verify_lock, write_lock

ROOT = Path(__file__).resolve().parents[2]
CASES = json.loads((ROOT / "tests/packages/cases.json").read_text(encoding="utf-8"))


def codes_of(python: str, entry: Path) -> list[str]:
    """the codes `decl check` reports for an entry, in order"""
    r = subprocess.run(
        [python, "-m", "decl", "check", str(entry)],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return re.findall(r"\[(E\d{4})\]", r.stderr)


@pytest.mark.parametrize("case", CASES["errors"], ids=[c["entry"] for c in CASES["errors"]])
def test_manifest_and_resolution_errors(case: dict[str, Any], python: str) -> None:
    assert codes_of(python, ROOT / case["entry"]) == case["codes"]


def fresh(tmp_path: Path, tag: str) -> tuple[Path, Path]:
    """a fresh copy of the package, with its lock written by the API"""
    lock = CASES["lock"]
    d = tmp_path / tag
    shutil.copytree(ROOT / lock["package"], d)
    entry = d / lock["entry"]
    write_lock(open_package_universe(str(entry)))
    return d, entry


def test_lock_is_reproducible_and_verifies_clean(python: str, tmp_path: Path) -> None:
    d, entry = fresh(tmp_path, "fresh")
    expected = (ROOT / CASES["lock"]["lock"]).read_text(encoding="utf-8")
    assert (d / "decl.lock").read_text(encoding="utf-8") == expected
    assert verify_lock(open_package_universe(str(entry))) == []
    assert codes_of(python, entry) == []


@pytest.mark.parametrize(
    "drift", CASES["lock"]["drift"], ids=[d["name"] for d in CASES["lock"]["drift"]]
)
def test_lock_fails_closed(drift: dict[str, Any], python: str, tmp_path: Path) -> None:
    d, entry = fresh(tmp_path, "drift")
    for f, text in drift.get("append", {}).items():
        with open(d / f, "a", encoding="utf-8") as fh:
            fh.write(text)
    if "lock_replace" in drift:
        a, b = drift["lock_replace"]
        p = d / "decl.lock"
        p.write_text(p.read_text(encoding="utf-8").replace(a, b, 1), encoding="utf-8")
    if "lock_text" in drift:
        (d / "decl.lock").write_text(drift["lock_text"], encoding="utf-8")
    assert codes_of(python, entry) == drift["codes"]
