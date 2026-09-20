"""The distribution's package list against the source tree: a subpackage that
the manifest does not name is left out of the wheel, which the corpus drivers,
running from the tree, cannot see."""

from __future__ import annotations

import re
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]


def test_every_source_package_is_in_the_distribution() -> None:
    manifest = (HERE / "pyproject.toml").read_text(encoding="utf-8")
    listed = re.search(r"^packages = \[([^\]]*)\]", manifest, re.M)
    assert listed is not None, "pyproject.toml lists its packages explicitly"
    declared = set(re.findall(r'"([^"]+)"', listed.group(1)))
    source = HERE / "src"
    present = {
        ".".join(init.parent.relative_to(source).parts)
        for init in source.rglob("__init__.py")
        if "__pycache__" not in init.parts
    }
    assert declared == present
