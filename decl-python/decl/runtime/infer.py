"""Expression-level static analysis — a port of the reference
implementation's infer.ts: type inference, assignability (§3.18, strict
S ⊑ T), the absence discipline (§4.10) with its two narrowing rules, and
the `match` static checks (§4.7). Inference is conservative: a form whose
type cannot be determined yields `unknown` (rt None) and suppresses
downstream judgments rather than guessing."""
from __future__ import annotations

import re
from typing import Any, Callable, Optional

from .semantics import (
    compile_pattern, is_bool, is_float, is_int, is_str, js_num_str, key_of_vec, pattern_error,
    vec_combine, vec_of_key,
)
from .subsume import subsumes


def PRIM(name: str) -> dict:
    return {"t": "prim", "name": name}


def TY(rt: Optional[dict], abs_: bool = False) -> dict:
    return {"rt": rt, "abs": abs_}


UNK: dict = {"rt": None, "abs": False}
BOOL: dict = {"rt": PRIM("bool"), "abs": False}


class Ctx:
    __slots__ = ("env", "report", "vars", "present", "nonnull", "const_memo")

    def __init__(self, env, report, vars_: dict, present: set, nonnull: set, const_memo: dict) -> None:
        self.env = env
        self.report = report
        self.vars = vars_
        self.present = present
        self.nonnull = nonnull
        self.const_memo = const_memo

    def child(self, vars_: Optional[dict] = None) -> "Ctx":
        return Ctx(self.env, self.report, dict(self.vars) if vars_ is None else vars_,
                   set(self.present), set(self.nonnull), self.const_memo)

    def with_env(self, env) -> "Ctx":
        return Ctx(env, self.report, self.vars, self.present, self.nonnull, self.const_memo)


def make_ctx(env, report: Callable[[str, str], None]) -> Ctx:
    return Ctx(env, report, {}, set(), set(), {})


# ---------------- JS-faithful helpers ----------------
def js_typeof(v: Any) -> str:
    if v is None:
        return "object"
    if is_bool(v):
        return "boolean"
    if is_int(v):
        return "bigint"
    if is_float(v):
        return "number"
    if is_str(v):
        return "string"
    return "object"


def js_str(v: Any) -> str:
    if v is None:
        return "null"
    if is_bool(v):
        return "true" if v else "false"
    if is_int(v):
        return str(v)
    if is_float(v):
        return js_num_str(v)
    return str(v)


# ---------------- type utilities ----------------
def _is_null_lit(t: dict) -> bool:
    return (t["t"] == "lit" and t["v"] is None) or (t["t"] == "prim" and t["name"] == "null")


def has_null(rt: Optional[dict]) -> bool:
    if not rt:
        return False
    if _is_null_lit(rt):
        return True
    if rt["t"] == "union":
        return any(has_null(a) for a in rt["arms"])
    return False


def strip_null(rt: dict) -> dict:
    if rt["t"] == "union":
        arms = [a for a in rt["arms"] if not _is_null_lit(a)]
        return arms[0] if len(arms) == 1 else {"t": "union", "arms": arms}
    return rt


def _same_rt(a: dict, b: dict) -> bool:
    if a is b:
        return True
    if a["t"] != b["t"]:
        return False
    if a["t"] == "prim":
        return a["name"] == b["name"]
    if a["t"] == "lit":
        return js_typeof(a["v"]) == js_typeof(b["v"]) and a["v"] == b["v"]
    return False


def mk_union(arms: list) -> Optional[dict]:
    if any(a is None for a in arms):
        return None
    flat: list = []
    for a in arms:
        if a["t"] == "union":
            flat.extend(a["arms"])
        else:
            flat.append(a)
    uniq: list = []
    for a in flat:
        if not any(_same_rt(a, b) for b in uniq):
            uniq.append(a)
    return uniq[0] if len(uniq) == 1 else {"t": "union", "arms": uniq}


def num_kind(rt: Optional[dict]) -> Optional[str]:
    if not rt:
        return None
    t = rt["t"]
    if t == "prim":
        return rt["name"] if rt["name"] in ("int", "float", "string", "bool") else None
    if t == "lit":
        v = rt["v"]
        if is_bool(v):
            return "bool"
        if is_int(v):
            return "int"
        if is_float(v):
            return "float"
        if is_str(v):
            return "string"
        return None
    if t == "range":
        return rt["base"]
    if t == "pattern":
        return "string"
    if t == "pred":
        return num_kind(rt["base"])
    if t == "quantity":
        return "quantity"
    if t == "union":
        ks = [num_kind(a) for a in rt["arms"]]
        return ks[0] if ks and all(k and k == ks[0] for k in ks) else None
    return None


def _is_boolish(rt: Optional[dict]) -> bool:
    return not rt or num_kind(rt) == "bool"


def _arm_of(rt: Optional[dict], t: str) -> Optional[dict]:
    """structural view: unwrap ref/pred and select the arm of an
    intersection that has the wanted shape (merged `&` members carry conj arms)"""
    if not rt:
        return None
    if rt["t"] == "ref" and t != "ref":
        return _arm_of(rt["target"], t)
    if rt["t"] == "pred":
        return _arm_of(rt["base"], t)
    if rt["t"] == t:
        return rt
    if rt["t"] == "isectN":
        for x in rt["arms"]:
            v = _arm_of(x, t)
            if v:
                return v
    if rt["t"] == "union":
        sub = [y for y in (_arm_of(x, t) for x in rt["arms"]) if y]
        if len(sub) == len(rt["arms"]) and sub:
            if t == "arr":
                return {"t": "arr", "elem": {"t": "union", "arms": [a["elem"] for a in sub]}}
            return sub[0]
    return None


# ---------------- navigation paths & narrowing ----------------
def path_key(e: dict) -> Optional[str]:
    k = e["e"]
    if k == "name":
        return e["name"]
    if k == "ctx":
        return e["name"]
    if k == "paren":
        return path_key(e["x"])
    if k == "member":
        if e.get("safe"):
            return None
        b = path_key(e["x"])
        return f"{b}.{e['name']}" if b else None
    if k == "index":
        b = path_key(e["x"])
        if not b:
            return None
        if e["i"]["e"] == "lit":
            return f"{b}[{js_str(e['i']['v'])}]"
        if e["i"]["e"] == "name":
            return f"{b}[{e['i']['name']}]"
        return None
    return None


def guards_of(e: dict, polarity: bool) -> dict:
    none = {"present": [], "nonnull": []}
    k = e["e"]
    if k == "paren":
        return guards_of(e["x"], polarity)
    if k == "un":
        return guards_of(e["x"], not polarity) if e["op"] == "!" else none
    if k == "bin":
        op = e["op"]
        if op == "&&" and polarity:
            return _merge(guards_of(e["l"], True), guards_of(e["r"], True))
        if op == "||" and not polarity:
            return _merge(guards_of(e["l"], False), guards_of(e["r"], False))
        if op == "in" and polarity:
            b = path_key(e["r"])
            if not b:
                return none
            l = e["l"]
            if l["e"] == "lit" and is_str(l["v"]):
                return {"present": [f"{b}.{l['v']}", f"{b}[{l['v']}]"], "nonnull": []}
            if l["e"] == "name":
                return {"present": [f"{b}[{l['name']}]"], "nonnull": []}
            return none
        null_side = e["r"] if (e["l"]["e"] == "lit" and e["l"]["v"] is None) else \
            e["l"] if (e["r"]["e"] == "lit" and e["r"]["v"] is None) else None
        if null_side is not None:
            p = path_key(null_side)
            if p and ((op == "!=" and polarity) or (op == "==" and not polarity)):
                return {"present": [], "nonnull": [p]}
        return none
    return none


def _name_bound(cx: Ctx, n: str) -> bool:
    """is a name already taken here? (locals or the module namespace — the
    no-shadowing rule E3019 spans both)"""
    env = cx.env
    return n in cx.vars or n in env.consts or n in env.funcs or n in env.type_asts or n in env.inputs \
        or any(o["name"] == n for o in env.outputs)


def _merge(a: dict, b: dict) -> dict:
    return {"present": a["present"] + b["present"], "nonnull": a["nonnull"] + b["nonnull"]}


def apply_guards(cx: Ctx, g: dict) -> Ctx:
    c2 = cx.child()
    c2.present.update(g["present"])
    c2.nonnull.update(g["nonnull"])
    return c2


# ---------------- stdlib signatures (arity + result) ----------------
STD: dict = {
    "array.count": (1, PRIM("int")),
    "array.all": (2, PRIM("bool")),
    "array.any": (2, PRIM("bool")),
    "array.filter": (2, None),
    "array.all_distinct": (1, PRIM("bool")),
    "array.sum": (1, None),
    "array.fold": (3, None),
    "map.keys": (1, {"t": "arr", "elem": PRIM("string")}),
    "map.values": (1, None),
    "string.length": (1, PRIM("int")),
    "string.of": (1, PRIM("string")),
    "string.join": (2, PRIM("string")),
    "string.starts_with": (2, PRIM("bool")),
    "string.ends_with": (2, PRIM("bool")),
    "string.contains": (2, PRIM("bool")),
    "string.split": (2, {"t": "arr", "elem": PRIM("string")}),
    "map.entries": (1, None),
    "ref.path": (1, PRIM("string")),
    "math.abs": (1, None),
    "math.min": (2, None),
    "math.max": (2, None),
    "math.clog2": (1, PRIM("int")),
    "math.floor": (1, PRIM("int")),
    "math.ceil": (1, PRIM("int")),
    "math.round": (1, PRIM("int")),
    "int.of": (1, PRIM("int")),
    "int.at_least": (1, {"t": "func", "params": [PRIM("int")], "ret": PRIM("bool")}),
    "int.at_most": (1, {"t": "func", "params": [PRIM("int")], "ret": PRIM("bool")}),
    "float.of": (1, PRIM("float")),
    "object.merge": (2, None),
}


def _std_path(e: dict) -> Optional[str]:
    if e["e"] == "member" and not e.get("safe"):
        b = _std_path(e["x"])
        if b is None:
            return None
        return f"{b}.{e['name']}" if b else e["name"]
    return "" if (e["e"] == "name" and e["name"] == "std") else None


# ---------------- the judgment ----------------
def try_resolve(env, ast: Optional[dict]) -> Optional[dict]:
    if not ast:
        return None
    try:
        return env.resolve(ast)
    except Exception:
        return None


def require_val(cx: Ctx, e: dict, ty: dict, what: str) -> dict:
    if ty["abs"]:
        k = path_key(e)
        if not k or k not in cx.present:
            cx.report("E4050", f"maybe-absent expression consumed {what} (use ?. / ?? or an `in` guard)")
    return ty


def _named(name: str) -> dict:
    return {"k": "named", "name": name, "args": [], "preds": None, "ext": None}


def infer(cx: Ctx, e: dict) -> dict:
    k = e["e"]
    if k == "lit":
        return TY({"t": "lit", "v": e["v"]})
    if k == "pattern":
        bad = pattern_error(e["re"])
        if bad:
            cx.report("E4119", f"malformed pattern /{e['re']}/: {bad}")
            return UNK
        return TY({"t": "pattern", "src": e["re"], "re": compile_pattern(e["re"])})
    if k == "unitlit":
        try:
            return TY({"t": "quantity", "dim": cx.env.unit_info(e["unit"])["key"]})
        except Exception as err:
            cx.report("E4073", str(err))
            return UNK
    if k == "template":
        for p in e["parts"]:
            if not is_str(p):
                require_val(cx, p, infer(cx, p), "in a template")
        return TY(PRIM("string"))
    if k == "name":
        name = e["name"]
        if name in cx.vars:
            return cx.vars[name]
        env = cx.env
        if name in env.consts:
            return _const_ty(cx, name)
        if name in env.funcs:
            return TY(_func_rt(cx, name))
        if name == "std":
            return UNK
        o = next((o for o in env.outputs if o["name"] == name), None)
        if o is not None:
            return TY(try_resolve(env, o["type"]))
        if name in env.inputs:
            return TY(try_resolve(env, env.inputs[name]["type"]))
        im = env.imports.get(name)
        if im is not None:
            return _imported_ty(cx, im)
        if name in env.namespaces:
            cx.report("E3008", f"namespace name {name} used as a value")
            return UNK
        if name in env.type_asts:
            cx.report("E3008", f"type/namespace name {name} used as a value")
            return UNK
        cx.report("E3003", f"unknown name {name}")
        return UNK
    if k == "ctx":
        return cx.vars.get(e["name"], UNK)
    if k == "referrers":
        rt = try_resolve(cx.env, _named(e["type"]))
        if not rt:
            cx.report("E4091", f"$referrers: unknown record type {e['type']}")
        elif rt["t"] != "rec":
            cx.report("E4091", f"$referrers: {e['type']} is not a record type")
        return TY({"t": "arr", "elem": {"t": "ref", "target": rt}} if rt and rt["t"] == "rec" else None)
    if k == "obj":
        for en in e["entries"]:
            require_val(cx, en["val"], infer(cx, en["val"]), "as a construction member")
        return UNK   # literals are typed by their checked position (§3.18)
    if k == "arr":
        ts = []
        for it in e["items"]:
            t = require_val(cx, it["expr"], infer(cx, it["expr"]), "as an array element")
            ts.append((t["rt"]["elem"] if (t["rt"] and t["rt"]["t"] == "arr") else None) if it["spread"] else t["rt"])
        elem = mk_union(ts)
        return TY({"t": "arr", "elem": elem} if elem else None)
    if k in ("comp", "mapcomp"):
        c2 = cx.child()
        for cl in e["clauses"]:
            vt = _iter_var_ty(c2, cl["iter"])
            if _name_bound(c2, cl["v"]):
                cx.report("E3019", f"comprehension variable {cl['v']} shadows an enclosing name")
            c2.vars[cl["v"]] = vt
            for f in cl["filters"]:
                require_val(c2, f, infer(c2, f), "as a filter")
                c2 = apply_guards(c2, guards_of(f, True))
        if k == "comp":
            h = require_val(c2, e["head"], infer(c2, e["head"]), "as a comprehension element")
            return TY({"t": "arr", "elem": h["rt"]} if h["rt"] else None)
        kk = require_val(c2, e["key"], infer(c2, e["key"]), "as a map key")
        if kk["rt"] and num_kind(kk["rt"]) != "string":
            cx.report("E4001", "map-comprehension key is not a string")
        v = require_val(c2, e["val"], infer(c2, e["val"]), "as a map value")
        return TY({"t": "map", "key": PRIM("string"), "val": v["rt"]} if v["rt"] else None)
    if k == "bin":
        return _infer_bin(cx, e)
    if k == "un":
        op = e["op"]
        t = require_val(cx, e["x"], infer(cx, e["x"]), f"as `{op}` operand")
        if op == "!":
            if t["rt"] and not _is_boolish(t["rt"]):
                cx.report("E4071", "`!` on a non-bool operand")
            return BOOL
        if op == "~":
            if t["rt"] and num_kind(t["rt"]) != "int":
                cx.report("E4071", "`~` on a non-int operand")
            return TY(PRIM("int"))
        kk = num_kind(t["rt"])
        if t["rt"] and kk != "int" and kk != "float" and kk != "quantity":
            cx.report("E4071", "unary `-` on a non-numeric operand")
        return TY(PRIM(kk) if kk in ("int", "float") else None)
    if k == "paren":
        return infer(cx, e["x"])
    if k == "if":
        c = require_val(cx, e["c"], infer(cx, e["c"]), "as a condition")
        if c["rt"] and not _is_boolish(c["rt"]):
            cx.report("E4001", "`if` condition is not bool")
        t = infer(apply_guards(cx, guards_of(e["c"], True)), e["t"])
        f = infer(apply_guards(cx, guards_of(e["c"], False)), e["f"])
        return TY(mk_union([t["rt"], f["rt"]]), t["abs"] or f["abs"])
    if k == "lambda":
        c2 = cx.child()
        for p in e["params"]:
            if _name_bound(c2, p):
                cx.report("E3019", f"lambda parameter {p} shadows an enclosing name")
            c2.vars[p] = UNK
        infer(c2, e["body"])
        return UNK
    if k == "call":
        return _infer_call(cx, e)
    if k == "member":
        return _infer_member(cx, e)
    if k == "index":
        b = require_val(cx, e["x"], infer(cx, e["x"]), "for indexing")
        return _index_core(cx, b, e)
    if k == "with":
        b = require_val(cx, e["base"], infer(cx, e["base"]), "as `with` base")
        brt = b["rt"]["target"] if (b["rt"] and b["rt"]["t"] == "ref") else b["rt"]
        if brt and brt["t"] != "rec":
            cx.report("E4080", "`with` on a non-record base")
            return UNK
        patch = e["patch"]
        if patch["e"] == "obj" and brt:
            for en in patch["entries"]:
                m = next((m for m in brt["members"] if m["name"] == en["key"]), None)
                if m is None and not brt.get("open"):
                    cx.report("E4080", f"`with` updates unknown member {en['key']}")
                elif m is not None and m["kind"] == "der":
                    cx.report("E4080", f"`with` updates derived member {en['key']}")
        if patch["e"] == "obj":
            for en in patch["entries"]:
                require_val(cx, en["val"], infer(cx, en["val"]), "as a `with` update")
        else:
            infer(cx, patch)
        return TY(brt)
    if k == "match":
        return _infer_match(cx, e, None)
    return UNK


def _iter_var_ty(cx: Ctx, it: dict) -> dict:
    t = require_val(cx, it, infer(cx, it), "as an iterable")
    if it["e"] == "bin" and it["op"] in ("..", "..<"):
        lo = it["l"]["v"] if it["l"]["e"] == "lit" else None
        hi = it["r"]["v"] if it["r"]["e"] == "lit" else None
        if is_float(lo) or is_float(hi):
            cx.report("E4115", "comprehension over a float range")
            return UNK
        if lo is not None and hi is not None:
            return TY({"t": "range", "base": "int", "lo": lo, "hi": hi, "excl": it["op"] == "..<"})
        return TY(PRIM("int"))
    if not t["rt"]:
        return UNK
    as_arr = _arm_of(t["rt"], "arr")
    if as_arr:
        return TY(as_arr["elem"])
    cx.report("E4115", f"comprehension over a non-iterable {'map (use std.map.keys/values)' if _arm_of(t['rt'], 'map') else 'value'}")
    return UNK


def _q_dim(rt: Optional[dict]) -> Optional[str]:
    if rt and rt["t"] == "quantity":
        return rt["dim"]
    if rt and rt["t"] == "pred" and rt["base"]["t"] == "quantity":
        return rt["base"]["dim"]
    return None


def _infer_bin(cx: Ctx, e: dict) -> dict:
    op = e["op"]
    if op == "|>":   # first-argument insertion (§4.9)
        r = e["r"]
        call = {"e": "call", "fn": r["fn"], "args": [e["l"]] + r["args"]} if r["e"] == "call" \
            else {"e": "call", "fn": r, "args": [e["l"]]}
        return _infer_call(cx, call)
    if op == "??":
        l = infer(cx, e["l"])   # absence/null on the left is the point
        r = require_val(cx, e["r"], infer(cx, e["r"]), "as `??` fallback")
        return TY(mk_union([strip_null(l["rt"]), r["rt"]]) if (l["rt"] and r["rt"]) else None)
    if op in ("&&", "||"):
        l = require_val(cx, e["l"], infer(cx, e["l"]), f"as `{op}` operand")
        if l["rt"] and not _is_boolish(l["rt"]):
            cx.report("E4071", f"`{op}` on a non-bool operand")
        c2 = apply_guards(cx, guards_of(e["l"], op == "&&"))
        r = require_val(c2, e["r"], infer(c2, e["r"]), f"as `{op}` operand")
        if r["rt"] and not _is_boolish(r["rt"]):
            cx.report("E4071", f"`{op}` on a non-bool operand")
        return BOOL
    if op == "in":
        require_val(cx, e["l"], infer(cx, e["l"]), "as `in` key")
        r = require_val(cx, e["r"], infer(cx, e["r"]), "as `in` container")
        rrt = r["rt"]["target"] if (r["rt"] and r["rt"]["t"] == "ref") else r["rt"]
        l = e["l"]
        if rrt and rrt["t"] == "rec" and l["e"] == "lit" and is_str(l["v"]):
            m = next((m for m in rrt["members"] if m["name"] == l["v"]), None)
            if m is not None and m["kind"] != "opt":
                cx.report("E4054", f"`in` on member {l['v']}, which is not optional")
            if m is None and not rrt.get("open"):
                cx.report("E4054", f"`in` on undeclared member {l['v']} of a closed record")
        return BOOL
    if op in ("..", "..<"):
        require_val(cx, e["l"], infer(cx, e["l"]), "as a range endpoint")
        require_val(cx, e["r"], infer(cx, e["r"]), "as a range endpoint")
        return UNK   # a range value: iterable / membership container only
    l = require_val(cx, e["l"], infer(cx, e["l"]), f"as `{op}` operand")
    r = require_val(cx, e["r"], infer(cx, e["r"]), f"as `{op}` operand")
    if op == "matches":
        if l["rt"] and num_kind(l["rt"]) != "string":
            cx.report("E4071", "`matches` needs a string left operand")
        return BOOL
    if op in ("==", "!="):
        return BOOL
    lk, rk = num_kind(l["rt"]), num_kind(r["rt"])
    cmp = op in ("<", "<=", ">", ">=")
    if lk == "quantity" or rk == "quantity":
        # §3.16: +/-/compare need equal dimensions; * and / compose them;
        # a bare int/float scales; a cancelled vector is a plain number
        if op in ("+", "-") or cmp:
            if l["rt"] and r["rt"]:
                if lk != "quantity" or rk != "quantity":
                    cx.report("E4071", f"`{op}` mixes quantity and {rk if lk == 'quantity' else lk}")
                else:
                    a, b = _q_dim(l["rt"]), _q_dim(r["rt"])
                    if a is not None and b is not None and a != b:
                        cx.report("E4072", f"`{op}` on quantities of different dimensions ({a or '1'} vs {b or '1'})")
            return BOOL if cmp else TY(l["rt"] if lk == "quantity" else r["rt"])
        if op in ("*", "/"):
            if not l["rt"] or not r["rt"]:
                return UNK
            lv, rv = _q_dim(l["rt"]), _q_dim(r["rt"])
            if (lv is None and lk not in ("int", "float")) or (rv is None and rk not in ("int", "float")):
                cx.report("E4071", f"`{op}` on a non-numeric operand")
                return UNK
            key = key_of_vec(vec_combine(vec_of_key(lv) if lv is not None else {},
                                         vec_of_key(rv) if rv is not None else {}, 1 if op == "*" else -1))
            return TY(PRIM("float") if key == "" else {"t": "quantity", "dim": key})
        cx.report("E4071", f"`{op}` on quantity operands")
        return UNK
    if l["rt"] and r["rt"] and lk and rk and lk != rk:
        cx.report("E4071", f"`{op}` mixes {lk} and {rk} operands")
    if cmp:
        return BOOL
    if op in ("&", "^", "<<", ">>"):
        if (l["rt"] and lk != "int") or (r["rt"] and rk != "int"):
            cx.report("E4071", f"`{op}` on non-int operands")
        return TY(PRIM("int"))
    if op == "|":   # bitwise on ints (type-level | never reaches expressions)
        if (l["rt"] and lk != "int") or (r["rt"] and rk != "int"):
            cx.report("E4071", "`|` on non-int operands")
        return TY(PRIM("int"))
    # + - * / %
    if op == "+" and lk == "string" and rk == "string":
        return TY(PRIM("string"))
    if l["rt"] and r["rt"] and lk and rk:
        if lk not in ("int", "float", "quantity"):
            cx.report("E4071", f"`{op}` on {lk} operands")
        if lk == "int" and op in ("+", "-", "*"):
            # interval arithmetic keeps range-typed operands range-typed, so
            # `9000 + i` with i: 0..<3 stays assignable where 1..65535 is expected
            a, b = _as_ival(l["rt"]), _as_ival(r["rt"])
            if a and b:
                cands = [a[0] + b[0], a[1] + b[1]] if op == "+" else \
                    [a[0] - b[1], a[1] - b[0]] if op == "-" else \
                    [a[0] * b[0], a[0] * b[1], a[1] * b[0], a[1] * b[1]]
                return TY({"t": "range", "base": "int", "lo": min(cands), "hi": max(cands), "excl": False})
        return TY(PRIM(lk) if lk in ("int", "float") else None)
    return UNK


def _as_ival(rt: dict) -> Optional[tuple]:
    if rt["t"] == "lit" and is_int(rt["v"]):
        return (rt["v"], rt["v"])
    if rt["t"] == "range" and rt["base"] == "int" and is_int(rt["lo"]) and is_int(rt["hi"]):
        return (rt["lo"], rt["hi"] - 1 if rt["excl"] else rt["hi"])
    if rt["t"] == "union":
        ivs = [_as_ival(a) for a in rt["arms"]]
        if ivs and all(v is not None for v in ivs):
            return (min(v[0] for v in ivs), max(v[1] for v in ivs))
    if rt["t"] == "pred":
        return _as_ival(rt["base"])
    return None


def _index_core(cx: Ctx, b: dict, e: dict) -> dict:
    it = require_val(cx, e["i"], infer(cx, e["i"]), "as an index")
    if not b["rt"]:
        return UNK
    as_arr = _arm_of(b["rt"], "arr")
    if as_arr:
        if it["rt"] and num_kind(it["rt"]) != "int":
            cx.report("E4071", "array index is not an int")
        return TY(as_arr["elem"])
    as_map = _arm_of(b["rt"], "map")
    if as_map:
        k = path_key(e)
        return TY(as_map["val"], not (k and k in cx.present))
    if _arm_of(b["rt"], "rec"):
        return UNK   # dynamic member access
    cx.report("E4071", "indexing a non-collection")
    return UNK


def _imported_ty(cx: Ctx, ex: dict) -> dict:
    """a name imported from another module, typed in that module's scope"""
    t = ex["env"]
    name = ex["name"]
    if name in t.consts:
        return TY(try_resolve(t, t.consts[name].get("type")))
    if name in t.funcs:
        f = t.funcs[name]
        return TY({"t": "func", "params": [try_resolve(t, p.get("type")) or {"t": "any"} for p in f["params"]],
                   "ret": try_resolve(t, f.get("ret"))})
    o = next((o for o in t.outputs if o["name"] == name), None)
    if o is not None:
        return TY(try_resolve(t, o["type"]))
    if name in t.inputs:
        return TY(try_resolve(t, t.inputs[name]["type"]))
    if name in t.type_asts:
        cx.report("E3008", f"type name {name} used as a value")
        return UNK
    return UNK


def _infer_member(cx: Ctx, e: dict) -> dict:
    if _std_path(e) is not None:
        return UNK   # std.* namespace path (typed at the call)
    x = e["x"]
    if x["e"] == "name" and x["name"] not in cx.vars and x["name"] in cx.env.namespaces:
        ex = cx.env.namespaces[x["name"]]["exports"].get(e["name"])
        if ex is None:
            cx.report("E3005", f"namespace {x['name']} has no export {e['name']}")
            return UNK
        return _imported_ty(cx, ex)
    b = infer(cx, x)
    key = path_key(x)
    if not e.get("safe"):
        if b["abs"] and not (key and key in cx.present):
            cx.report("E4050", "member access on a maybe-absent expression (use ?. or an `in` guard)")
        if has_null(b["rt"]) and not (key and key in cx.nonnull):
            cx.report("E4051", f"member .{e['name']} on a possibly-null expression without ?.")
    return _member_core(cx, b, e)


def _member_core(cx: Ctx, b: dict, e: dict) -> dict:
    brt = strip_null(b["rt"]) if b["rt"] else None
    if brt and brt["t"] == "ref":
        brt = brt["target"]
    if brt and brt["t"] == "pred":
        brt = brt["base"]
    if brt and brt["t"] == "isectN":
        brt = _arm_of(brt, "rec") or _arm_of(brt, "map") or brt
    safe = bool(e.get("safe"))

    def mk_abs(t: dict) -> dict:
        return TY(t["rt"], True) if safe else t
    if not brt:
        return mk_abs(UNK)
    if brt["t"] == "rec":
        m = next((m for m in brt["members"] if m["name"] == e["name"]), None)
        if m is None:
            if not brt.get("open"):
                cx.report("E4003", f"member {e['name']} is not declared on {brt.get('name') or 'this record'}")
            return mk_abs(UNK)
        rt = {"t": "isectN", "arms": m["conj"]} if m.get("conj") else m.get("type")
        k = path_key(e)
        return TY(rt, safe or (m["kind"] == "opt" and not (k and k in cx.present)))
    if brt["t"] == "map":
        k = path_key(e)
        return TY(brt["val"], safe or not (k and k in cx.present))
    if brt["t"] == "union":
        parts = []
        for a in brt["arms"]:
            arm = next((m for m in a["members"] if m["name"] == e["name"]), None) if a["t"] == "rec" else None
            parts.append(arm.get("type") if arm else None)
        return mk_abs(TY(mk_union(parts)))
    if brt["t"] == "quantity" and e["name"] in ("value", "unit"):
        return mk_abs(TY(PRIM("float") if e["name"] == "value" else PRIM("string")))
    return mk_abs(UNK)


def _func_rt(cx: Ctx, name: str) -> dict:
    f = cx.env.funcs[name]
    return {"t": "func", "params": [try_resolve(cx.env, p.get("type")) or {"t": "any"} for p in f["params"]],
            "ret": try_resolve(cx.env, f.get("ret"))}


def _const_ty(cx: Ctx, name: str) -> dict:
    if name in cx.const_memo:
        return cx.const_memo[name]
    cx.const_memo[name] = UNK   # cycle guard
    c = cx.env.consts[name]
    anno = try_resolve(cx.env, c.get("type"))
    ty = TY(anno) if anno else infer(make_ctx(cx.env, lambda code, msg: None), c["expr"])   # silent module-scope inference
    cx.const_memo[name] = ty
    return ty


def _infer_call(cx: Ctx, e: dict) -> dict:
    sp = _std_path(e["fn"])
    if sp is not None:
        sig = STD.get(sp)
        if sig is None:
            cx.report("E3003", f"std.{sp} does not exist (§13.1: names not listed do not exist)")
        if sig is not None and len(e["args"]) != sig[0]:
            cx.report("E4062", f"std.{sp} expects {sig[0]} argument(s), got {len(e['args'])}")
        for a in e["args"]:
            if a["e"] == "lambda":
                infer(cx, a)
                continue
            require_val(cx, a, infer(cx, a), "as an argument")
        return TY(sig[1] if sig else None)
    f = infer(cx, e["fn"])
    frt = f["rt"] if (f["rt"] and f["rt"]["t"] == "func") else None
    if frt and len(e["args"]) != len(frt["params"]):
        cx.report("E4062", f"call expects {len(frt['params'])} argument(s), got {len(e['args'])}")
    for i, a in enumerate(e["args"]):
        expected = frt["params"][i] if (frt and i < len(frt["params"]) and frt["params"][i]["t"] != "any") else None
        if a["e"] == "lambda" and expected and expected["t"] == "func":
            _check_lambda(cx, a, expected)
            continue
        if a["e"] == "lambda":
            infer(cx, a)
            continue
        at = require_val(cx, a, infer(cx, a), "as an argument")
        if at["rt"] and expected and not subsumes(cx.env, at["rt"], expected) and not _deferrable(at["rt"], expected):
            cx.report("E4001", f"argument {i + 1} is not assignable to its parameter")
    return TY(frt["ret"] if frt else None)


def _check_lambda(cx: Ctx, e: dict, expected: dict) -> None:
    if len(e["params"]) != len(expected["params"]):
        cx.report("E4062", "lambda arity differs from expected function type")
        return
    c2 = cx.child()
    for i, p in enumerate(e["params"]):
        if _name_bound(c2, p):
            cx.report("E3019", f"lambda parameter {p} shadows an enclosing name")
        c2.vars[p] = TY(None if expected["params"][i]["t"] == "any" else expected["params"][i])
    b = require_val(c2, e["body"], infer(c2, e["body"]), "as a lambda result")
    if b["rt"] and expected.get("ret") and not subsumes(cx.env, b["rt"], expected["ret"]) and not _deferrable(b["rt"], expected["ret"]):
        cx.report("E4001", "lambda body is not assignable to the expected result type")


# ---------------- match (§4.7) ----------------
def _infer_match(cx: Ctx, e: dict, expected: Optional[dict]) -> dict:
    s = require_val(cx, e["subject"], infer(cx, e["subject"]), "as a match subject")
    variants: Optional[list] = None
    if s["rt"]:
        srt = strip_null(s["rt"])
        if srt["t"] == "union":
            variants = srt["arms"] + [{"t": "lit", "v": None}] if has_null(s["rt"]) else srt["arms"]
        else:
            cx.report("E4103", "`match` subject is not a discriminable union")
    covered: set = set()
    catch_alls = 0
    results: list = []
    for arm in e["arms"]:
        c2 = cx.child()
        if _name_bound(c2, arm["v"]):
            cx.report("E3019", f"match binding {arm['v']} shadows an enclosing name")
        arm_ty: Optional[dict] = None
        if arm.get("type"):
            arm_ty = try_resolve(cx.env, arm["type"])
            if not arm_ty:
                cx.report("E3003", "unknown type in match arm")
            if variants is not None and arm_ty:
                for i, v in enumerate(variants):
                    if subsumes(cx.env, v, arm_ty):
                        if i in covered:
                            cx.report("E4100", "match arms overlap on a variant")
                        covered.add(i)
        else:
            catch_alls += 1
            if variants is not None:
                rest = [v for i, v in enumerate(variants) if i not in covered]
                if not rest:
                    cx.report("E4102", "match catch-all is dead (typed arms are exhaustive)")
                arm_ty = mk_union(rest)
        c2.vars[arm["v"]] = TY(arm_ty)
        b = require_val(c2, arm["body"], check_expr(c2, arm["body"], expected) if expected else infer(c2, arm["body"]), "as a match result")
        results.append(b["rt"])
    if catch_alls > 1:
        cx.report("E4100", "more than one match catch-all arm")
    if variants is not None and catch_alls == 0 and len(covered) < len(variants):
        cx.report("E4101", "`match` is not exhaustive over the subject union")
    return TY(mk_union(results))


# ---------------- bidirectional checking (§3.18) ----------------
def _place_ty(cx: Ctx, e: dict) -> dict:
    """a navigation expression in a ref<T> position denotes a place (§7.4):
    the absence discipline does not apply along the spine — whether the
    place holds a value is reference integrity (§7.5), checked at binding"""
    k = e["e"]
    if k == "paren":
        return _place_ty(cx, e["x"])
    if k == "member":
        base = _place_ty(cx, e["x"])
        # a hidden member's value is not part of any document: no reference
        # can target it (§7.5, D34)
        rt = base.get("rt")
        rec = rt["target"] if rt is not None and rt["t"] == "ref" else rt
        if rec is not None and rec["t"] == "rec":
            hm = next((m for m in rec["members"] if m["name"] == e["name"]), None)
            if hm is not None and hm.get("hidden"):
                cx.report("E4093", f"`ref` position navigates hidden member {e['name']} — not part of the value (§7.5)")
        return _member_core(cx, base, e)
    if k == "index":
        return _index_core(cx, _place_ty(cx, e["x"]), e)
    if k == "if":
        # a conditional between places is a place: each branch is read in
        # the ref position, the condition as an ordinary value (§7.4)
        c = require_val(cx, e["c"], infer(cx, e["c"]), "as a condition")
        if c["rt"] and not _is_boolish(c["rt"]):
            cx.report("E4001", "`if` condition is not bool")
        t = _place_ty(apply_guards(cx, guards_of(e["c"], True)), e["t"])
        f = _place_ty(apply_guards(cx, guards_of(e["c"], False)), e["f"])
        return TY(mk_union([t["rt"], f["rt"]]), t["abs"] or f["abs"])
    # the spine root must be a root-derived place, never a module const (§7.5, D32)
    if k == "name" and e["name"] not in cx.vars and e["name"] in cx.env.consts:
        cx.report("E4093", f"`ref` position navigates module const {e['name']} — not a root-derived place (§7.5)")
    return infer(cx, e)


def _bind_clauses(cx: Ctx, clauses: list) -> Ctx:
    c2 = cx.child()
    for cl in clauses:
        vt = _iter_var_ty(c2, cl["iter"])
        if _name_bound(c2, cl["v"]):
            cx.report("E3019", f"comprehension variable {cl['v']} shadows an enclosing name")
        c2.vars[cl["v"]] = vt
        for f in cl["filters"]:
            require_val(c2, f, infer(c2, f), "as a filter")
            c2 = apply_guards(c2, guards_of(f, True))
    return c2


def check_expr(cx: Ctx, e: dict, expected: Optional[dict]) -> dict:
    if not expected:
        return infer(cx, e)
    if expected["t"] == "ref":
        _place_ty(cx, e)
        return TY(expected)   # place, not value (§7.4)
    if expected["t"] == "pred":
        return check_expr(cx, e, expected["base"])
    k = e["e"]
    if expected["t"] == "isectN" and k in ("obj", "arr", "comp", "mapcomp"):
        for arm in expected["arms"]:
            check_expr(cx, e, arm)   # a literal must satisfy every arm
        return TY(expected)
    if k == "comp" and expected["t"] == "arr":
        c2 = _bind_clauses(cx, e["clauses"])
        check_expr(c2, e["head"], expected["elem"])
        return TY(expected)
    if k == "mapcomp" and expected["t"] == "map":
        c2 = _bind_clauses(cx, e["clauses"])
        kk = require_val(c2, e["key"], infer(c2, e["key"]), "as a map key")
        if kk["rt"] and num_kind(kk["rt"]) != "string":
            cx.report("E4001", "map-comprehension key is not a string")
        check_expr(c2, e["val"], expected["val"])
        return TY(expected)
    if k == "paren":
        return check_expr(cx, e["x"], expected)
    if k == "if":
        c = require_val(cx, e["c"], infer(cx, e["c"]), "as a condition")
        if c["rt"] and not _is_boolish(c["rt"]):
            cx.report("E4001", "`if` condition is not bool")
        check_expr(apply_guards(cx, guards_of(e["c"], True)), e["t"], expected)
        check_expr(apply_guards(cx, guards_of(e["c"], False)), e["f"], expected)
        return TY(expected)
    if k == "match":
        return _infer_match(cx, e, expected)
    if k == "obj":
        if expected["t"] == "rec":
            # entries see the record's members (siblings + inherited scope chain)
            cx_r = cx.child()
            for m in expected["members"]:
                mt = {"t": "isectN", "arms": m["conj"]} if m.get("conj") else m.get("type")
                cx_r.vars[m["name"]] = TY(mt, m["kind"] == "opt")
            for en in e["entries"]:
                m = next((m for m in expected["members"] if m["name"] == en["key"]), None)
                if m is None:
                    if not expected.get("open"):
                        cx.report("E4003", f"member {en['key']} is not declared on {expected.get('name') or 'the record'}")
                    require_val(cx_r, en["val"], infer(cx_r, en["val"]), "as a construction member")
                    continue
                mt = {"t": "isectN", "arms": m["conj"]} if m.get("conj") else m.get("type")
                require_val(cx_r, en["val"], check_expr(cx_r, en["val"], mt), "as a construction member")
            for m in expected["members"]:
                if m["kind"] == "req" and not any(en["key"] == m["name"] for en in e["entries"]):
                    cx.report("E4002", f"required member {m['name']} missing in the construction")
            return TY(expected)
        if expected["t"] == "map":
            for en in e["entries"]:
                require_val(cx, en["val"], check_expr(cx, en["val"], expected["val"]), "as a map value")
            return TY(expected)
        if expected["t"] == "union":
            infer(cx, e)
            return TY(expected)   # discriminated at binding
        infer(cx, e)
        cx.report("E4001", f"object literal where {expected['t']} is expected")
        return TY(expected)
    if k == "arr" and expected["t"] == "arr":
        for it in e["items"]:
            if it["spread"]:
                require_val(cx, it["expr"], infer(cx, it["expr"]), "as a spread")
                continue
            require_val(cx, it["expr"], check_expr(cx, it["expr"], expected["elem"]), "as an array element")
        return TY(expected)
    if k == "lambda" and expected["t"] == "func":
        _check_lambda(cx, e, expected)
        return TY(expected)
    ty = require_val(cx, e, infer(cx, e), "as a value")
    if ty["rt"] and not subsumes(cx.env, ty["rt"], expected) and not _deferrable(ty["rt"], expected):
        cx.report("E4001", "expression type does not satisfy the expected type")
    return ty


def _deferrable(s: dict, t: dict) -> bool:
    """a same-kind refinement target (pattern, range, literal set) whose
    membership the static type cannot prove is validated at binding, not
    rejected here — the corpus (guide, benchmarks) relies on this split;
    kind-level mismatches still fail statically"""
    k = num_kind(s)
    if not k:
        return False
    tt = t["t"]
    if tt == "pattern":
        return k == "string"
    if tt == "range":
        return k == t["base"]
    if tt == "lit":
        return k == num_kind(t)
    if tt == "union":
        return any(_deferrable(s, a) for a in t["arms"])
    if tt == "pred":
        return _deferrable(s, t["base"])
    return False
