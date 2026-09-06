"""The render corpus (tests/render) through the native command line: the
format goldens of formats.json (`--format yaml`, `--indent n`, and the
YAML read back to the golden document), every golden document bound from
its YAML twin under inputs/, and the documents under invalid/ that the
reader must refuse with their messages (tests/render/README.md)."""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Any

import pytest

from decl.semantics import read_json
from decl.yaml import YamlError, read_yaml, to_json

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = json.loads((ROOT / "tests/golden/manifest.json").read_text(encoding="utf-8"))
FORMATS = json.loads((ROOT / "tests/render/formats.json").read_text(encoding="utf-8"))
INVALID = json.loads((ROOT / "tests/render/invalid/cases.json").read_text(encoding="utf-8"))


def module_of(entry: dict[str, Any], tmp_path: Path) -> str:
    """a markdown entry's module: its ```decl blocks in order, in a temporary file"""
    if "module" in entry:
        return str(entry["module"])
    md = (ROOT / entry["markdown"]).read_text(encoding="utf-8")
    src = "\n".join(re.findall(r"```decl\n([\s\S]*?)```", md))
    p = tmp_path / "guide.decl"
    p.write_text(src, encoding="utf-8")
    return str(p)


def args_of(entry: dict[str, Any], tmp_path: Path, inputs: list[str] | None = None) -> list[str]:
    args = ["validate" if entry.get("rejected") else "evaluate", module_of(entry, tmp_path)]
    for spec in entry.get("inputs", []) if inputs is None else inputs:
        args += ["--input", spec]
    if "output" in entry:
        args += ["--output", entry["output"]]
    return args


def run(python: str, args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run([python, "-m", "decl", *args], cwd=ROOT, capture_output=True, text=True)


@pytest.mark.parametrize("f", FORMATS, ids=[f["golden"] for f in FORMATS])
def test_formats(f: dict[str, Any], python: str, tmp_path: Path) -> None:
    entry = next(e for e in MANIFEST if e["golden"] == f["golden"])
    golden = (ROOT / f["golden"]).read_text(encoding="utf-8")
    yaml = (ROOT / f["yaml"]).read_text(encoding="utf-8")
    r = run(python, [*args_of(entry, tmp_path), "--format", "yaml"])
    assert r.returncode == 0 and r.stdout == yaml, r.stderr
    assert to_json(read_yaml(yaml)) + "\n" == golden
    for n, file in f["indent"].items():
        want = (ROOT / file).read_text(encoding="utf-8")
        r = run(python, [*args_of(entry, tmp_path), "--indent", n])
        assert r.returncode == 0 and r.stdout == want, r.stderr
        assert to_json(read_json(want)) + "\n" == golden


def twin(spec: str) -> str:
    return re.sub(r"=tests/golden/inputs/(.*)\.json$", r"=tests/render/inputs/\1.yaml", spec)


TWINS = [e for e in MANIFEST if e.get("inputs") and [twin(s) for s in e["inputs"]] != e["inputs"]]


@pytest.mark.parametrize("entry", TWINS, ids=[e["golden"] for e in TWINS])
def test_yaml_twins(entry: dict[str, Any], python: str, tmp_path: Path) -> None:
    r = run(python, args_of(entry, tmp_path, [twin(s) for s in entry["inputs"]]))
    expected = (ROOT / entry["golden"]).read_text(encoding="utf-8")
    rejected = entry.get("rejected", False)
    assert r.returncode == (1 if rejected else 0), r.stderr
    assert (r.stderr if rejected else r.stdout) == expected


@pytest.mark.parametrize("case", INVALID, ids=[c["file"] for c in INVALID])
def test_invalid_documents(case: dict[str, Any], python: str) -> None:
    file = f"tests/render/invalid/{case['file']}"
    with pytest.raises(YamlError) as info:
        read_yaml((ROOT / file).read_text(encoding="utf-8"))
    assert str(info.value) == case["message"]
    r = run(python, ["validate", "tests/render/invalid/doc.decl", "--input", f"doc={file}"])
    want = (
        "tests/render/invalid/doc.decl: error [E6004] at doc: "
        f"bound document is not well-formed YAML: {file}: {case['message']}\n"
    )
    assert r.returncode == 1 and r.stderr == want
