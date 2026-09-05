"""The command-line corpus (tests/cli/cases.json) through the native `decl`
and `decl-lsp`: every case's exit status, standard output, standard error,
and the files it leaves, against the recorded expectations
(tests/cli/README.md)."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

import pytest

import decl

ROOT = Path(__file__).resolve().parents[2]
CASES = json.loads((ROOT / "tests/cli/cases.json").read_text(encoding="utf-8"))


@pytest.mark.parametrize("case", CASES, ids=[c["name"] for c in CASES])
def test_case(case: dict[str, Any], python: str, tmp_path: Path) -> None:
    for name, text in case.get("files", {}).items():
        (tmp_path / name).write_text(text, encoding="utf-8")
    module = "decl.lsp" if case.get("program") == "decl-lsp" else "decl"
    args = [a.replace("<dir>", str(tmp_path)) for a in case["args"]]
    r = subprocess.run(
        [python, "-m", module, *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        input=case.get("stdin", ""),
    )

    def norm(s: str) -> str:
        return s.replace(str(tmp_path), "<dir>").replace(decl.__version__, "<version>")

    assert (r.returncode, norm(r.stdout), norm(r.stderr)) == (
        case["exit"],
        case["stdout"],
        case["stderr"],
    )
    for name, text in case.get("after", {}).items():
        p = tmp_path / name
        assert (p.read_text(encoding="utf-8") if p.exists() else None) == text, name
