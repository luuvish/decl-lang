"""Console-script entry points: ``decl`` (check / evaluate / validate /
fmt) and ``decl-lsp`` — the native Python implementation, no Node.js."""

from __future__ import annotations

import sys


def main(argv: list[str] | None = None) -> int:
    from .runtime.cli import main as run

    return run(sys.argv[1:] if argv is None else argv)


def lsp_main(argv: list[str] | None = None) -> int:
    from .runtime.lsp import main as run

    return run(sys.argv[1:] if argv is None else argv)


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
