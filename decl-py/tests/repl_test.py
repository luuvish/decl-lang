"""`decl repl`: the session corpus (tests/repl/<case>/), each case replayed
in a fresh copy of its directory against its transcript and the files it
leaves (tests/repl/README.md), and again under DECL_FULL_RECOMPUTE=1 —
the incremental step is observationally identical to a full recomputation
(docs/tooling/02_repl.md §6)."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
CASES = sorted(p for p in (ROOT / "tests/repl").iterdir() if (p / "session.txt").exists())


def normalize(s: str) -> str:
    """milliseconds are the clock's, not the session's"""
    return re.sub(r"\d+\.\d ms", "<ms> ms", s)


def run(python: str, case: Path, tmp_path: Path, tag: str, full: bool) -> tuple[int, str, str]:
    d = tmp_path / tag
    shutil.copytree(case, d)
    entry = ["main.decl"] if (case / "main.decl").exists() else []
    env = {k: v for k, v in os.environ.items() if k != "DECL_FULL_RECOMPUTE"}
    if full:
        env["DECL_FULL_RECOMPUTE"] = "1"
    r = subprocess.run(
        [python, "-m", "decl", "repl", *entry, "--script", "session.txt"],
        cwd=d,
        capture_output=True,
        text=True,
        env=env,
    )
    return r.returncode, normalize(r.stdout), r.stderr


@pytest.mark.parametrize("case", CASES, ids=[c.name for c in CASES])
def test_session(case: Path, python: str, tmp_path: Path) -> None:
    want = (case / "transcript.txt").read_text(encoding="utf-8")
    want_code = 1 if re.search(r"^error: ", want, re.M) else 0
    code, out, err = run(python, case, tmp_path, "inc", full=False)
    assert out == want
    assert code == want_code, err
    # the files the session leaves: every file under expected/, byte for byte
    expected = case / "expected"
    if expected.is_dir():
        for f in sorted(expected.iterdir()):
            actual = tmp_path / "inc" / f.name
            assert actual.exists(), f.name
            assert actual.read_text(encoding="utf-8") == f.read_text(encoding="utf-8"), f.name
    # the incremental step against a full recomputation
    code_full, full, _ = run(python, case, tmp_path, "full", full=True)
    # what only the incremental step reports: the count of recomputed slots
    assert (code, re.sub(r", recomputed \d+ of \d+ slots", "", out)) == (code_full, full)
