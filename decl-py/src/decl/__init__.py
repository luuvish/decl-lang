"""Decl, implemented natively in Python: the parser binding, the static
checker, the evaluator, modules and packages, the canonical formatter, and
the language server — the same behavior as the TypeScript reference
implementation, verified by tests/parity in the repository. The package's
entry re-exports the high-level API (api.py) in the command line's
vocabulary; the modules it is built from (parse, semantics, subsume, infer,
checker, engine, module, package, fmt, conformance, session, repl,
pipeline, lsp, cli) are importable as well."""

# the render module is loaded before the function of the same name below
# takes the package attribute; a later import of the module leaves it
from . import render as _render_module  # noqa: F401
from .api import (
    DeclError,
    Diagnostic,
    __version__,
    check,
    evaluate,
    evaluate_source,
    format_file,
    format_source,
    render,
    to_json,
    to_yaml,
    validate,
)

__all__ = [
    "DeclError",
    "Diagnostic",
    "__version__",
    "check",
    "evaluate",
    "evaluate_source",
    "format_file",
    "format_source",
    "render",
    "to_json",
    "to_yaml",
    "validate",
]
