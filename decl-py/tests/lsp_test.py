"""The language-server corpus (tests/lsp/<case>/) replayed over the native
server with the shared driver (tests/lsp/replay.py): every session's
answers against its transcript."""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("replay", ROOT / "tests/lsp/replay.py")
assert _spec and _spec.loader
replay = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(replay)
CASES = replay.cases(ROOT)


@pytest.mark.parametrize("case", CASES, ids=[c.name for c in CASES])
def test_session(case: Path) -> None:
    want = json.loads((case / "transcript.json").read_text(encoding="utf-8"))
    got = replay.replay(case, [sys.executable, "-m", "decl.lsp"])
    assert len(got) == len(want), [g[0] for g in got]
    for (label, expected), (_, answer) in zip(want, got, strict=True):
        # canonical text: key order counts, as the parity harness counts it
        assert json.dumps(answer) == json.dumps(expected), label
