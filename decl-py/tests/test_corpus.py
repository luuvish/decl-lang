"""The shared corpora (tests/README.md at the repository root), driven by
the scripts the Makefile runs: this file only runs them under pytest so
one command reports everything."""

from __future__ import annotations

import subprocess
from pathlib import Path


def test_e2e_script_passes(root: Path, python: str) -> None:
    """scripts/e2e.py: the examples, packages, formatter, command line,
    language server, API, REPL corpus, and subsumption corpus."""
    r = subprocess.run(
        [python, "scripts/e2e.py"], cwd=root / "decl-py", capture_output=True, text=True
    )
    assert r.returncode == 0, f"scripts/e2e.py failed:\n{r.stdout}\n{r.stderr}"
    assert "0 failed" in r.stdout


def test_validation_corpus(root: Path, python: str) -> None:
    """`decl validate tests/validation`: every fixture parses, checks, and
    evaluates as its header says."""
    r = subprocess.run(
        [python, "-m", "decl.runtime", "validate", "tests/validation"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, f"validate failed:\n{r.stdout}\n{r.stderr}"
    assert r.stderr.rstrip().endswith("0 failed"), r.stderr  # the summary line goes to stderr
