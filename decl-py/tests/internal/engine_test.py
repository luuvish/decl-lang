"""engine (tests/internal/checks.json): the engine's boundary through the
single-module pipeline — quantities, references, $referrers, a cycle."""

from __future__ import annotations

import json
from pathlib import Path

from decl.parse import parse_source
from decl.pipeline import run_pipeline
from decl.qengine.qeval import _eval_rounds
from decl.semantics import Env, Quantity


def test_values() -> None:
    q = run_pipeline(
        parse_source(
            "dimension Speed = Length / Time\nunit mps: Speed\n"
            "output v: quantity<Speed> = 3km / 2s\n"
        )["decls"]
    )
    assert q["diags"] == []
    v = q["eng"].resolve_segs(["v"])
    assert isinstance(v, Quantity) and v.dim == "Length*Time^-1" and v.value == 1500.0
    r = run_pipeline(
        parse_source(
            'type S = { name: string, inbound = $referrers(L, "target") }\n'
            "type L = { source: ref<S>, target: ref<S> }\n"
            "type Top = { services: S[], links: L[] }\n"
            'export output top: Top = { services: [{ name: "a" }, { name: "b" }], '
            "links: [{ source: services[0], target: services[1] }] }\n"
        )["decls"]
    )
    assert r["diags"] == []
    ser = r["eng"].serialize(r["env"].roots["top"], "top")
    assert '"source":"$.services[0]"' in ser
    assert '"inbound":["$.links[0]"]' in ser


def test_cycle() -> None:
    p = run_pipeline(parse_source("type T = { a = b, b = a }\nexport output t: T = {}\n")["decls"])
    assert any(d.get("code") == "E5007" for d in p["diags"]), p["diags"]


def test_reference_round_reuse(root: Path) -> None:
    for row in json.loads((root / "tests/internal/rounds.json").read_text()):
        decls = parse_source((root / row["file"]).read_text())["decls"]
        ref = run_pipeline(decls)
        env = Env()
        env.load(decls)
        roots = [(o["name"], o["type"], o["expr"], env) for o in env.outputs]
        rep, eng = _eval_rounds(env, roots, [], [])
        ok = not any(d["severity"] == "error" for d in ref["diags"])
        outputs = (
            [
                {
                    "name": o["name"],
                    "json": ref["eng"].serialize(ref["env"].roots[o["name"]], o["name"]),
                }
                for o in ref["env"].outputs
                if o["name"] in ref["env"].roots
            ]
            if ok
            else []
        )
        assert (rep.ok, rep.outputs, rep.diagnostics) == (ok, outputs, ref["diags"]), row["file"]
        if row["reuse"]:
            assert eng.round_cache is not None
            assert eng.round_cache.reused_rounds >= 3
            assert eng.round_cache.retained_records >= 20
