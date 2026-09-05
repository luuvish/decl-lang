"""The Decl grammar as a `tree_sitter.Language` (compiled C extension)."""

from __future__ import annotations

from tree_sitter import Language

from ._binding import language as _language_ptr

LANGUAGE = Language(_language_ptr())

__all__ = ["LANGUAGE"]
