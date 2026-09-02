"""Conformance judging — a port of the reference implementation's
conformance.ts: judges fixtures by their declared phase.
  valid/*                          -> parse + checks clean + outputs evaluate clean
  invalid @expect-phase: parsing   -> must fail to parse
  invalid @expect-phase: checking  -> parses; static checks report @expect-error
  invalid @expect-phase: binding   -> parses; the pipeline reports @expect-error"""
from __future__ import annotations

import json
import os
import re
from typing import Iterator

from .checker import check_module
from .parse import parse_source
from .pipeline import run_pipeline


def walk_decl(dir_: str) -> Iterator[str]:
    for e in sorted(os.listdir(dir_)):
        p = os.path.join(dir_, e)
        if os.path.isdir(p):
            yield from walk_decl(p)
        elif p.endswith(".decl"):
            yield p


def judge_fixture(file: str, is_valid: bool) -> dict:
    with open(file, encoding="utf-8") as f:
        src = f.read()
    meta = dict(m for m in re.findall(r"// @([a-z-]+):\s*(.+)", src))
    phase = meta.get("expect-phase")
    want = (meta.get("expect-error") or "").strip()
    want_msg = (meta.get("expect-message") or "").strip()
    parsed = parse_source(src)
    if is_valid:
        # a valid fixture must parse, check clean, AND evaluate its outputs
        # without error-severity diagnostics
        checks = check_module(parsed["decls"]) if not parsed["errors"] else []
        eval_errs = [d for d in run_pipeline(parsed["decls"])["diags"] if d["severity"] == "error"] \
            if (not parsed["errors"] and not checks) else []
        ok = not parsed["errors"] and not checks and not eval_errs
        detail = f"{len(parsed['errors'])} parse errors" if parsed["errors"] else json.dumps(checks + eval_errs, default=str)
        return {"file": file, "ok": ok, "detail": detail}
    if phase == "parsing":
        return {"file": file, "ok": bool(parsed["errors"]), "detail": "expected parse errors, got none"}
    if phase == "checking":
        checks = check_module(parsed["decls"]) if not parsed["errors"] else []
        ok = any(d.get("code") == want for d in checks) and (not want_msg or any(want_msg in d["message"] for d in checks))
        return {"file": file, "ok": ok, "detail": json.dumps(checks, default=str)}
    if phase == "binding":
        diags = run_pipeline(parsed["decls"])["diags"] if not parsed["errors"] else []
        ok = any(d.get("code") == want for d in diags) and (not want_msg or any(want_msg in d["message"] for d in diags))
        return {"file": file, "ok": ok, "detail": json.dumps(diags, default=str)}
    return {"file": file, "ok": False, "detail": f"unknown phase {phase}"}


def judge_corpus(dir_: str) -> list:
    return [judge_fixture(f, "/valid/" in f) for f in walk_decl(dir_)]
