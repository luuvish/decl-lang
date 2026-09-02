"""End-to-end parity for the native Python runtime — the reference
implementation's impl/test/e2e.ts scenarios: benchmark round-trips
byte-identical under renamed roots, root-cause diagnostics on a
corrupted document, layered configs, and the guide module."""
from __future__ import annotations

import re
import sys
from pathlib import Path

from decl.runtime.engine import Engine
from decl.runtime.modules import sort_diags
from decl.runtime.parser import parse_source
from decl.runtime.semantics import ABSENT, ArrV, Env, JObj, Quantity, Scope, read_json

ROOT = Path(__file__).resolve().parent.parent.parent
passed = failed = 0


def check(name: str, cond: bool, detail: str = "") -> None:
    global passed, failed
    if cond:
        passed += 1
        print(f"  ok   {name}")
    else:
        failed += 1
        print(f"  FAIL {name} {detail}")


def pipeline(src: str, binds: list | None = None):
    parsed = parse_source(src)
    assert not parsed["errors"], "parse errors"
    env = Env()
    env.load(parsed["decls"])
    eng = Engine(env)
    for o in env.outputs:
        sc = Scope(None, {}, o["name"])
        try:
            env.roots[o["name"]] = eng.bind(eng.ev(o["expr"], sc), env.resolve(o["type"]), [o["name"]], None, sc)
        except Exception:
            pass
    for name, raw in (binds or []):
        decl = env.inputs[name]
        sc = Scope(None, {}, name)
        try:
            env.roots[name] = eng.bind(raw, env.resolve(decl["type"]), [name], None, sc)
        except Exception:
            pass
    for v in list(env.roots.values()):
        eng.force_all(v, False)
    eng.phase = 2
    i = 0
    while i < len(eng.deferred_slots):
        inst, n = eng.deferred_slots[i]
        eng.force_slot_safe(inst, n)
        i += 1
    for v in list(env.roots.values()):
        eng.force_all(v, True)
    eng.validate_all("")
    env.diagnostics[:] = sort_diags(env.diagnostics)
    return env, eng


def get(eng: Engine, segs: list):
    return eng.resolve_segs(segs)


print("== case 3: fixtures ==")
env, eng = pipeline((ROOT / "docs/examples/03_fixtures.decl").read_text())
check("no diagnostics", not env.diagnostics, str(env.diagnostics[:2]))
check("32 cases", isinstance(env.roots.get("sweep"), ArrV) and len(env.roots["sweep"].items) == 32)
check("16 packets each", get(eng, ["sweep", 0, "packet_count"]) == 16)
check("total_bytes", get(eng, ["sweep", 0, "total_bytes"]) == 1024)
check("derived label", get(eng, ["sweep", 0, "packets", 3, "label"]) == "pkt-3-p0")
check("repeated serialization byte-identical", eng.serialize(env.roots["sweep"], "sweep") == eng.serialize(env.roots["sweep"], "sweep"))

print("== case 2: config ==")
cfg_src = (ROOT / "docs/examples/02_config.decl").read_text()
env, eng = pipeline(cfg_src)
check("outputs clean", not env.diagnostics, str(env.diagnostics[:2]))
check("prod layering", get(eng, ["prod", "host"]) == "api.internal" and get(eng, ["prod", "workers"]) == 32)
check("derived recomputed after with", get(eng, ["prod", "insecure"]) is False)
env2, eng2 = pipeline(cfg_src, [("deployed", read_json('{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}'))])
errs = [d for d in env2.diagnostics if d["severity"] == "error"]
warns = [d for d in env2.diagnostics if d["severity"] == "warn"]
check("two errors, one warning", len(errs) == 2 and len(warns) == 1, str(env2.diagnostics))
check("cert assert id", any(d.get("id") == "TlsConfig.cert_present" for d in errs))
check("warned value preserved", get(eng2, ["deployed", "workers"]) == 100)

print("== case 1: interconnect ==")
ic_src = (ROOT / "docs/examples/01_interconnect.decl").read_text()
env, eng = pipeline(ic_src)
check("xbar clean", not env.diagnostics, str(env.diagnostics[:2]))
check("propagated master si", get(eng, ["xbar", "nodes", "dom0", "nodes", "mst0", "ports", "si", "width"]) == 64)
check("arbiter max", get(eng, ["xbar", "nodes", "dom0", "nodes", "arb0", "ports", "mi", "width"]) == 64)
ser = eng.serialize(env.roots["xbar"], "xbar")
check("relative ref paths", '"$.nodes.dom0.ports.si0"' in ser)
env2, eng2 = pipeline(ic_src, [("doc", read_json(ser))])
check("round-trip validates", not env2.diagnostics, str(env2.diagnostics[:2]))
check("round-trip byte-identical", eng2.serialize(env2.roots["doc"], "doc") == ser)
doc = read_json(ser)
ports = next(v for k, v in doc.entries if k == "ports")
mi1 = next(v for k, v in ports.entries if k == "mi1")
mi1.entries = [(k, 32 if k == "width" else v) for k, v in mi1.entries]
env3, _ = pipeline(ic_src, [("doc", doc)])
errs = [d for d in env3.diagnostics if d["severity"] == "error"]
check("one width_match at root cause", len(errs) == 1 and errs[0].get("id") == "Edge.width_match", str(errs))

print("== guide: end to end ==")
md = (ROOT / "docs/guide/01_overview_by_example.md").read_text()
guide_src = "\n".join(m.group(1) for m in re.finditer(r"```decl\n([\s\S]*?)```", md))
env, eng = pipeline(guide_src)
check("guide evaluates clean", not env.diagnostics, str(env.diagnostics[:3]))
check("guide endpoint derived", get(eng, ["demo", "services", 0, "endpoint"]) == "svc-0:9000")
check("guide defaults filled", get(eng, ["demo", "services", 0, "replicas"]) == 1)
check("guide quantity default", isinstance(get(eng, ["demo", "services", 0, "timeout"]), Quantity)
      and get(eng, ["demo", "services", 0, "timeout"]).value == 0.25)
check("guide service_count", get(eng, ["demo", "service_count"]) == 3)
inbound = get(eng, ["demo", "services", 1, "inbound"])
check("guide inbound via referrers", isinstance(inbound, ArrV) and len(inbound.items) == 2)
check("guide serialized refs relative", '"$.links[0]"' in eng.serialize(env.roots["demo"], "demo"))

print(f"\nTOTAL {passed} ok, {failed} failed")
sys.exit(1 if failed else 0)
