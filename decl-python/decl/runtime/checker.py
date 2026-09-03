"""Static checks over the AST + resolved types — a port of the reference
implementation's checker.ts (chapters 3–4). Implemented:
  E3001 duplicate module name         E3003 unknown type name
  E4010 mixed range endpoints         E4011 empty range / array size
  E4012 structurally empty intersection
  E4013 non-discriminable record union arms
  E4014 more than one non-record object arm in a union
  E4015 map key not string-shaped     E4030 inheritance widening
  E4032 illegal member-kind transition
  E4052 ?? mixed with &&/|| unparenthesized
  E4094 context variable without / with an invalid context declaration
plus the expression pass of infer.py (inference, assignability, absence)."""
from __future__ import annotations

import json
import re
from typing import Any, Optional

from .engine import Engine
from .infer import (
    Ctx, TY, apply_guards, check_expr, guards_of, infer, js_str, js_typeof, make_ctx, require_val, try_resolve,
)
from .semantics import Env, is_bool, is_str
from .subsume import structurally_empty, subsumes


def _named(name: str) -> dict:
    return {"k": "named", "name": name, "args": [], "preds": None, "ext": None}


def _str_shaped(k: dict) -> bool:
    t = k["t"]
    return (t == "prim" and k["name"] == "string") or t == "pattern" \
        or (t == "lit" and is_str(k["v"])) \
        or (t == "union" and all(_str_shaped(a) for a in k["arms"])) \
        or (t == "pred" and _str_shaped(k["base"]))


def check_module(decls: list, linked: Optional[Env] = None) -> list:
    out: list = []

    def report(code: str, message: str) -> None:
        out.append({"code": code, "message": message, "severity": "error", "path": ""})

    env = linked if linked is not None else Env()
    if linked is None:
        env.load(decls)
    if env.const_eval is None:
        Engine(env)   # installs env.const_eval / env.expr_eval (§4.13, §3.16)
    env.on_const_diag = lambda d: out.append(d)   # constant-evaluation errors surface here
    for n in env.duplicates:
        report("E3001", f"duplicate name {n} in module")
    out.extend(env.finalize_unit_space())   # §3.16 unit/dimension spaces

    # ---------- §4.13: constant positions ----------
    state = {"tparams": set()}

    def check_endpoint(v: Any, where: str) -> None:
        if not is_str(v) or v in state["tparams"] or "." in v:
            return
        if v in env.inputs or any(o["name"] == v for o in env.outputs):
            report("E4021", f"non-constant {where}: {v} is an input/output, not a module const")
        elif v not in env.consts:
            report("E3003", f"unknown name {v} in a {where}")

    def const_violation(e: Any) -> Optional[str]:
        if isinstance(e, list):
            for x in e:
                r = const_violation(x)
                if r:
                    return r
            return None
        if not isinstance(e, dict):
            return None
        k = e.get("e")
        if k == "ctx":
            return f"context variable {e['name']}"
        if k == "referrers":
            return "$referrers"
        if k == "name" and e["name"] in env.inputs:
            return f"input {e['name']}"
        if k == "name" and any(o["name"] == e["name"] for o in env.outputs):
            return f"output {e['name']}"
        for v in e.values():
            if isinstance(v, (list, dict)):
                r = const_violation(v)
                if r:
                    return r
        return None

    # ---------- AST-level walks ----------
    def walk_type(t: Optional[dict], depth: int, decl_name: Optional[str] = None) -> None:
        if not t:
            return
        k = t["k"]
        if k == "range":
            kinds = [js_typeof(t["lo"]), js_typeof(t["hi"])]
            if kinds[0] != kinds[1] and "string" not in kinds:
                report("E4010", f"mixed range endpoints: {js_str(t['lo'])}..{js_str(t['hi'])}")
            check_endpoint(t["lo"], "range endpoint")
            check_endpoint(t["hi"], "range endpoint")
        elif k == "record":
            check_record_ctx(t, depth, decl_name)
            for m in t["members"]:
                walk_member(m, depth, decl_name)
        elif k == "map":
            walk_type(t["key"], depth, decl_name)
            walk_type(t["val"], depth, decl_name)
        elif k == "array":
            check_endpoint(t.get("lo"), "array size")
            check_endpoint(t.get("hi"), "array size")
            walk_type(t["elem"], depth, decl_name)
        elif k in ("union", "isect"):
            for a in t["arms"]:
                walk_type(a, depth, decl_name)
        elif k == "func":
            for p in t["params"]:
                walk_type(p, depth, decl_name)
            walk_type(t["ret"], depth, decl_name)
        elif k == "named":
            for a in t["args"]:
                walk_type(a, depth, decl_name)
            if t.get("ext"):
                check_extension(t, decl_name)
                walk_type(t["ext"], depth + 1, decl_name)
            for p in t.get("preds") or []:
                bad = const_violation(p)
                if bad:
                    report("E4021", f"non-constant predicate argument: {bad} (§4.13)")
                walk_expr(p)

    def walk_member(m: dict, depth: int, decl_name: Optional[str]) -> None:
        k = m["m"]
        if k == "value":
            walk_type(m["type"], depth + 1, decl_name)
            if m.get("dflt"):
                walk_expr(m["dflt"])
        elif k == "derived":
            walk_type(m.get("type"), depth + 1, decl_name)
            walk_expr(m["expr"])
        elif k == "context":
            walk_type(m["type"], depth + 1, decl_name)
        elif k == "assert":
            walk_expr(m["cond"])
        elif k == "when":
            walk_expr(m["cond"])
            for x in m["body"]:
                walk_member(x, depth, decl_name)

    def is_bool_op(e: Any) -> bool:
        return isinstance(e, dict) and e.get("e") == "bin" and e["op"] in ("&&", "||")

    def walk_expr(e: Any) -> None:
        if isinstance(e, list):
            for x in e:
                walk_expr(x)
            return
        if not isinstance(e, dict):
            return
        if e.get("e") == "bin" and e["op"] == "??" and (is_bool_op(e["l"]) or is_bool_op(e["r"])):
            report("E4052", "`??` mixed with `&&`/`||` without parentheses")
        for v in e.values():
            if isinstance(v, (list, dict)):
                walk_expr(v)

    # ---------- D30: context obligations ----------
    def ctx_uses(m: dict) -> dict:
        used: dict = {}

        def scan(e: Any) -> None:
            if isinstance(e, list):
                for x in e:
                    scan(x)
                return
            if not isinstance(e, dict):
                return
            if e.get("e") == "ctx" and e["name"] in ("$parent", "$root", "$key"):
                used[e["name"]] = True
            for v in e.values():
                if isinstance(v, list):
                    scan(v)
                elif isinstance(v, dict) and not v.get("k"):
                    scan(v)
        k = m["m"]
        if k == "value" and m.get("dflt"):
            scan(m["dflt"])
        if k == "derived":
            scan(m["expr"])
        if k == "assert":
            scan(m["cond"])
        if k == "when":
            scan(m["cond"])
            for b in m["body"]:
                used.update(ctx_uses(b))
        return used

    def check_record_ctx(rec: dict, depth: int, decl_name: Optional[str]) -> None:
        declared = {m["variable"]: m["type"] for m in rec["members"] if m["m"] == "context"}
        for v, ty in declared.items():
            if v in ("$parent", "$root"):
                is_ref = ty["k"] == "named" and ty["name"] == "ref"
                if not is_ref:
                    report("E4094", f"{v} declaration must be ref<...> ({decl_name or 'anonymous'})")
            if v == "$key" and ty["k"] == "named" and ty["name"] == "ref":
                report("E4094", "$key declares a plain value type, not ref<...>")
        if depth > 1:
            return   # lexically nested: parent evident, no declaration required
        used: dict = {}
        for m in rec["members"]:
            used.update(ctx_uses(m))
        for u in used:
            if u not in declared:
                report("E4094", f"{u} used without a context declaration in {decl_name or 'anonymous type'}")

    # ---------- inheritance (extension) ----------
    def check_extension(t: dict, decl_name: Optional[str]) -> None:
        try:
            base = env.resolve({"k": "named", "name": t["name"], "args": t["args"], "preds": None, "ext": None})
        except Exception:
            return   # unknown base reported by the resolution pass
        if base["t"] != "rec":
            report("E4031", f"extending non-record type {t['name']}")
            return
        for om in t["ext"]["members"]:
            if om["m"] in ("assert", "when", "context"):
                continue
            bm = next((x for x in base["members"] if x["name"] == om["name"]), None)
            if bm is None:
                continue   # addition
            o_kind = "der" if om["m"] == "derived" else "dflt" if om.get("dflt") else "opt" if om.get("opt") else "req"
            allowed = {"req": ["req", "dflt", "der"], "opt": ["req", "opt", "dflt", "der"],
                       "dflt": ["req", "dflt", "der"], "der": ["der"]}
            if o_kind not in allowed.get(bm["kind"], []):
                report("E4032", f"illegal member-kind transition for {om['name']}: {bm['kind']} -> {o_kind} ({decl_name or t['name']})")
                continue
            o_type = try_resolve(env, om["type"]) if om.get("type") else None
            if o_type and bm.get("type") and not subsumes(env, o_type, bm["type"]):
                report("E4030", f"override widens inherited member {om['name']} ({decl_name or t['name']})")

    # ---------- resolution-level checks ----------
    resolve_reported: set = set()

    def map_resolve_err(msg: str, where: str) -> None:
        key = f"{msg}|{where}"
        if key in resolve_reported:
            return   # one resolution failure, one report
        resolve_reported.add(key)
        if re.search(r"unknown dimension|circular dimension", msg):
            report("E3003", f"{msg} (in {where})")
        elif re.search(r"unknown unit", msg):
            report("E4073", f"{msg} (in {where})")
        elif re.search(r"pattern interpolation of .*: unknown type", msg):
            report("E3003", f"{msg} (in {where})")
        elif re.search(r"unknown type", msg):
            report("E3003", f"{msg} (in {where})")
        elif re.search(r"generic arity", msg):
            report("E4022", f"{msg} (in {where})")
        elif re.search(r"outside parameter", msg):
            report("E4023", f"{msg} (in {where})")
        elif re.search(r"non-constant value argument", msg):
            report("E4021", f"{msg} (in {where})")
        elif re.search(r"pattern interpolation", msg):
            report("E4117", f"{msg} (in {where})")
        elif re.search(r"malformed pattern", msg):
            report("E4119", f"{msg} (in {where})")
        else:
            report("E4001", f"{msg} (in {where})")   # never drop a resolution failure silently

    def resolve_or_report(t: Optional[dict], where: str) -> Optional[dict]:
        if not t:
            return None
        try:
            return env.resolve(t)
        except Exception as e:
            map_resolve_err(str(e), where)
            return None

    def check_resolved(rt: Optional[dict], name: str, seen: set) -> None:
        if not rt or id(rt) in seen:
            return
        seen.add(id(rt))
        t = rt["t"]
        if t == "range":
            ks = [js_typeof(rt["lo"]), js_typeof(rt["hi"])]
            if "string" not in ks and ks[0] != ks[1]:
                report("E4010", f"mixed range endpoints after constant substitution in {name}")
            if structurally_empty(env, rt):
                report("E4011", f"empty range in {name}")
        elif t == "arr":
            if structurally_empty(env, rt):
                report("E4011", f"empty array size in {name}")
            check_resolved(rt["elem"], name, seen)
        elif t == "isectN":
            if structurally_empty(env, rt):
                report("E4012", f"structurally empty intersection in {name}")
            for a in rt["arms"]:
                check_resolved(a, name, seen)
        elif t == "map":
            if not _str_shaped(rt["key"]):
                report("E4015", f"map key type not string-shaped in {name}")
            check_resolved(rt["val"], name, seen)
        elif t == "union":
            recs = [a for a in rt["arms"] if a["t"] == "rec"]
            if len(recs) >= 2:
                disc = [m for m in recs[0]["members"]
                        if m.get("type") and m["type"]["t"] == "lit"
                        and all(any(x["name"] == m["name"] and x.get("type") and x["type"]["t"] == "lit" for x in r["members"]) for r in recs)]
                tuples = set()
                for r in recs:
                    tuples.add(json.dumps([js_str(next(x for x in r["members"] if x["name"] == d["name"])["type"]["v"]) for d in disc]))
                if not disc or len(tuples) != len(recs):
                    report("E4013", f"record union arms not discriminable in {name}")
            non_rec_obj = [a for a in rt["arms"] if a["t"] in ("map", "quantity")]
            if len(non_rec_obj) > 1:
                report("E4014", f"more than one non-record object arm in {name}")
            for a in rt["arms"]:
                check_resolved(a, name, seen)
        elif t == "rec":
            for m in rt["members"]:
                if m.get("type"):
                    check_resolved(m["type"], name, seen)
        elif t == "pred":
            check_resolved(rt["base"], name, seen)
        elif t == "ref":
            check_resolved(rt["target"], name, seen)

    for name, decl in list(env.type_asts.items()):
        if decl.get("params"):
            continue   # generic declarations check at instantiation (§3.15)
        try:
            rt = env.resolve(_named(name))
        except Exception as e:
            map_resolve_err(str(e), name)
            continue
        check_resolved(rt, name, set())

    # AST walks over all declarations
    for d in decls:
        state["tparams"] = set(p["name"] for p in (d.get("params") or [])) if d["d"] == "type" else set()
        k = d["d"]
        if k == "type":
            walk_type(d["type"], 1, d["name"])
        elif k == "const":
            walk_type(d.get("type"), 0)
            walk_expr(d["expr"])
        elif k == "func":
            for p in d["params"]:
                walk_type(p.get("type"), 0)
            walk_type(d.get("ret"), 0)
            walk_expr(d["body"])
        elif k == "output":
            walk_type(d["type"], 0)
            walk_expr(d["expr"])
        elif k == "input":
            walk_type(d["type"], 0)
            if d.get("fallback"):
                walk_expr(d["fallback"])

    # ---------- expression pass: inference, assignability, absence (§3.18, §4.10) ----------
    cx0 = make_ctx(env, report)

    def is_bool_ty(t: dict) -> bool:
        rt = t["rt"]
        return not rt or (rt["t"] == "prim" and rt["name"] == "bool") or (rt["t"] == "lit" and is_bool(rt["v"]))

    def rec_ctx(cx: Ctx, rt: dict, ast: Optional[dict]) -> Ctx:
        vars_ = dict(cx.vars)
        for m in rt["members"]:
            mt = {"t": "isectN", "arms": m["conj"]} if m.get("conj") else m.get("type")
            vars_[m["name"]] = TY(mt, m["kind"] == "opt")
        vars_["$this"] = TY(rt)
        vars_["$path"] = TY({"t": "prim", "name": "string"})
        if ast and ast["k"] == "record":
            for m in ast["members"]:
                if m["m"] == "context":
                    vars_[m["variable"]] = TY(try_resolve(env, m["type"]))
        return Ctx(cx.env, cx.report, vars_, set(cx.present), set(cx.nonnull), cx.const_memo)

    def check_member_ast(cx: Ctx, m: dict) -> None:
        k = m["m"]
        if k == "value" and m.get("dflt"):
            check_expr(cx, m["dflt"], try_resolve(env, m["type"]))
        elif k == "derived":
            check_expr(cx, m["expr"], try_resolve(env, m.get("type")))
        elif k == "assert":
            if not is_bool_ty(require_val(cx, m["cond"], infer(cx, m["cond"]), "as an assert condition")):
                report("E4001", "assert condition is not bool")
            tail = m.get("tail")
            if tail and tail["t"] == "inline":
                for p in tail["template"]:
                    if not is_str(p):
                        infer(cx, p)
            if tail and tail["t"] == "ref":
                for a in tail["args"]:
                    require_val(cx, a, infer(cx, a), "as a diagnostic argument")
        elif k == "when":
            if not is_bool_ty(require_val(cx, m["cond"], infer(cx, m["cond"]), "as a when condition")):
                report("E4001", "when condition is not bool")
            c2 = apply_guards(cx, guards_of(m["cond"], True))
            for b in m["body"]:
                check_member_ast(c2, b)

    seen_recs: set = set()

    def check_record_exprs(rt: dict, cx: Ctx, ast: Optional[dict] = None) -> None:
        if rt["t"] != "rec" or id(rt) in seen_recs:
            return
        seen_recs.add(id(rt))
        cx_r = rec_ctx(cx, rt, ast)

        # member expressions and asserts check in their declaring module's
        # scope (§8.3) — same rule the engine follows at evaluation
        def cx_for(menv) -> Ctx:
            return cx_r.with_env(menv) if (menv is not None and menv is not cx_r.env) else cx_r

        # D30/E4090: an embedded type's declared bounds must hold at this
        # site — the container is the parent, the collection's key or index
        # type is what $key ranges over (none for a direct member)
        def check_embedding(member_rt: dict, member_name: str, key_rt: Optional[dict]) -> None:
            site = f"{rt.get('name') or 'record'}.{member_name}"
            who = member_rt.get("name") or "the member type"
            for cd in member_rt.get("ctx_decls") or []:
                if cd["variable"] == "$parent":
                    bound = cd["type"]["target"] if (cd.get("type") and cd["type"]["t"] == "ref") else None
                    if bound and not subsumes(env, rt, bound):
                        report("E4090", f"embedding site {site} fails {who}'s $parent bound (§7.3)")
                elif cd["variable"] == "$key":
                    if not key_rt:
                        report("E4090", f"embedding site {site} gives $key no meaning: {who} is a direct member, not a collection element (§7.3)")
                    elif not subsumes(env, key_rt, cd["type"]):
                        report("E4090", f"embedding site {site} fails {who}'s $key bound (§7.3)")

        INT = {"t": "prim", "name": "int"}
        for m in rt["members"]:
            mt = m.get("type")
            if m["kind"] == "der" and m.get("expr"):
                check_expr(cx_for(m.get("menv")), m["expr"], mt)
            if m["kind"] == "dflt" and m.get("dflt"):
                check_expr(cx_for(m.get("menv")), m["dflt"], mt)
            if mt and mt["t"] == "rec":
                check_embedding(mt, m["name"], None)
                check_record_exprs(mt, cx_for(m.get("menv")))
            if mt and mt["t"] == "arr" and mt.get("elem") and mt["elem"]["t"] == "rec":
                check_embedding(mt["elem"], m["name"], INT)
                check_record_exprs(mt["elem"], cx_for(m.get("menv")))
            if mt and mt["t"] == "map" and mt.get("val") and mt["val"]["t"] == "rec":
                check_embedding(mt["val"], m["name"], mt["key"])
                check_record_exprs(mt["val"], cx_for(m.get("menv")))
        for a in rt["asserts"]:
            if a["kind"] == "assert":
                check_member_ast(cx_for(a.get("menv")), {"m": "assert", "name": a["name"], "cond": a["cond"], "tail": a.get("tail")})
            elif a["kind"] == "when":
                check_member_ast(cx_for(a.get("menv")), {"m": "when", "cond": a["cond"], "body": a["body"]})

    for name, decl in list(env.type_asts.items()):
        if decl.get("params"):
            continue
        try:
            rt = env.resolve(_named(name))
        except Exception:
            continue
        check_record_exprs(rt, cx0, decl["ast"])

    # D30/E4090 for $root: every record type owned (transitively) by an
    # evaluation root must have its declared $root bound met by the root's
    # own type — checked once per root declaration
    def check_root_bounds(root_name: str, root_rt: dict) -> None:
        seen: set = set()

        def walk(t: Optional[dict]) -> None:
            if not t or id(t) in seen:
                return
            seen.add(id(t))
            k = t["t"]
            if k == "rec":
                for cd in t.get("ctx_decls") or []:
                    if cd["variable"] != "$root":
                        continue
                    bound = cd["type"]["target"] if (cd.get("type") and cd["type"]["t"] == "ref") else None
                    if bound and not subsumes(env, root_rt, bound):
                        report("E4090", f"root {root_name} fails {t.get('name') or 'a member type'}'s $root bound (§7.3)")
                for m in t["members"]:
                    walk(m.get("type"))
            elif k == "arr":
                walk(t.get("elem"))
            elif k == "map":
                walk(t.get("val"))
            elif k in ("union", "isectN"):
                for a in t["arms"]:
                    walk(a)
            elif k == "pred":
                walk(t.get("base"))
        walk(root_rt)

    # §7.3: the root's own type gives $parent and $key no meaning — the root
    # has no owner and sits under no key — so a declaration of either on it
    # (directly, or on a union arm) is an error at the root
    def check_root_type(root_name: str, root_rt: dict) -> None:
        arms = root_rt["arms"] if root_rt["t"] == "union" else [root_rt]
        for t in arms:
            if t["t"] != "rec":
                continue
            who = t.get("name") or "its type"
            for cd in t.get("ctx_decls") or []:
                if cd["variable"] == "$parent":
                    report("E4090", f"root {root_name} gives $parent no meaning: {who} is the evaluation root's own type (§7.3)")
                elif cd["variable"] == "$key":
                    report("E4090", f"root {root_name} gives $key no meaning: {who} is the evaluation root's own type, not a collection element (§7.3)")

    for d in decls:
        if d["d"] not in ("output", "input"):
            continue
        rt = try_resolve(env, d["type"])
        if rt:
            check_root_type(d["name"], rt)
            check_root_bounds(d["name"], rt)
    for d in decls:
        k = d["d"]
        if k == "const":
            check_expr(cx0, d["expr"], resolve_or_report(d["type"], f"const {d['name']}") if d.get("type") else None)
        elif k == "func":
            cx_f = cx0.child()
            for p in d["params"]:
                cx_f.vars[p["name"]] = TY(resolve_or_report(p.get("type"), f"func {d['name']}"))
            check_expr(cx_f, d["body"], resolve_or_report(d["ret"], f"func {d['name']}") if d.get("ret") else None)
        elif k == "output":
            check_expr(cx0, d["expr"], resolve_or_report(d["type"], f"output {d['name']}"))
        elif k == "input" and d.get("fallback"):
            check_expr(cx0, d["fallback"], resolve_or_report(d["type"], f"input {d['name']}"))
        elif k == "input":
            resolve_or_report(d["type"], f"input {d['name']}")
        elif k == "diagnostic":
            cx_d = cx0.child()
            for p in d["params"]:
                cx_d.vars[p["name"]] = TY(try_resolve(env, p.get("type")))
            for p in d["template"]:
                if not is_str(p):
                    infer(cx_d, p)
        elif k == "unit" and d.get("factor"):
            bad = const_violation(d["factor"])
            if bad:
                report("E4021", f"non-constant unit factor for {d['name']}: {bad} (§3.16)")
    return out
