"""The fixture corpus (tests/validation, tests/validation/README.md) judged
by `decl validate <dir>`: every fixture parses, checks, and evaluates as
its header says — the native counterpart of the reference's corpus judge
(decl-ts/src/conformance.ts)."""

from __future__ import annotations

import subprocess
from pathlib import Path


def test_validation_corpus(root: Path, python: str) -> None:
    r = subprocess.run(
        [python, "-m", "decl", "validate", "tests/validation"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, f"validate failed:\n{r.stdout}\n{r.stderr}"
    assert r.stderr.rstrip().endswith("0 failed"), r.stderr  # the summary line goes to stderr
