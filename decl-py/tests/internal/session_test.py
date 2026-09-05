"""session (tests/internal/checks.json): the operation log — apply, undo,
redo, and a new operation after undo discarding the redo tail."""

from __future__ import annotations

from pathlib import Path

import pytest

from decl.session import Session, SessionError

ROOT = Path(__file__).resolve().parents[3]


def test_undo_redo() -> None:
    s = Session(str(ROOT / "tests/repl/documents/main.decl"))

    def bind(text: str) -> None:
        s.apply({"op": "bind", "name": "extra", "src": {"kind": "inline", "text": text}})

    bind('{ "port": 1, "name": "x" }')
    bound = s.document_text("extra")
    assert bound == '{"port":1,"name":"x"}'
    assert s.undo() == 1
    with pytest.raises(SessionError):  # undone: the root is invalid
        s.document_text("extra")
    assert s.redo() == 1
    assert s.document_text("extra") == bound
    s.undo()
    bind('{ "port": 2, "name": "y" }')
    assert s.redo() == 0, "a new operation after undo discards the redo tail"
    assert s.document_text("extra") == '{"port":2,"name":"y"}'
