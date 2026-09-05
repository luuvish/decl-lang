"""`python -m decl` — the native implementation's command line (cli.py)."""

import sys

from .cli import main

if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
