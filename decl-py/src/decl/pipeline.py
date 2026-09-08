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

_FALSY = {"", "0", "false", "off", "no"}


def use_qengine() -> bool:
    """The incremental query engine (qengine/DESIGN.md) is the default evaluator,
    a drop-in, byte-identical replacement for the tree walker; opt out of it with
    DECL_QENGINE=0 (or false/off/no) to run the tree walker."""
    v = os.environ.get("DECL_QENGINE")
    return True if v is None else v.lower() not in _FALSY


def strict_qengine() -> bool:
    """strict mode: an unhandled form is a hard error, not a silent fall back to
    the tree walker — off unless DECL_QENGINE_STRICT is set, to prove coverage"""
    v = os.environ.get("DECL_QENGINE_STRICT")
    return v is not None and v.lower() not in _FALSY


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
