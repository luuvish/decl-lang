"""Python API for the Decl language.

Every operation runs the native Python implementation (``decl.runtime``),
held byte-identical to the reference implementation by
tests/parity/differential.py.

    >>> import decl
    >>> decl.evaluate("site.decl", root="site")        # -> the evaluated value (dict/list/...)
    >>> decl.check("schema.decl")                        # -> [] when clean, else diagnostics
    >>> decl.validate("cfg.decl", input=("deployed", "doc.json"))
    >>> decl.format_source("const x=1+2\\n")             # -> 'const x = 1 + 2\\n'
"""
from __future__ import annotations

import json
import os
from typing import Any, Iterable, Sequence, TypedDict

__all__ = [
    "Diagnostic", "DeclError",
    "check", "evaluate", "evaluate_source", "validate", "format_source", "format_file",
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


def evaluate_source(text: str) -> dict:
    """Parse, check, and evaluate one module given as source text; returns
    the report dict (phase, ok, parse_errors, checks, diagnostics, outputs, inputs)."""
    from .runtime.pipeline import evaluate_source as run
    return run(text)


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
    with open(path, encoding="utf-8") as f:
        src = f.read()
    out = format_source(src)
    if out != src and not check:
        with open(path, "w", encoding="utf-8") as f:
            f.write(out)
    return out != src


def format_source(text: str) -> str:
    """Return the canonical formatting of a Decl source string."""
    from .runtime.fmt import format_source as fmt
    try:
        return fmt(text)
    except ValueError as e:
        raise DeclError(str(e)) from None
