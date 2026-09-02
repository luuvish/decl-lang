"""Console-script entry points: ``decl`` and ``decl-lsp``.

Both hand the process over to the bundled JavaScript under Node with the
user's arguments and exit status passed through unchanged.
"""
from __future__ import annotations

import sys

from ._runtime import NodeNotFound, run


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
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
