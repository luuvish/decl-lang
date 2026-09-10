"""engine (tests/internal/checks.json): the engine's boundary through the
single-module pipeline — quantities, references, $referrers, a cycle."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from decl.engine import Engine
from decl.parse import parse_source
from decl.pipeline import run_pipeline
from decl.qengine.qeval import BoundSpec, _eval_rounds
from decl.semantics import Env, Quantity, Scope, read_json, sort_diags


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
        if row.get("cutoff"):
            assert eng.revisions is not None
            assert eng.revisions.verified_cutoffs >= 3
            assert eng.revisions.value_cutoffs >= 3


def test_shared_programs(root: Path) -> None:
    for row in json.loads((root / "tests/internal/programs.json").read_text()):
        decls = parse_source(row["source"])["decls"]
        first = None
        for size in row["sizes"]:
            raw = read_json("[" + ",".join([row["element"]] * size) + "]")
            reference = Env()
            reference.load(decls)

            def bind(
                eng: Engine, name: str = row["input"], raw: Any = raw, reference: Env = reference
            ) -> None:
                eng.bind_root(
                    name,
                    raw,
                    reference.resolve(reference.inputs[name]["type"]),
                    Scope(None, {}, name),
                    False,
                )
                for o in reference.outputs:
                    eng.bind_root(
                        o["name"],
                        o["expr"],
                        reference.resolve(o["type"]),
                        Scope(None, {}, o["name"]),
                        True,
                    )

            tree = Engine.evaluate(reference, bind)
            tree.validate_all("")
            env = Env()
            env.load(decls)
            roots = [(o["name"], o["type"], o["expr"], env) for o in env.outputs]
            report, eng = _eval_rounds(env, roots, [BoundSpec(row["input"], raw, env)], [env])
            assert report.ok, report.diagnostics
            expected = [
                {"name": o["name"], "json": tree.serialize(reference.roots[o["name"]], o["name"])}
                for o in reference.outputs
            ]
            assert report.outputs == expected
            assert report.diagnostics == sort_diags(reference.diagnostics)
            assert len(env.registry) >= size
            assert eng.programs is not None
            counts = (eng.programs.compiled, eng.programs.compiled_schemas)
            assert all(n > 0 for n in counts)
            assert first is None or first == counts, (row["name"], size, first, counts)
            first = counts
