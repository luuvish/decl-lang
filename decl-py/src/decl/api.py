"""Python API for the Decl language.

Every operation runs the native Python implementation (the modules beside
this file),
held byte-identical to the reference implementation by
tests/parity/differential.py. The functions mirror the ``decl`` command
line, in its vocabulary: ``evaluate`` binds inputs and returns outputs.

    >>> import decl
    >>> decl.evaluate("site.decl")          # {"site": {...}} — the exported outputs
    >>> decl.evaluate("cfg.decl", inputs={"deployed": "doc.json"}, outputs=["deployed"])["deployed"]
    >>> decl.check("schema.decl")           # [] when clean, else diagnostics
    >>> decl.validate("cfg.decl", inputs={"deployed": {"host": "h"}})
    ...                                     # a document may be a value, not a file
    >>> decl.format_source("const x=1+2\\n")                          # 'const x = 1 + 2\\n'
"""

from __future__ import annotations

import json
import os
from collections.abc import Iterable, Mapping, Sequence
from typing import Any, TypedDict, cast

__all__ = [
    "DeclError",
    "Diagnostic",
    "check",
    "evaluate",
    "evaluate_source",
    "format_file",
    "format_source",
    "validate",
]
__version__ = "0.3.0"


class Diagnostic(TypedDict, total=False):
    """One diagnostic, in the report's field order (§12.2)."""

    file: str
    code: str
    id: str
    severity: str
    message: str
    path: str


class DeclError(Exception):
    """Raised when an operation fails; ``diagnostics`` carries the report
    (empty for a usage error such as an unknown input or root)."""

    def __init__(self, message: str, diagnostics: Sequence[Diagnostic] = ()):
        super().__init__(message)
        self.diagnostics: list[Diagnostic] = list(diagnostics)


# a document to bind to an input: the path of a JSON file, or the value itself
InputDocument = "str | os.PathLike[str] | Any"
Inputs = "Mapping[str, InputDocument] | Iterable[tuple[str, InputDocument]]"


def _tagged(file: str, d: dict[str, Any]) -> Diagnostic:
    o: dict[str, Any] = {"file": file}
    if d.get("code"):
        o["code"] = d["code"]
    if d.get("id"):
        o["id"] = d["id"]
    o["severity"] = d["severity"]
    o["message"] = d["message"]
    o["path"] = d.get("path", "")
    return cast(Diagnostic, o)


def _fail(fallback: str, diagnostics: list[Any]) -> None:
    raise DeclError(diagnostics[0]["message"] if diagnostics else fallback, diagnostics)


def _file_tag(given: str, entry: Any, module_path: str) -> str:
    return given if entry is not None and module_path == entry.path else module_path


def _pairs(inputs: Any) -> list[Any]:
    if inputs is None:
        return []
    return list(inputs.items()) if isinstance(inputs, Mapping) else list(inputs)


def _bind_inputs(modules: list[Any], file: str, inputs: Any) -> list[Any]:
    """The documents to bind, each to the module that declares its input (§10)."""
    from .semantics import read_json

    binds = []
    for name, doc in _pairs(inputs):
        module = next((m for m in modules if name in m.env.inputs), None)
        if module is None:
            raise DeclError(f"no input named {name}")
        if isinstance(doc, (str, os.PathLike)):
            try:
                with open(doc, encoding="utf-8") as fh:
                    text = fh.read()
            except OSError:
                _fail(
                    "",
                    [
                        {
                            "file": file,
                            "code": "E6004",
                            "severity": "error",
                            "message": f"bound document cannot be read: {doc}",
                            "path": name,
                        }
                    ],
                )
            where = str(doc)
        else:
            text, where = json.dumps(doc), name
        try:
            raw = read_json(text)
        except Exception:
            _fail(
                "",
                [
                    {
                        "file": file,
                        "code": "E6004",
                        "severity": "error",
                        "message": f"bound document is not well-formed JSON: {where}",
                        "path": name,
                    }
                ],
            )
        binds.append({"module": module, "input": name, "raw": raw})
    return binds


def evaluate(
    path: str | os.PathLike[str],
    *,
    inputs: Mapping[str, Any] | Iterable[tuple[str, Any]] | None = None,
    outputs: Iterable[str] | None = None,
) -> dict[str, Any]:
    """Evaluate a module on the native runtime: bind the ``inputs`` documents
    (by input name; a JSON file path, or the value itself), run the pipeline,
    and return the requested roots' documents by name — ``outputs`` may name
    outputs and inputs (bound here, or demanded through their fallback);
    by default the entry module's exported outputs (§5.5). Raises DeclError
    with the diagnostics on any error-severity outcome."""
    from .checker import check_module
    from .cli import open_universe
    from .module import run_universe

    file = str(path)
    r = open_universe(file)
    if r["diags"] or r["entry"] is None:
        _fail(f"{file}: cannot be loaded", [_tagged(file, d) for d in r["diags"]])
    entry = r["entry"]
    checks = [
        _tagged(_file_tag(file, entry, m.path), d)
        for m in r["modules"]
        for d in check_module(m.decls, m.env)
    ]
    if checks:
        _fail("", checks)
    u = run_universe(r["modules"], entry, _bind_inputs(r["modules"], file, inputs))
    report = [_tagged(file, d) for d in u["diags"]]
    if any(d["severity"] == "error" for d in report):
        _fail("", report)
    names = (
        list(outputs)
        if outputs is not None
        else [o["name"] for o in entry.env.outputs if o.get("exported")]
    )
    out: dict[str, Any] = {}
    for n in names:
        if n not in entry.env.roots:
            raise DeclError(f"no root named {n}", report)
        out[n] = json.loads(u["eng"].serialize(entry.env.roots[n], n))
    return out


def check(*paths: str | os.PathLike[str]) -> list[Diagnostic]:
    """Parse and statically check entry files (module-aware) on the native
    runtime. Returns diagnostics; empty means clean."""
    from .cli import check_files

    return [_tagged(d["file"], d) for d in check_files([str(p) for p in paths])]


def validate(
    path: str | os.PathLike[str],
    *,
    inputs: Mapping[str, Any] | Iterable[tuple[str, Any]] | None = None,
    expect_errors: Iterable[str] | None = None,
) -> list[Diagnostic]:
    """Validate a file: static checks, then evaluation with the ``inputs``
    documents bound; returns every diagnostic (all severities). Raises
    DeclError when the file does not parse. With ``expect_errors`` the set
    of error codes must match exactly; DeclError carries the mismatch."""
    from .checker import check_module
    from .cli import open_universe
    from .module import run_universe
    from .parse import parse_source
    from .pipeline import run_pipeline

    file = str(path)
    try:
        with open(file, encoding="utf-8") as fh:
            src = fh.read()
    except OSError:
        raise DeclError(f"{file}: cannot be read") from None
    parsed = parse_source(src)
    if parsed["errors"]:
        raise DeclError(f"{file}: {len(parsed['errors'])} parse error(s)")
    decls = parsed["decls"]
    diags = [_tagged(file, d) for d in check_module(decls)]
    if not diags:
        if _pairs(inputs):
            r = open_universe(file)
            u = run_universe(r["modules"], r["entry"], _bind_inputs(r["modules"], file, inputs))
            diags = [_tagged(file, d) for d in u["diags"]]
        else:
            diags = [_tagged(file, d) for d in run_pipeline(decls)["diags"]]
    if expect_errors is not None:
        want = sorted(expect_errors)
        got = sorted(d.get("code") or "" for d in diags if d["severity"] == "error")
        if set(want) != set(got):
            raise DeclError(f"expected errors {want}, got {got}", diags)
    return diags


def evaluate_source(text: str) -> dict[str, Any]:
    """Parse, check, and evaluate one module given as source text; returns
    the report dict (phase, ok, parse_errors, checks, diagnostics, outputs, inputs)."""
    from .pipeline import evaluate_source as run

    return run(text)


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
    """Return the canonical formatting of a Decl source string; raises
    DeclError when it does not parse."""
    from .fmt import format_source as fmt

    try:
        return fmt(text)
    except ValueError as e:
        raise DeclError(str(e)) from None
