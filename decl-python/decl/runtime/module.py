"""Module loading, linking, and universe evaluation — port of module.ts
(§8.1–8.5, §8.8): files are modules, the import graph is acyclic, exports
are explicit; packages (§8.6–8.7) plug in through the resolver hook."""
from __future__ import annotations

import os
from typing import Any, Optional

from .engine import Engine
from .parse import parse_source
from .semantics import Env, Scope


class Module:
    __slots__ = ("path", "decls", "env", "exports")

    def __init__(self, path: str, decls: list, env: Env):
        self.path, self.decls, self.env = path, decls, env
        self.exports: dict = {}


def load_modules(entry_path: str, source_override: Optional[dict] = None, resolve_package=None) -> dict:
    """Load the module graph from `entry_path`. `source_override` maps
    absolute paths to buffer contents (editors); `resolve_package(spec,
    from_dir)` maps a package specifier to a path or to a `{code, message}`
    diagnostic (packages, §8.6)."""
    diags: list = []

    def report(code: str, message: str) -> None:
        diags.append({"severity": "error", "code": code, "message": message, "path": ""})

    modules: dict = {}
    order: list = []
    visiting: list = []

    def resolve_spec(spec: str, from_dir: str) -> Optional[str]:
        if spec.startswith("./") or spec.startswith("../"):
            return os.path.normpath(os.path.join(from_dir, spec))
        if resolve_package is None:
            report("E3010", f'package import "{spec}" outside a package (no manifest)')
            return None
        r = resolve_package(spec, from_dir)
        if isinstance(r, str):
            return r
        report(r["code"], r["message"])
        return None

    def load(path: str) -> Optional[Module]:
        abs_ = os.path.abspath(path)
        if abs_ in modules:
            return modules[abs_]
        if abs_ in visiting:
            ci = visiting.index(abs_)
            report("E3007", "module import cycle: " + " -> ".join(visiting[ci:] + [abs_]))
            return None
        if source_override and abs_ in source_override:
            src = source_override[abs_]
        else:
            try:
                with open(abs_, encoding="utf-8") as f:
                    src = f.read()
            except OSError:
                report("E3004", f"module not found: {abs_}")
                return None
        parsed = parse_source(src)
        if parsed["errors"]:
            report("E2001", f"{abs_}: {len(parsed['errors'])} parse error(s)")
            return None
        decls = parsed["decls"]
        env = Env()
        env.load(decls)
        for n in env.duplicates:
            report("E3001", f"duplicate name {n} in {abs_}")
        mod = Module(abs_, decls, env)
        visiting.append(abs_)
        targets: dict = {}
        for d in decls:
            if d["d"] not in ("import", "re_export"):
                continue
            target = resolve_spec(d["from"], os.path.dirname(abs_))
            if target is None:
                continue
            tm = load(target)
            if tm is not None:
                targets[d["from"]] = tm
        visiting.pop()
        modules[abs_] = mod

        def taken(n: str) -> bool:
            e = mod.env
            return n in e.type_asts or n in e.consts or n in e.funcs or n in e.diags or n in e.inputs \
                or any(o["name"] == n for o in e.outputs) or n in e.imports or n in e.namespaces

        for d in decls:
            if d["d"] == "import":
                tm = targets.get(d["from"])
                if tm is None:
                    continue
                if d.get("ns") is not None:
                    if taken(d["ns"]):
                        report("E3006", f"import {d['ns']} collides with an existing binding in {abs_}")
                        continue
                    mod.env.namespaces[d["ns"]] = {"env": tm.env, "exports": tm.exports}
                    continue
                for it in d["names"]:
                    local = it.get("as") or it["name"]
                    ex = tm.exports.get(it["name"])
                    if ex is None:
                        report("E3005", f"{tm.path} does not export {it['name']}")
                        continue
                    if taken(local):
                        report("E3006", f"import {local} collides with an existing binding in {abs_}")
                        continue
                    mod.env.imports[local] = ex
            elif d["d"] == "re_export":
                tm = targets.get(d["from"])
                if tm is None:
                    continue
                for it in d["names"]:
                    ex = tm.exports.get(it["name"])
                    if ex is None:
                        report("E3005", f"{tm.path} does not export {it['name']}")
                        continue
                    mod.exports[it.get("as") or it["name"]] = ex
        for d in decls:
            name = d.get("name")
            if not d.get("exported") or not isinstance(name, str):
                continue
            if d["d"] in ("unit", "dimension", "import", "re_export"):
                continue
            mod.exports[name] = {"env": mod.env, "name": name}
        order.append(mod)
        return mod

    entry = load(entry_path)
    if entry is not None:
        _link_universe(order, entry, report)
    return {"modules": order, "entry": entry, "diags": diags}


def _link_universe(mods: list, entry: Module, report) -> None:
    root_owners: dict = {}
    for m in mods:
        for d in m.decls:
            if d["d"] in ("output", "input"):
                prev = root_owners.get(d["name"])
                if prev is not None and prev != m.path:
                    report("E3018", f"root {d['name']} declared in both {prev} and {m.path}")
                root_owners[d["name"]] = m.path
    for m in mods:
        for d in m.decls:
            if not d.get("exported"):
                continue
            if d["d"] == "dimension":
                for m2 in mods:
                    if m2 is m:
                        continue
                    if d["name"] in m2.env.dim_decls and not any(x["d"] == "dimension" and x["name"] == d["name"] for x in m2.decls):
                        continue
                    if d["name"] in m2.env.dim_decls:
                        report("E3001", f"dimension {d['name']} redeclared across modules")
                    else:
                        m2.env.dim_decls[d["name"]] = {"terms": d.get("terms")}
            elif d["d"] == "unit":
                for m2 in mods:
                    if m2 is m:
                        continue
                    if d["name"] in m2.env.unit_decls and not any(x["d"] == "unit" and x["name"] == d["name"] for x in m2.decls):
                        continue
                    if d["name"] in m2.env.unit_decls:
                        report("E4073", f"unit {d['name']} redeclared across modules")
                    else:
                        m2.env.unit_decls[d["name"]] = {"dim": d.get("dim"), "factor": d.get("factor"), "base": d.get("base")}
    for m in mods:
        if m is entry:
            continue
        m.env.registry = entry.env.registry
        m.env.roots = entry.env.roots
        m.env.diagnostics = entry.env.diagnostics


def run_universe(mods: list, entry: Module, binds: Optional[list] = None) -> dict:
    eng = Engine(entry.env)
    for m in mods:
        menv = m.env
        menv.const_eval = (lambda e_: (lambda n: eng.force_const_in(e_, n, "")))(menv)
        menv.expr_eval = (lambda e_: (lambda x: eng.ev(x, Scope(None, {}, "", e_))))(menv)
    # bound documents first: an output may read an input (§5.5), and a
    # bound input is a root of the universe (§9.2); unbound inputs with a
    # fallback bind on first demand (§9.4)
    for b in (binds or []):
        m = b.get("module") or entry
        decl = m.env.inputs[b["input"]]
        sc = Scope(None, {}, b["input"], m.env)
        eng.bind_root(b["input"], b["raw"], m.env.resolve(decl["type"]), sc, False)
    for m in mods:
        for o in m.env.outputs:
            sc = Scope(None, {}, o["name"], m.env)
            eng.bind_root(o["name"], o["expr"], m.env.resolve(o["type"]), sc, True)
    for v in list(entry.env.roots.values()):
        eng.force_all(v, False)
    eng.phase = 2
    i = 0
    while i < len(eng.deferred_slots):
        inst, name = eng.deferred_slots[i]
        eng.force_slot_safe(inst, name)
        i += 1
    for v in list(entry.env.roots.values()):
        eng.force_all(v, True)
    eng.validate_all("")
    return {"eng": eng, "diags": entry.env.diagnostics}


def sort_diags(diags: list) -> list:
    return sorted(diags, key=lambda d: (d.get("path", ""), d.get("id") or ""))
