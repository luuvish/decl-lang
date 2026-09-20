"""The bytes the package writes do not depend on the platform: a line feed
where the text has one, and UTF-8. On Windows a text stream opened without
saying so writes a carriage return before each, which no corpus driver can
see on the platforms the gate runs on; the release's Windows job compares an
evaluated document with its golden byte for byte."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Any

import pytest

from decl.cli import plain_streams

SOURCE = Path(__file__).resolve().parents[1] / "src" / "decl"
OPENS = re.compile(r"\bopen\((?:[^()\n]|\([^()\n]*\))*\)")


def test_every_file_opened_for_writing_says_its_line_ending() -> None:
    writes = [
        (path.name, call)
        for path in sorted(SOURCE.rglob("*.py"))
        for call in OPENS.findall(path.read_text(encoding="utf-8"))
        if re.search(r"""["'][wax]\+?["']""", call)
    ]
    assert writes, "the scan finds the package's writes"
    assert [w for w in writes if 'newline="\\n"' not in w[1]] == []


def test_nothing_writes_text_through_pathlib() -> None:
    # Path.write_text translates line endings and takes no `newline` before 3.10
    users = [p.name for p in sorted(SOURCE.rglob("*.py")) if ".write_text(" in p.read_text("utf-8")]
    assert users == []


class Stream:
    def __init__(self) -> None:
        self.asked: list[dict[str, Any]] = []

    def reconfigure(self, **options: Any) -> None:
        self.asked.append(options)


def test_the_standard_streams_are_set_to_line_feeds_and_utf8(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    out, err = Stream(), Stream()
    monkeypatch.setattr(sys, "stdout", out)
    monkeypatch.setattr(sys, "stderr", err)
    plain_streams()
    assert out.asked == err.asked == [{"encoding": "utf-8", "newline": "\n"}]


def test_a_stream_that_cannot_be_reconfigured_is_left_alone(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(sys, "stdout", object())
    monkeypatch.setattr(sys, "stderr", object())
    plain_streams()
