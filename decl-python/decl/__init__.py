"""Python API for the Decl language.

``check``, ``evaluate``, and ``validate`` run the native Python runtime
(``decl.runtime``, byte-identical to the reference implementation —
see tests/parity/differential.py); the formatter is a thin wrapper over
the bundled reference implementation.

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
    """Parse and statically check entry files (module-aware) on the native
    runtime. Returns diagnostics; empty means clean."""
    from .runtime.cli import check_files
    return [dict(file=d["file"], **{k: v for k, v in d.items() if k != "file"}) for d in check_files([str(p) for p in paths])]


def evaluate(path: str | os.PathLike[str], root: str | None = None) -> Any:
    """Evaluate a module's outputs on the native runtime. With ``root`` returns
    that output's value; otherwise a dict of every universe root. Raises
    DeclError with diagnostics on failure."""
    from .runtime.cli import evaluate_file
    code, text, diags = evaluate_file(str(path), root)
    if code != 0 or text is None:
        raise DeclError("evaluation failed", [dict(file=str(path), **d) for d in diags])
    return json.loads(text)


def validate(
    path: str | os.PathLike[str],
    *,
    input: tuple[str, str | os.PathLike[str]] | None = None,
    expect_errors: Iterable[str] | None = None,
) -> list[Diagnostic]:
    """Validate a file on the native runtime (optionally binding an input
    document as ``(name, json_path)``). Returns diagnostics. With
    ``expect_errors`` the error-code set must match exactly; DeclError
    carries the mismatch otherwise."""
    from .runtime.cli import validate_file
    parse_errors, found = validate_file(str(path), f"{input[0]}={input[1]}" if input else None)
    if parse_errors:
        raise DeclError(f"{path}: {parse_errors} parse error(s)")
    diags = [dict(file=str(path), **d) for d in found]
    if expect_errors is not None:
        want = sorted(expect_errors)
        got = sorted(d.get("code") or "" for d in diags if d["severity"] == "error")
        if set(want) != set(got):
            raise DeclError(f"expected errors {want}, got {got}", diags)
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
