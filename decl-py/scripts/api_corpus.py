"""The API corpus (tests/api/cases.json) through the Python API: every case
run from the repository root, the answers printed as one JSON array in the
form tests/api/README.md fixes — what the parity harness diffs and what
tests/test_api.py compares with tests/api/expected.json.

    decl-py/.venv/bin/python decl-py/scripts/api_corpus.py
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "decl-py/src"))

import decl


def _document(spec: dict[str, Any]) -> Any:
    # a file is named by its path (a string); any other value is the document itself
    return spec["file"] if "file" in spec else spec["json"]


def run_case(case: dict[str, Any]) -> dict[str, Any]:
    name = case["name"]
    try:
        if "evaluate" in case:
            kw: dict[str, Any] = {}
            if "inputs" in case:
                kw["inputs"] = {k: _document(v) for k, v in case["inputs"].items()}
            if "outputs" in case:
                kw["outputs"] = case["outputs"]
            value: Any = decl.evaluate(case["evaluate"], **kw)
        elif "render" in case:
            kw = {}
            if "inputs" in case:
                kw["inputs"] = {k: _document(v) for k, v in case["inputs"].items()}
            for key in ("outputs", "format", "indent"):
                if key in case:
                    kw[key] = case[key]
            if "templates" in case:
                kw["templates"] = {
                    k: v["file"] if "file" in v else {"text": v["text"]}
                    for k, v in case["templates"].items()
                }
            value = decl.render(case["render"], **kw)
        elif "check" in case:
            value = decl.check(*case["check"])
        elif "validate" in case:
            kw = {}
            if "inputs" in case:
                kw["inputs"] = {k: _document(v) for k, v in case["inputs"].items()}
            value = decl.validate(case["validate"], **kw)
        elif "format_source" in case:
            value = decl.format_source(case["format_source"])
        else:
            raise ValueError(f"unknown call in {name}")
        return {"name": name, "ok": True, "value": value}
    except decl.DeclError as e:
        return {"name": name, "ok": False, "message": str(e), "diagnostics": e.diagnostics}


def run_all() -> list[dict[str, Any]]:
    os.chdir(ROOT)
    cases = json.loads((ROOT / "tests/api/cases.json").read_text(encoding="utf-8"))
    return [run_case(c) for c in cases]


if __name__ == "__main__":
    print(json.dumps(run_all(), indent=2, ensure_ascii=False))
