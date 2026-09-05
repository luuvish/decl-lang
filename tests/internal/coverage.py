"""The internal checks (tests/internal/checks.json) across the three
implementations: every check must be carried, under its name, by
decl-ts/tests/internal/<module>_test.ts (`check('<name>', …)`),
decl-rs/tests/internal/<module>_test.rs (`#[test] fn <name>()`), and
decl-py/tests/internal/<module>_test.py (`def test_<name>(`). The parity
harness runs this as its last section; on its own it prints what each
language lacks or carries undeclared.

    python tests/internal/coverage.py
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

PATTERNS = {
    "typescript": (ROOT / "decl-ts/tests/internal", "_test.ts", re.compile(r"check\(\s*'([a-z_]+)'")),
    "rust": (ROOT / "decl-rs/tests/internal", "_test.rs", re.compile(r"#\[test\]\s*\n\s*fn ([a-z_]+)\(")),
    "python": (ROOT / "decl-py/tests/internal", "_test.py", re.compile(r"^def test_([a-z_]+)\(", re.M)),
}


def declared() -> list[str]:
    """the checks, as `module.name`, in the order checks.json names them"""
    return [f"{c['module']}.{c['name']}" for c in json.loads((ROOT / "tests/internal/checks.json").read_text(encoding="utf-8"))]


def carried(language: str) -> set[str]:
    """the checks a language's internal tests carry, as `module.name`"""
    d, suffix, pattern = PATTERNS[language]
    out: set[str] = set()
    if not d.is_dir():
        return out
    for f in sorted(d.iterdir()):
        if not f.name.endswith(suffix):
            continue
        module = f.name[: -len(suffix)]
        for m in pattern.finditer(f.read_text(encoding="utf-8")):
            out.add(f"{module}.{m.group(1)}")
    return out


def report() -> dict[str, dict[str, list[str]]]:
    """per language: the declared checks it lacks, and the checks it carries undeclared"""
    want = set(declared())
    out = {}
    for language in PATTERNS:
        have = carried(language)
        out[language] = {"missing": sorted(want - have), "undeclared": sorted(have - want)}
    return out


if __name__ == "__main__":
    r = report()
    bad = False
    for language, d in r.items():
        for kind in ("missing", "undeclared"):
            for name in d[kind]:
                print(f"{language}: {kind} {name}")
                bad = True
    print(f"{len(declared())} checks; " + ", ".join(f"{lang} {len(carried(lang))}" for lang in PATTERNS))
    sys.exit(1 if bad else 0)
