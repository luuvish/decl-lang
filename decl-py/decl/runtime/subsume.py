"""The subsumption judgment ⊑ (spec §3.17) — port of subsume.ts. Used by
the runtime for `match` arm selection over bound records and for
generic value-argument checks."""
from __future__ import annotations

import re
from typing import Any, Optional

from .semantics import is_bool, is_float, is_int, is_str


def subsumes(env, a: dict, b: dict, assume: dict | None = None) -> bool:
    assume = assume if assume is not None else {}
    if a is b:
        return True
    if a["t"] == "rec" and b["t"] == "rec":
        s = assume.get(id(a))
        if s is not None and id(b) in s:
            return True
    if a["t"] == "union":
        return all(subsumes(env, x, b, assume) for x in a["arms"])
    if b["t"] == "union":
        return any(subsumes(env, a, x, assume) for x in b["arms"])
    if a["t"] == "isectN":
        return any(subsumes(env, x, b, assume) for x in a["arms"])
    if b["t"] == "isectN":
        return all(subsumes(env, a, x, assume) for x in b["arms"])
    if b["t"] == "pred":
        if a["t"] == "pred":
            return subsumes(env, a["base"], b["base"], assume) and \
                all(any(_pred_eq(p, q) for q in a["preds"]) for p in b["preds"])
        if a["t"] == "lit":
            return _lit_satisfies(env, a["v"], b)
        return False
    if a["t"] == "pred":
        return subsumes(env, a["base"], b, assume)
    bt = b["t"]
    if bt == "prim":
        if a["t"] == "prim":
            return a["name"] == b["name"]
        if a["t"] == "lit":
            return _lit_kind(a["v"]) == b["name"]
        if a["t"] == "range":
            return a["base"] == b["name"]
        if a["t"] == "pattern":
            return b["name"] == "string"
        return False
    if bt == "lit":
        return a["t"] == "lit" and _val_eq(a["v"], b["v"])
    if bt == "range":
        if a["t"] == "lit":
            return _lit_kind(a["v"]) == b["base"] and _in_range(a["v"], b)
        if a["t"] == "range":
            if a["base"] != b["base"]:
                return False
            a_hi = _dec(a["hi"]) if a["excl"] else a["hi"]
            b_hi = _dec(b["hi"]) if b["excl"] else b["hi"]
            try:
                return a["lo"] >= b["lo"] and a_hi <= b_hi
            except TypeError:
                return False
        return False
    if bt == "pattern":
        if a["t"] == "lit" and is_str(a["v"]):
            return re.fullmatch(f"(?:{b['src']})", a["v"]) is not None
        if a["t"] == "pattern":
            return a["src"] == b["src"]
        return False
    if bt == "arr":
        if a["t"] != "arr":
            return False
        if not subsumes(env, a["elem"], b["elem"], assume):
            return False
        a_lo, a_hi = a.get("lo") if a.get("lo") is not None else 0, a.get("hi") if a.get("hi") is not None else float("inf")
        b_lo, b_hi = b.get("lo") if b.get("lo") is not None else 0, b.get("hi") if b.get("hi") is not None else float("inf")
        try:
            return a_lo >= b_lo and a_hi <= b_hi
        except TypeError:
            return False
    if bt == "map":
        return a["t"] == "map" and subsumes(env, a["key"], b["key"], assume) and subsumes(env, a["val"], b["val"], assume)
    if bt == "quantity":
        return a["t"] == "quantity" and a["dim"] == b["dim"]
    if bt == "ref":
        return a["t"] == "ref" and subsumes(env, a["target"], b["target"], assume)
    if bt == "func":
        if a["t"] != "func" or len(a["params"]) != len(b["params"]):
            return False
        return all(subsumes(env, bp, ap, assume) for bp, ap in zip(b["params"], a["params"])) \
            and subsumes(env, a["ret"], b["ret"], assume)
    if bt == "rec":
        if a["t"] != "rec":
            return False
        s = assume.setdefault(id(a), set())
        s.add(id(b))
        for m in b["members"]:
            if m.get("hidden"):
                continue   # not part of the value: ⊑ never compares it (D34)
            sm = next((x for x in a["members"] if x["name"] == m["name"]), None)
            m_types = m.get("conj") or ([m["type"]] if m.get("type") else [])
            s_types = (sm.get("conj") or ([sm["type"]] if sm.get("type") else [])) if sm else []

            def type_ok():
                return not m_types or not s_types or \
                    all(any(subsumes(env, st, mt, assume) for st in s_types) for mt in m_types)
            k = m["kind"]
            if k == "req":
                if sm is None or sm["kind"] == "opt" or not type_ok():
                    s.discard(id(b))
                    return False
            elif k in ("opt", "dflt"):
                if sm is not None and not type_ok():
                    s.discard(id(b))
                    return False
            elif k == "der":
                if sm is None or not type_ok():
                    s.discard(id(b))
                    return False
        return True
    if bt == "any":
        return True
    return False


def _lit_kind(v: Any) -> str:
    if is_bool(v):
        return "bool"
    if is_int(v):
        return "int"
    if is_float(v):
        return "float"
    if is_str(v):
        return "string"
    if v is None:
        return "null"
    return "unknown"


def _val_eq(a: Any, b: Any) -> bool:
    return type(a) is type(b) and a == b


def _in_range(v: Any, r: dict) -> bool:
    hi = _dec(r["hi"]) if r["excl"] else r["hi"]
    try:
        return v >= r["lo"] and v <= hi
    except TypeError:
        return False


def _dec(v: Any) -> Any:
    return v - 1


def _pred_eq(a: Any, b: Any) -> bool:
    if a.get("e") == "name" and b.get("e") == "name":
        return a["name"] == b["name"]
    if a.get("e") == "call" and b.get("e") == "call":
        return _pred_eq(a["fn"], b["fn"]) and len(a["args"]) == len(b["args"]) and \
            all(x.get("e") == "lit" and y.get("e") == "lit" and _val_eq(x["v"], y["v"])
                for x, y in zip(a["args"], b["args"]))
    return False


def _lit_satisfies(env, v: Any, pred: dict) -> bool:
    from .engine import Engine
    from .semantics import Scope
    try:
        eng = Engine(env)
        sc = Scope(None, {}, "")
        if not subsumes(env, {"t": "lit", "v": v}, pred["base"]):
            return False
        for p in pred["preds"]:
            fn = eng.ev(p, sc)
            if eng.call(fn, [v], sc) is not True:
                return False
        return True
    except Exception:
        return False


# ---------------- structural emptiness (§3.17; the checker's E4011/E4012) ----------------
def _js_gt(a: Any, b: Any) -> bool:
    """JavaScript `>`: strings compare lexically, a string against a number is NaN (false)"""
    if is_str(a) and is_str(b):
        return a > b
    if is_str(a) or is_str(b):
        return False
    try:
        return a > b
    except TypeError:
        return False


def structurally_empty(env, t: dict) -> bool:
    tt = t["t"]
    if tt == "range":
        hi = _dec(t["hi"]) if (t["excl"] and not is_str(t["hi"])) else t["hi"]
        return _js_gt(t["lo"], hi)
    if tt == "arr":
        return t.get("lo") is not None and t.get("hi") is not None and _js_gt(t["lo"], t["hi"])
    if tt == "isectN":
        arms = t["arms"]
        for i in range(len(arms)):
            for j in range(i + 1, len(arms)):
                if _disjoint(env, arms[i], arms[j]):
                    return True
        return any(structurally_empty(env, a) for a in arms)
    if tt == "union":
        return all(structurally_empty(env, a) for a in t["arms"])
    return False


def _kind_of(t: dict) -> Optional[str]:
    tt = t["t"]
    if tt == "prim":
        return t["name"]
    if tt == "lit":
        return _lit_kind(t["v"])
    if tt == "range":
        return t["base"]
    if tt == "pattern":
        return "string"
    if tt == "arr":
        return "array"
    if tt in ("rec", "map"):
        return "object"
    if tt == "quantity":
        return "object"
    return None


def _disjoint(env, a: dict, b: dict) -> bool:
    ka, kb = _kind_of(a), _kind_of(b)
    if ka and kb and ka != kb:
        return True
    if a["t"] == "range" and b["t"] == "range" and a["base"] == b["base"]:
        a_hi = _dec(a["hi"]) if a["excl"] else a["hi"]
        b_hi = _dec(b["hi"]) if b["excl"] else b["hi"]
        return _js_gt(a["lo"], b_hi) or _js_gt(b["lo"], a_hi)
    if a["t"] == "lit" and b["t"] == "lit":
        return not _val_eq(a["v"], b["v"])
    if a["t"] == "lit" and b["t"] == "range":
        return not (_lit_kind(a["v"]) == b["base"] and _in_range(a["v"], b))
    if a["t"] == "range" and b["t"] == "lit":
        return _disjoint(env, b, a)
    return False
