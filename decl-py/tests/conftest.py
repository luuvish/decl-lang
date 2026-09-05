"""pytest fixtures: the repository root (the shared corpora live there) and
the interpreter the tests run under (the venv's, so subprocesses see the
same installed package)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture(scope="session")
def root() -> Path:
    return ROOT


@pytest.fixture(scope="session")
def python() -> str:
    return sys.executable
