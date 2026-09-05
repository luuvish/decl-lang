"""engine (tests/internal/checks.json): the engine's boundary through the
single-module pipeline — quantities, references, $referrers, a cycle."""

from __future__ import annotations

from decl.parse import parse_source
from decl.pipeline import run_pipeline
from decl.semantics import Quantity


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
