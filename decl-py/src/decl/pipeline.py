"""The single-module pipeline — a port of the reference implementation's
pipeline.ts: bind and evaluate every output of one module's declarations
(the judgment the conformance runner applies), and the source-level
report that front-ends and embedders consume."""

from __future__ import annotations

import os
from typing import Any

from .checker import check_module
from .engine import Engine
from .parse import parse_source
from .qengine.qeval import Unsupported, qevaluate
from .semantics import Env, Scope, sort_diags


def use_qengine() -> bool:
    """The incremental query engine may stand in for the tree walker
    (qengine/DESIGN.md): a drop-in, byte-identical evaluator, off unless
    DECL_QENGINE is set to a non-empty value — while it is validated across the
    whole corpus. A non-empty env value is truthy, as the reference reads it."""
    return bool(os.environ.get("DECL_QENGINE"))


def strict_qengine() -> bool:
    """strict mode: an unhandled form is a hard error, not a silent fall back to
    the tree walker — used to prove the query engine covers a corpus end to end"""
    return bool(os.environ.get("DECL_QENGINE_STRICT"))


def run_pipeline(decls: list[Any]) -> dict[str, Any]:
    """Returns {"env", "eng", "diags"}."""
    env = Env()
    env.load(decls)

    def bind(eng: Engine) -> None:
        for o in env.outputs:
            sc = Scope(None, {}, o["name"])
            eng.bind_root(o["name"], o["expr"], env.resolve(o["type"]), sc, True)

    eng = Engine.evaluate(env, bind)
    eng.validate_all("")
    env.diagnostics[:] = sort_diags(env.diagnostics)  # §6.7
    return {"env": env, "eng": eng, "diags": env.diagnostics}


def evaluate_source(source: str) -> dict[str, Any]:
    """Parse, check, and evaluate one module given as source text. Returns
    {"phase", "ok", "parse_errors", "checks", "diagnostics", "outputs", "inputs"}."""
    parsed = parse_source(source)
    decls, errors = parsed["decls"], parsed["errors"]
    inputs = [d["name"] for d in decls if d["d"] == "input"]
    if errors:
        return {
            "phase": "parse",
            "ok": False,
            "parse_errors": errors,
            "checks": [],
            "diagnostics": [],
            "outputs": [],
            "inputs": inputs,
        }
    checks = check_module(decls)
    if any(d["severity"] == "error" for d in checks):
        return {
            "phase": "check",
            "ok": False,
            "parse_errors": [],
            "checks": checks,
            "diagnostics": [],
            "outputs": [],
            "inputs": inputs,
        }
    # the query engine, when selected, evaluates in place of the tree walker;
    # an unhandled form falls back (a hard error under strict mode)
    if use_qengine():
        env2 = Env()
        env2.load(decls)
        try:
            qr = qevaluate(env2)
            return {
                "phase": "evaluate",
                "ok": qr.ok,
                "parse_errors": [],
                "checks": checks,
                "diagnostics": qr.diagnostics,
                "outputs": qr.outputs,
                "inputs": inputs,
            }
        except Unsupported as u:
            if strict_qengine():
                raise RuntimeError(f"DECL_QENGINE_STRICT: unhandled form: {u.what}") from None
            # else fall back to the tree walker
    r = run_pipeline(decls)
    env, eng, diags = r["env"], r["eng"], r["diags"]
    ok = not any(d["severity"] == "error" for d in diags)
    outputs = (
        [
            {"name": o["name"], "json": eng.serialize(env.roots[o["name"]], o["name"])}
            for o in env.outputs
            if o["name"] in env.roots
        ]
        if ok
        else []
    )
    return {
        "phase": "evaluate",
        "ok": ok,
        "parse_errors": [],
        "checks": checks,
        "diagnostics": diags,
        "outputs": outputs,
        "inputs": inputs,
    }
