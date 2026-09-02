"""Python API for the Decl language.

Thin, faithful wrappers over the ``decl`` CLI's machine-readable modes:
every function runs the bundled reference implementation and returns
its JSON — no second implementation of the language exists here.

    >>> import decl
    >>> decl.evaluate("site.decl", root="site")        # -> the evaluated value (dict/list/...)
    >>> decl.check("schema.decl")                        # -> [] when clean, else diagnostics
    >>> decl.validate("cfg.decl", input=("deployed", "doc.json"))
    >>> decl.format_source("const x=1+2\\n")             # -> 'const x = 1 + 2\\n'
"""
from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
from typing import Any, Iterable, Sequence, TypedDict

from ._runtime import NodeNotFound, node_executable, run

__all__ = [
    "Diagnostic", "DeclError", "NodeNotFound",
    "check", "evaluate", "validate", "format_source", "format_file", "node_executable",
]
__version__ = "0.2.0"


class Diagnostic(TypedDict, total=False):
    file: str
    severity: str
    code: str
    id: str
    path: str
    message: str


class DeclError(Exception):
    """Raised when an operation fails; ``diagnostics`` carries the report."""

    def __init__(self, message: str, diagnostics: Sequence[Diagnostic] = ()):
        super().__init__(message)
        self.diagnostics: list[Diagnostic] = list(diagnostics)


def _json_stdout(proc) -> Any:
    text = proc.stdout.strip()
    if not text:
        raise DeclError(proc.stderr.strip() or "decl produced no output")
    return json.loads(text)


def check(*paths: str | os.PathLike[str]) -> list[Diagnostic]:
    """Parse and statically check entry files (module-aware). Returns diagnostics; empty means clean."""
    proc = run("cli.js", ["check", *map(str, paths), "--json"], capture=True)
    return _json_stdout(proc)


def evaluate(path: str | os.PathLike[str], root: str | None = None) -> Any:
    """Evaluate a module's outputs. With ``root`` returns that output's value;
    otherwise a dict of every universe root. Raises DeclError with diagnostics on failure."""
    args = ["evaluate", str(path), "--json"]
    if root is not None:
        args += ["--root", root]
    proc = run("cli.js", args, capture=True)
    report = _json_stdout(proc)
    if not report.get("ok"):
        raise DeclError("evaluation failed", report.get("diagnostics", []))
    return report.get("value")


def validate(
    path: str | os.PathLike[str],
    *,
    input: tuple[str, str | os.PathLike[str]] | None = None,
    expect_errors: Iterable[str] | None = None,
) -> list[Diagnostic]:
    """Validate a file (optionally binding an input document as ``(name, json_path)``).
    Returns diagnostics. With ``expect_errors`` the CLI judges the error set and the
    return value is empty on match; DeclError carries the mismatch otherwise."""
    args = ["validate", str(path), "--json"]
    if input is not None:
        args += ["--input", f"{input[0]}={input[1]}"]
    if expect_errors is not None:
        args += ["--expect-errors", ",".join(expect_errors)]
    proc = run("cli.js", args, capture=True)
    diags = _json_stdout(proc)
    if expect_errors is not None and proc.returncode != 0:
        raise DeclError(proc.stderr.strip() or "expected-error set did not match", diags)
    return diags


def format_file(path: str | os.PathLike[str], *, check: bool = False) -> bool:
    """Canonically format a file in place. Returns True if it was (or would be) changed."""
    args = ["fmt", str(path)] + (["--check"] if check else [])
    proc = run("cli.js", args, capture=True)
    if proc.returncode not in (0, 1):
        raise DeclError(proc.stderr.strip() or "fmt failed")
    return "reformat" in proc.stderr


def format_source(text: str) -> str:
    """Return the canonical formatting of a Decl source string."""
    with tempfile.TemporaryDirectory() as d:
        p = Path(d) / "source.decl"
        p.write_text(text, encoding="utf-8")
        proc = run("cli.js", ["fmt", str(p)], capture=True)
        if proc.returncode != 0:
            raise DeclError(proc.stderr.strip() or "fmt failed")
        return p.read_text(encoding="utf-8")
