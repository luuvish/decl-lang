"""Builds the tree-sitter grammar extension; everything else is declared
in pyproject.toml. The grammar sources under src/decl/_tree_sitter/src are
synced from ../tree-sitter-decl/src by `npm run build` in ../decl-ts;
inside the repository they are copied from the grammar directly when
that sync has not run (so `pip install -e python` works from a fresh
checkout)."""

import shutil
from pathlib import Path

from setuptools import Extension, setup

_here = Path(__file__).resolve().parent
_src = _here / "src/decl/_tree_sitter/src"
_grammar = _here.parent / "tree-sitter-decl/src"
if not (_src / "parser.c").exists() and (_grammar / "parser.c").exists():
    shutil.rmtree(_src, ignore_errors=True)
    (_src / "tree_sitter").mkdir(parents=True, exist_ok=True)
    for f in ("parser.c", "scanner.c"):
        shutil.copy(_grammar / f, _src / f)
    for f in (_grammar / "tree_sitter").iterdir():
        shutil.copy(f, _src / "tree_sitter" / f.name)

_headers = _src / "tree_sitter"

setup(
    ext_modules=[
        Extension(
            "decl._tree_sitter._binding",
            sources=[
                "src/decl/_tree_sitter/binding.c",
                "src/decl/_tree_sitter/src/parser.c",
                "src/decl/_tree_sitter/src/scanner.c",
            ],
            include_dirs=["src/decl/_tree_sitter/src"],
            # The directory catches additions/removals; files catch in-place edits.
            # Its absolute path keeps the directory out of the source manifest.
            depends=[
                str(_headers),
                *(str(path.relative_to(_here)) for path in sorted(_headers.glob("*"))),
            ],
            extra_compile_args=["-std=c11"],
        )
    ]
)
