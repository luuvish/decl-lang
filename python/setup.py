"""Builds the tree-sitter grammar extension; everything else is declared
in pyproject.toml. The grammar sources under decl/_tree_sitter/src are
synced from ../tree-sitter-decl/src by `npm run build` in ../impl."""
from setuptools import Extension, setup

setup(
    ext_modules=[
        Extension(
            "decl._tree_sitter._binding",
            sources=[
                "decl/_tree_sitter/binding.c",
                "decl/_tree_sitter/src/parser.c",
                "decl/_tree_sitter/src/scanner.c",
            ],
            include_dirs=["decl/_tree_sitter/src"],
            extra_compile_args=["-std=c11"],
        )
    ]
)
