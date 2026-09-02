"""Console-script entry points: ``decl`` and ``decl-lsp``.

``check``, ``evaluate``, and ``validate`` run on the native Python
implementation (``decl.runtime``); ``fmt`` and the language server hand
the process to the bundled reference implementation under Node.js with
the exit status passed through.
"""
from __future__ import annotations

import sys

from ._runtime import NodeNotFound, run

NATIVE_COMMANDS = {"check", "evaluate", "validate"}


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if args and args[0] in NATIVE_COMMANDS and "--node" not in args:
        try:
            from .runtime.cli import main as native_main
        except ImportError as e:   # grammar extension missing — fall back to Node
            print(f"decl: native runtime unavailable ({e}); using the bundled reference implementation", file=sys.stderr)
        else:
            return native_main(args)
    args = [a for a in args if a != "--node"]
    try:
        return run("cli.js", args).returncode
    except NodeNotFound as e:
        print(f"decl: {e}", file=sys.stderr)
        return 2


def lsp_main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    try:
        return run("lsp.js", args).returncode
    except NodeNotFound as e:
        print(f"decl-lsp: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
