"""The single-module pipeline — a port of the reference implementation's
pipeline.ts: bind and evaluate every output of one module's declarations
(the judgment the conformance runner applies), and the source-level
report that front-ends and embedders consume."""
from __future__ import annotations

from .checker import check_module
from .engine import Engine
from .parse import parse_source
from .semantics import sort_diags, Env, Scope


def run_pipeline(decls: list) -> dict:
    """Returns {"env", "eng", "diags"}."""
    env = Env()
    env.load(decls)
    eng = Engine(env)
    for o in env.outputs:
        sc = Scope(None, {}, o["name"])
        eng.bind_root(o["name"], o["expr"], env.resolve(o["type"]), sc, True)
    eng.force_all_roots(False)
    eng.phase = 2
    i = 0
    while i < len(eng.deferred_slots):
        inst, name = eng.deferred_slots[i]
        eng.force_slot_safe(inst, name)
        i += 1
    eng.bind_deferred_roots()
    eng.force_all_roots(True)
    eng.validate_all("")
    env.diagnostics[:] = sort_diags(env.diagnostics)   # §6.7
    return {"env": env, "eng": eng, "diags": env.diagnostics}


def evaluate_source(source: str) -> dict:
    """Parse, check, and evaluate one module given as source text. Returns
    {"phase", "ok", "parse_errors", "checks", "diagnostics", "outputs", "inputs"}."""
    parsed = parse_source(source)
    decls, errors = parsed["decls"], parsed["errors"]
    inputs = [d["name"] for d in decls if d["d"] == "input"]
    if errors:
        return {"phase": "parse", "ok": False, "parse_errors": errors, "checks": [], "diagnostics": [], "outputs": [], "inputs": inputs}
    checks = check_module(decls)
    if any(d["severity"] == "error" for d in checks):
        return {"phase": "check", "ok": False, "parse_errors": [], "checks": checks, "diagnostics": [], "outputs": [], "inputs": inputs}
    r = run_pipeline(decls)
    env, eng, diags = r["env"], r["eng"], r["diags"]
    ok = not any(d["severity"] == "error" for d in diags)
    outputs = [{"name": o["name"], "json": eng.serialize(env.roots[o["name"]], o["name"])}
               for o in env.outputs if o["name"] in env.roots] if ok else []
    return {"phase": "evaluate", "ok": ok, "parse_errors": [], "checks": checks, "diagnostics": diags, "outputs": outputs, "inputs": inputs}
