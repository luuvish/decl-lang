"""Value model, environment, and type resolution — a faithful port of the
reference implementation's semantics.ts (decl-ts/src). Types and ASTs are
plain dicts shaped exactly like the TypeScript objects; values use
Python natives for scalars (int / float / str / bool / None) plus the
small classes below."""
from __future__ import annotations

import copy
import json
import re
from typing import Any, Callable, Optional


# ---------------- values ----------------
class _Absent:
    __slots__ = ()

    def __repr__(self) -> str:
        return "ABSENT"


ABSENT = _Absent()


def is_int(v: Any) -> bool:
    return type(v) is int


def is_float(v: Any) -> bool:
    return type(v) is float


def is_bool(v: Any) -> bool:
    return type(v) is bool


def is_str(v: Any) -> bool:
    return type(v) is str


class Quantity:
    __slots__ = ("dim", "value")

    def __init__(self, dim: str, value: float):
        self.dim, self.value = dim, value


class Ref:
    __slots__ = ("segs",)

    def __init__(self, segs: list):
        self.segs = segs


class RangeV:
    __slots__ = ("lo", "hi", "excl")

    def __init__(self, lo, hi, excl: bool):
        self.lo, self.hi, self.excl = lo, hi, excl


class Closure:
    __slots__ = ("params", "body", "scope")

    def __init__(self, params, body, scope):
        self.params, self.body, self.scope = params, body, scope


class NatFn:
    __slots__ = ("fn",)

    def __init__(self, fn: Callable):
        self.fn = fn


class StdRef:
    __slots__ = ("path",)

    def __init__(self, path: list):
        self.path = path


class NsRef:
    __slots__ = ("env", "exports")

    def __init__(self, env, exports):
        self.env, self.exports = env, exports


class Pattern:
    __slots__ = ("re",)

    def __init__(self, re_: str):
        self.re = re_


class PreVal:
    """An unevaluated literal entry: expression + the scope it closes over."""
    __slots__ = ("expr", "scope")

    def __init__(self, expr, scope):
        self.expr, self.scope = expr, scope


class PreObj:
    __slots__ = ("entries",)

    def __init__(self, entries: list):
        self.entries = entries          # [(key, PreVal | value)]


class PreArr:
    __slots__ = ("items",)

    def __init__(self, items: list):
        self.items = items              # [(spread: bool, PreVal | value)]


class JObj:
    """A lexical-JSON object (ordered entries), as read from a document."""
    __slots__ = ("entries",)

    def __init__(self, entries: list):
        self.entries = entries          # [(key, value)]


class Segs:
    __slots__ = ("segs",)

    def __init__(self, segs: list):
        self.segs = segs


class Key:
    """A map key inside a canonical path (§7.2) — kept apart from a record
    member because the canonical text differs: a map key is always
    bracketed, a member is dotted when the dot can spell it."""
    __slots__ = ("k",)

    def __init__(self, k: str):
        self.k = k

    def __eq__(self, other):
        return isinstance(other, Key) and other.k == self.k

    def __hash__(self):
        return hash(("key", self.k))

    def __repr__(self):
        return f"Key({self.k!r})"


def seg_text(s):
    return s.k if isinstance(s, Key) else s


def dot_spellable(name) -> bool:
    """§3.11, §4.3: identifier-shaped and not a literal keyword."""
    return isinstance(name, str) and _ID_RE.match(name) is not None and name not in ("true", "false", "null")


class ArrV:
    __slots__ = ("items", "path")

    def __init__(self, items: list, path: list):
        self.items, self.path = items, path


class MapV:
    __slots__ = ("entries", "path")

    def __init__(self, entries: dict, path: list):
        self.entries, self.path = entries, path


class Slot:
    __slots__ = ("kind", "state", "value", "deferred", "compute", "hidden")

    def __init__(self, kind: str, state: str, deferred: bool = False, compute=None, hidden: bool = False):
        self.kind, self.state, self.deferred, self.compute = kind, state, deferred, compute
        self.value = None
        self.hidden = hidden   # `x$ = e`: computed, never part of the value (D34)


class RecInst:
    __slots__ = ("type_name", "rt", "path", "parent", "slots", "entry_order", "extras", "menv")

    def __init__(self, type_name, rt, path, parent):
        self.type_name, self.rt, self.path, self.parent = type_name, rt, path, parent
        self.slots: dict = {}
        self.entry_order: list = []
        self.extras: dict = {}
        self.menv = None


class Scope:
    __slots__ = ("inst", "locals", "root_name", "menv")

    def __init__(self, inst, locals_: dict, root_name: str, menv=None):
        self.inst, self.locals, self.root_name, self.menv = inst, locals_, root_name, menv

    def with_locals(self, locals_: dict) -> "Scope":
        return Scope(self.inst, locals_, self.root_name, self.menv)

    def with_inst(self, inst) -> "Scope":
        return Scope(inst, self.locals, self.root_name, self.menv)

    def with_menv(self, menv) -> "Scope":
        return Scope(self.inst, self.locals, self.root_name, menv)


class Taint(Exception):
    pass


class DeferSig(Exception):
    pass


class EvalErr(Exception):
    def __init__(self, msg: str, code: Optional[str] = None):
        super().__init__(msg)
        self.msg, self.code = msg, code


# ---------------- dimensions as exponent vectors (§3.16) ----------------
def key_of_vec(v: dict) -> str:
    parts = sorted((n, e) for n, e in v.items() if e != 0)
    return "*".join(n if e == 1 else f"{n}^{e}" for n, e in parts)


def vec_of_key(key: str) -> dict:
    v: dict = {}
    if not key:
        return v
    for p in key.split("*"):
        n, _, e = p.partition("^")
        v[n] = v.get(n, 0) + (int(e) if e else 1)
    return v


def vec_combine(a: dict, b: dict, sign: int) -> dict:
    out = dict(a)
    for n, e in b.items():
        out[n] = out.get(n, 0) + sign * e
    return out


# ---------------- environment ----------------
SI_PREFIXES = [
    ("y", 1e-24), ("z", 1e-21), ("a", 1e-18), ("f", 1e-15), ("p", 1e-12),
    ("n", 1e-9), ("u", 1e-6), ("m", 1e-3), ("c", 1e-2), ("d", 1e-1),
    ("da", 1e1), ("h", 1e2), ("k", 1e3), ("M", 1e6), ("G", 1e9),
    ("T", 1e12), ("P", 1e15), ("E", 1e18), ("Z", 1e21), ("Y", 1e24),
]


def _t(name: str, exp: int) -> dict:
    return {"name": name, "exp": exp}


class Env:
    def __init__(self) -> None:
        self.type_asts: dict = {}
        self.type_memo: dict = {}
        # names being spliced into a pattern right now, across nested
        # resolutions — a mutually recursive pair is a cycle, not a stack overflow
        self.pattern_visiting: set = set()
        self.consts: dict = {}
        self.funcs: dict = {}
        self.duplicates: list = []
        self.outputs: list = []
        self.inputs: dict = {}
        self.diags: dict = {}
        self.registry: list = []
        self.roots: dict = {}
        self.diagnostics: list = []
        # installed by the engine: the evaluation step a report is attributed to
        self.tagger: Optional[Callable[[], Optional[str]]] = None
        self.const_eval: Optional[Callable[[str], Any]] = None
        self.expr_eval: Optional[Callable[[dict], Any]] = None
        self.imports: dict = {}
        self.namespaces: dict = {}
        self.on_const_diag: Optional[Callable[[dict], None]] = None
        self._const_diag_seen: set = set()
        self.dim_decls: dict = {}
        self.dim_memo: dict = {}
        self.unit_decls: dict = {}
        self.unit_memo: dict = {}
        self.base_unit_of: dict = {}
        self.space_diags: list = []
        self._seed_units()

    # std.units — the SI catalog generated from the §13.10 prefix rule (D15)
    def _seed_units(self) -> None:
        def dim(name, terms=None):
            self.dim_decls[name] = {"terms": terms}

        def unit(sym, dim_=None, factor=None, base=None):
            if sym in self.unit_decls:
                return
            self.unit_decls[sym] = {"dim": dim_} if dim_ is not None else \
                {"factor": {"e": "lit", "v": float(factor)}, "base": base}

        bases = [("Time", "s"), ("Length", "m"), ("Mass", "kg"), ("Current", "A"),
                 ("Temperature", "K"), ("Amount", "mol"), ("LuminousIntensity", "cd")]
        for d, _ in bases:
            dim(d)
        derived = [
            ("Frequency", [_t("Time", -1)], "Hz"),
            ("Force", [_t("Mass", 1), _t("Length", 1), _t("Time", -2)], "N"),
            ("Pressure", [_t("Mass", 1), _t("Length", -1), _t("Time", -2)], "Pa"),
            ("Energy", [_t("Mass", 1), _t("Length", 2), _t("Time", -2)], "J"),
            ("Power", [_t("Mass", 1), _t("Length", 2), _t("Time", -3)], "W"),
            ("Charge", [_t("Current", 1), _t("Time", 1)], "C"),
            ("Voltage", [_t("Mass", 1), _t("Length", 2), _t("Time", -3), _t("Current", -1)], "V"),
            ("Resistance", [_t("Mass", 1), _t("Length", 2), _t("Time", -3), _t("Current", -2)], "Ohm"),
            ("Capacitance", [_t("Mass", -1), _t("Length", -2), _t("Time", 4), _t("Current", 2)], "F"),
            ("DataSize", None, "bit"),
        ]
        for d, terms, _ in derived:
            dim(d, terms)
        for d, s in bases:
            unit(s, dim_=d)
        for d, _, s in derived:
            unit(s, dim_=d)
        unit("B", factor=8, base="bit")
        unit("g", factor=1e-3, base="kg")
        prefixable = [s for _, s in bases if s != "kg"] + [s for _, _, s in derived if s != "bit"] + ["g"]
        for u0 in prefixable:
            for p, f in SI_PREFIXES:
                unit(p + u0, factor=f, base=u0)
        for u0 in ("bit", "B"):
            for p, f in [("Ki", 1024), ("Mi", 1024 ** 2), ("Gi", 1024 ** 3), ("Ti", 1024 ** 4),
                         ("Pi", 1024 ** 5), ("Ei", 1024 ** 6)]:
                unit(p + u0, factor=f, base=u0)
            for p, f in SI_PREFIXES:
                if p in ("k", "M", "G", "T", "P", "E"):
                    unit(p + u0, factor=f, base=u0)

    def load(self, decls: list) -> None:
        seen: set = set()

        def claim(n: str) -> None:
            if n in seen:
                self.duplicates.append(n)
            seen.add(n)

        for d in decls:
            name = d.get("name")
            if isinstance(name, str) and d["d"] not in ("unit", "dimension"):
                claim(name)
            k = d["d"]
            if k == "dimension":
                if name in self.dim_decls:
                    self.space_diags.append({"severity": "error", "code": "E3001",
                                             "message": f"dimension {name} redeclared", "path": ""})
                else:
                    self.dim_decls[name] = {"terms": d.get("terms")}
            elif k == "unit":
                if name in self.unit_decls:
                    self.space_diags.append({"severity": "error", "code": "E4073",
                                             "message": f"unit {name} redeclared", "path": ""})
                else:
                    self.unit_decls[name] = {"dim": d.get("dim"), "factor": d.get("factor"), "base": d.get("base")}
            if k == "type":
                self.type_asts[name] = {"ast": d["type"], "tail": d.get("tail"), "params": d.get("params")}
            elif k == "const":
                self.consts[name] = {"expr": d["expr"], "type": d.get("type"), "state": "unforced", "value": None}
            elif k == "func":
                self.funcs[name] = {"params": d["params"], "ret": d.get("ret"), "body": d["body"]}
            elif k == "output":
                self.outputs.append(d)
            elif k == "input":
                self.inputs[name] = {"type": d["type"], "fallback": d.get("fallback")}
            elif k == "diagnostic":
                self.diags[name] = d

    def report(self, d: dict) -> None:
        by = self.tagger() if self.tagger is not None else None
        if by is not None:
            d["by"] = by
        self.diagnostics.append(d)

    def finalize_unit_space(self) -> list:
        """§3.16 unit/dimension-space findings for the checker: the load-time
        redeclarations plus unresolvable units and duplicate base units."""
        out = list(self.space_diags)
        base_seen: dict = {}
        for sym, u in list(self.unit_decls.items()):
            try:
                info = self.unit_info(sym)
                if u.get("dim") is not None:
                    prev = base_seen.get(info["key"])
                    if prev:
                        out.append({"severity": "error", "code": "E4073",
                                    "message": f"second base unit {sym} for dimension {info['key']} (base is {prev})", "path": ""})
                    else:
                        base_seen[info["key"]] = sym
            except Exception as e:
                msg = str(e)
                code = "E3003" if ("unknown dimension" in msg or "circular dimension" in msg) else "E4073"
                out.append({"severity": "error", "code": code, "message": msg, "path": ""})
        return out

    # §4.13: a named endpoint in a constant position evaluates at elaboration time
    def const_num(self, v: Any) -> Any:
        if not is_str(v) or self.const_eval is None or v not in self.consts:
            return v

        def diag(code: str, message: str) -> None:
            if v + code in self._const_diag_seen:
                return
            self._const_diag_seen.add(v + code)
            d = {"severity": "error", "code": code, "message": message, "path": ""}
            (self.on_const_diag or self.report)(d)

        try:
            r = self.const_eval(v)
            if is_int(r) or is_float(r):
                return r
            if r is not None:
                diag("E4021", f"constant {v} is not numeric in a constant position")
            return v
        except EvalErr as e:
            code = "E5001" if "zero" in e.msg else "E5002" if ("NaN" in e.msg or "Infinity" in e.msg) else "E5001"
            diag(code, f"evaluating constant {v}: {e.msg}")
            return v
        except Exception:
            return v

    # ---- unit / dimension name spaces ----
    def resolve_dim(self, name: str, visiting: Optional[set] = None) -> dict:
        if name in self.dim_memo:
            return self.dim_memo[name]
        visiting = visiting or set()
        if name in visiting:
            raise RuntimeError(f"circular dimension {name}")
        d = self.dim_decls.get(name)
        if d is None:
            raise RuntimeError(f"unknown dimension {name}")
        vec: dict = {}
        if not d.get("terms"):
            vec = {name: 1}
        else:
            visiting.add(name)
            for t in d["terms"]:
                sub = self.resolve_dim(t["name"], visiting)
                for n, e in sub.items():
                    vec[n] = vec.get(n, 0) + e * t["exp"]
            visiting.discard(name)
        self.dim_memo[name] = vec
        return vec

    def unit_info(self, sym: str, visiting: Optional[set] = None) -> dict:
        if sym in self.unit_memo:
            return self.unit_memo[sym]
        visiting = visiting or set()
        if sym in visiting:
            raise RuntimeError(f"circular unit {sym}")
        u = self.unit_decls.get(sym)
        if u is None:
            raise RuntimeError(f"unknown unit {sym}")
        if u.get("dim") is not None:
            key = key_of_vec(self.resolve_dim(u["dim"]))
            if key not in self.base_unit_of:
                self.base_unit_of[key] = sym
            info = {"key": key, "to_base": 1.0}
        else:
            visiting.add(sym)
            b = self.unit_info(u["base"], visiting)
            visiting.discard(sym)
            f = u["factor"]["v"] if u.get("factor") and u["factor"].get("e") == "lit" else None
            if f is None and u.get("factor") is not None and self.expr_eval is not None:
                try:
                    f = self.expr_eval(u["factor"])
                except Exception:
                    f = None
            if is_int(f):
                f = float(f)
            if not is_float(f):
                raise RuntimeError(f"unit {sym}: factor is not a numeric constant")
            info = {"key": b["key"], "to_base": f * b["to_base"]}
        self.unit_memo[sym] = info
        return info

    # ---- type resolution ----
    def resolve(self, ast: dict, name: Optional[str] = None) -> dict:
        k = ast["k"]
        if k == "prim":
            return {"t": "prim", "name": ast["name"]}
        if k == "lit":
            return {"t": "lit", "v": ast["v"]}
        if k == "range":
            lo, hi = self.const_num(ast["lo"]), self.const_num(ast["hi"])
            is_f = is_float(lo) or is_float(hi)
            return {"t": "range", "lo": lo, "hi": hi, "excl": ast["excl"], "base": "float" if is_f else "int"}
        if k == "pattern":
            src = self.expand_pattern(ast["re"])
            bad = pattern_error(src)
            if bad:
                raise RuntimeError(f"malformed pattern /{ast['re']}/: {bad}")
            try:
                compiled = compile_pattern(src)
            except re.error as e:
                raise RuntimeError(f"malformed pattern /{ast['re']}/: {e}")
            return {"t": "pattern", "src": src, "re": compiled}
        if k == "map":
            return {"t": "map", "key": self.resolve(ast["key"]), "val": self.resolve(ast["val"])}
        if k == "array":
            lo, hi0 = self.const_num(ast.get("lo")), self.const_num(ast.get("hi"))
            hi = (int(hi0) - 1) if (ast.get("excl") and not is_str(hi0) and hi0 is not None) else hi0
            return {"t": "arr", "elem": self.resolve(ast["elem"]),
                    "lo": int(lo) if is_int(lo) or is_float(lo) else lo,
                    "hi": int(hi) if is_int(hi) or is_float(hi) else hi}
        if k == "union":
            return {"t": "union", "arms": [self.resolve(a) for a in ast["arms"]]}
        if k == "isect":
            arms = [self.resolve(a) for a in ast["arms"]]
            if all(a["t"] == "rec" for a in arms):
                return self.merge_isect(arms, name)
            return {"t": "isectN", "arms": arms}
        if k == "record":
            rt = {"t": "rec", "name": name, "members": [], "asserts": [], "open": ast["open"], "tail": None}
            self.fill_record(rt, ast["members"])
            return rt
        if k == "func":
            return {"t": "func", "params": [self.resolve(p) for p in ast["params"]], "ret": self.resolve(ast["ret"])}
        if k == "named":
            return self._resolve_named(ast, name)
        raise RuntimeError(f"resolve: unhandled {k}")

    def _resolve_named(self, ast: dict, name: Optional[str]) -> dict:
        if ast.get("preds"):
            base = self.resolve({**ast, "preds": None}, name)
            return {"t": "pred", "base": base, "preds": ast["preds"]}
        n = ast["name"]
        args = ast.get("args") or []
        if n == "quantity":
            return {"t": "quantity", "dim": key_of_vec(self.resolve_dim(args[0]["name"]))}
        if n == "map" and len(args) == 2:
            return {"t": "map", "key": self.resolve(args[0]), "val": self.resolve(args[1])}
        if n == "ref":
            return {"t": "ref", "target": self.resolve(args[0])}
        if n in ("int", "float", "bool", "string") and not args and not ast.get("ext"):
            return {"t": "prim", "name": n}
        decl = self.type_asts.get(n)
        if decl is None:
            im = self.imports.get(n)
            if im is not None:
                return im["env"].resolve({**ast, "name": im["name"]}, name)
            if "." in n:
                ns, _, rest = n.partition(".")
                nsp = self.namespaces.get(ns)
                ex = nsp["exports"].get(rest) if nsp else None
                if ex is not None:
                    return ex["env"].resolve({**ast, "name": ex["name"]}, name)
            raise RuntimeError(f"unknown type {n}")
        if decl.get("params"):
            base = self.instantiate(ast, decl)
        elif n in self.type_memo:
            base = self.type_memo[n]
        elif decl["ast"]["k"] == "record":
            base = {"t": "rec", "name": n, "members": [], "asserts": [], "open": decl["ast"]["open"], "tail": decl.get("tail")}
            self.type_memo[n] = base
            # a member that fails to resolve must not leave a half-filled
            # record memoized (later lookups would miss its later members)
            try:
                self.fill_record(base, decl["ast"]["members"])
            except BaseException:
                self.type_memo.pop(n, None)
                raise
        elif decl["ast"]["k"] == "named" and decl["ast"].get("ext"):
            # an extension declaration (§3.14) is memoized before its parent
            # resolves: in a recursive family — `type Base = { kids: { [string]:
            # Kid } }`, `type Kid = Base { … }` — the parent's body names this
            # type, and every reference must share the one final record rather
            # than a snapshot of the parent's members taken mid-fill
            base = {"t": "rec", "name": n, "members": [], "asserts": [], "tail": decl.get("tail"), "filling": True}
            self.type_memo[n] = base
            try:
                parent = self.resolve({**decl["ast"], "ext": None})
                self.extend_into(base, parent, self.resolve(decl["ast"]["ext"]))
            except BaseException:
                self.type_memo.pop(n, None)
                raise
        else:
            base = self.resolve(decl["ast"], n)
            if base["t"] in ("rec", "union"):
                base["name"] = n
            if base.get("tail") is None:
                base["tail"] = decl.get("tail")
            self.type_memo[n] = base
        if ast.get("ext"):
            # an inline extension in a type position: anonymous, never memoized
            merged = {"t": "rec", "name": base.get("name"), "members": [], "asserts": [], "filling": True}
            self.extend_into(merged, base, self.resolve(ast["ext"]))
            return merged
        return base

    # §3.14: fill `target` as `base` extended by the override body `ext` —
    # base members copied, overrides replacing or adding, asserts appended,
    # and a context declaration narrowed by the extension replacing the
    # inherited one (§7.3). A base still being filled (the recursive-family
    # case above) defers the merge until it completes; `target` stays marked
    # filling meanwhile, so an extension of an extension waits in turn.
    def extend_into(self, target: dict, base: dict, ext: dict) -> None:
        if base["t"] != "rec":
            target.update(base)
            target["filling"] = False
            return
        if base.get("filling"):
            base.setdefault("pending_exts", []).append((target, ext))
            return
        target["open"] = base.get("open")
        target["tail"] = base.get("tail") if base.get("tail") is not None else target.get("tail")
        ctx_decls = list(base.get("ctx_decls") or [])
        for cd in ext.get("ctx_decls") or []:
            idx = next((i for i, x in enumerate(ctx_decls) if x["variable"] == cd["variable"]), -1)
            if idx >= 0:
                ctx_decls[idx] = cd
            else:
                ctx_decls.append(cd)
        target["ctx_decls"] = ctx_decls if ctx_decls else None
        target["members"] = [dict(m) for m in base["members"]]
        for om in ext["members"]:
            idx = next((i for i, m in enumerate(target["members"]) if m["name"] == om["name"]), -1)
            if idx >= 0:
                target["members"][idx] = om
            else:
                target["members"].append(om)
        target["asserts"] = list(base["asserts"]) + list(ext["asserts"])
        self.complete_record(target)

    # a record's members are final: extensions that waited on it merge now
    def complete_record(self, rt: dict) -> None:
        rt["filling"] = False
        pending = rt.pop("pending_exts", None) or []
        for target, ext in pending:
            self.extend_into(target, rt, ext)

    # §3.6: `${T}` inside a pattern splices another type — a string-shaped
    # T (pattern, string literal, union of those) as its regular language,
    # an integer-shaped T (int literal, int range, union) as the decimal
    # representations of its members
    def expand_pattern(self, re_: str) -> str:
        visiting = self.pattern_visiting

        def arm_fragment(arm: str, text: str) -> str:
            m = re.fullmatch(r'"((?:[^"\\]|\\.)*)"', arm)
            if m:
                return self.pattern_fragment({"t": "lit", "v": json.loads(f'"{m.group(1)}"')}, text)
            m = re.fullmatch(r"(-?[0-9]+)\.\.(<?)(-?[0-9]+)", arm)
            if m:
                return self.pattern_fragment({"t": "range", "base": "int", "lo": int(m.group(1)), "hi": int(m.group(3)),
                                              "excl": m.group(2) == "<"}, text)
            if re.fullmatch(r"-?[0-9]+", arm):
                return self.pattern_fragment({"t": "lit", "v": int(arm)}, text)
            if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.]*", arm):
                raise RuntimeError(f"pattern interpolation of {text}: not a type (§3.6)")
            if arm in visiting:
                raise RuntimeError(f"pattern interpolation of {arm} is circular")
            visiting.add(arm)
            try:
                rt = self.resolve({"k": "named", "name": arm, "args": [], "preds": None, "ext": None})
            except Exception as e:
                visiting.discard(arm)
                if str(e).startswith("unknown type"):
                    raise RuntimeError(f"pattern interpolation of {arm}: unknown type")
                raise
            visiting.discard(arm)
            return self.pattern_fragment(rt, arm)

        def splice(m: "re.Match") -> str:
            text = m.group(1).strip()
            # the spliced type: a union of string literals, int literals, int
            # ranges, and named types — the type-expression subset that fits
            # inside a pattern token
            frags = [arm_fragment(arm.strip(), text) for arm in text.split("|")]
            return frags[0] if len(frags) == 1 else f"(?:{'|'.join(frags)})"

        return re.sub(r"\$\{([^}]*)\}", splice, re_)

    def pattern_fragment(self, rt: dict, name: str) -> str:
        def esc(s: str) -> str:
            return re.sub(r"[.*+?^${}()|\[\]\\/]", lambda m: "\\" + m.group(0), s)

        def bad():
            raise RuntimeError(f"pattern interpolation of {name}: type is neither string- nor integer-shaped (§3.6)")

        t = rt["t"]
        if t == "pattern":
            return f"(?:{rt['src']})"
        if t == "lit":
            if is_str(rt["v"]):
                return esc(rt["v"])
            if is_int(rt["v"]):
                return str(rt["v"])
            bad()
        if t == "range":
            if rt["base"] != "int" or not is_int(rt["lo"]) or not is_int(rt["hi"]):
                bad()
            hi = rt["hi"] - 1 if rt["excl"] else rt["hi"]
            if hi - rt["lo"] >= 65536:
                raise RuntimeError(f"pattern interpolation of {name}: range too large (limit 65536 values)")
            return "(?:" + "|".join(str(v) for v in range(rt["lo"], hi + 1)) + ")"
        if t == "union":
            return "(?:" + "|".join(self.pattern_fragment(a, name) for a in rt["arms"]) + ")"
        if t == "pred":
            return self.pattern_fragment(rt["base"], name)
        if t == "prim":
            if rt["name"] == "string":
                return ".*"
            if rt["name"] == "int":
                return "-?[0-9]+"
            bad()
        bad()

    # §3.15 generics
    def instantiate(self, ast: dict, decl: dict) -> dict:
        from .subsume import subsumes
        ps = decl["params"]
        args = ast.get("args") or []
        if len(args) != len(ps):
            raise RuntimeError(f"generic arity: {ast['name']} expects {len(ps)} argument(s), got {len(args)}")
        types: dict = {}
        values: dict = {}
        label = []
        for p, a in zip(ps, args):
            if p.get("type"):
                if a["k"] == "lit":
                    v = a["v"]
                elif a["k"] == "named" and not a.get("args") and not a.get("ext") and not a.get("preds"):
                    v = self.const_num(a["name"])
                    if is_str(v):
                        raise RuntimeError(f"non-constant value argument {a['name']} for {p['name']} of {ast['name']}")
                else:
                    raise RuntimeError(f"generic arity: parameter {p['name']} of {ast['name']} takes a constant value")
                bound = self.resolve(self.subst_type(p["type"], types, values))
                if not subsumes(self, {"t": "lit", "v": v}, bound):
                    raise RuntimeError(f"value argument {v} outside parameter {p['name']}'s type in {ast['name']}")
                values[p["name"]] = v
                label.append(str(v))
            else:
                types[p["name"]] = a
                label.append(a["name"] if a["k"] in ("named", "prim") else a["k"])
        key = f"{ast['name']}<{_json_key(args)}>"
        if key in self.type_memo:
            return self.type_memo[key]
        shown = f"{ast['name']}<{', '.join(label)}>"
        body = self.subst_type(decl["ast"], types, values)
        if body["k"] == "record":
            rt = {"t": "rec", "name": shown, "members": [], "asserts": [], "open": body["open"], "tail": decl.get("tail")}
            self.type_memo[key] = rt
            try:
                self.fill_record(rt, body["members"])
            except BaseException:
                self.type_memo.pop(key, None)
                raise
        else:
            rt = self.resolve(body, shown)
            if rt["t"] in ("rec", "union"):
                rt["name"] = shown
            if rt.get("tail") is None:
                rt["tail"] = decl.get("tail")
            self.type_memo[key] = rt
        return rt

    def subst_type(self, ast: dict, types: dict, values: dict) -> dict:
        t = lambda a: self.subst_type(a, types, values)
        k = ast["k"]
        if k == "named":
            plain = not ast.get("args") and not ast.get("ext") and not ast.get("preds")
            if plain and ast["name"] in types:
                return types[ast["name"]]
            if plain and ast["name"] in values:
                return {"k": "lit", "v": values[ast["name"]]}
            return {**ast, "args": [t(a) for a in (ast.get("args") or [])],
                    "ext": t(ast["ext"]) if ast.get("ext") else None,
                    "preds": [subst_expr(p, values) for p in ast["preds"]] if ast.get("preds") else None}
        if k == "range":
            sub = lambda v: values[v] if is_str(v) and v in values else v
            return {**ast, "lo": sub(ast["lo"]), "hi": sub(ast["hi"])}
        if k == "array":
            sub = lambda v: int(values[v]) if is_str(v) and v in values else v
            return {**ast, "elem": t(ast["elem"]), "lo": sub(ast.get("lo")), "hi": sub(ast.get("hi"))}
        if k == "record":
            return {**ast, "members": [self.subst_member(m, types, values) for m in ast["members"]]}
        if k == "map":
            return {**ast, "key": t(ast["key"]), "val": t(ast["val"])}
        if k in ("union", "isect"):
            return {**ast, "arms": [t(a) for a in ast["arms"]]}
        if k == "func":
            return {**ast, "params": [t(p) for p in ast["params"]], "ret": t(ast["ret"])}
        return ast

    def subst_member(self, m: dict, types: dict, values: dict) -> dict:
        t = lambda a: self.subst_type(a, types, values)
        k = m["m"]
        if k == "value":
            return {**m, "type": t(m["type"]), "dflt": subst_expr(m["dflt"], values) if m.get("dflt") else None}
        if k == "derived":
            return {**m, "type": t(m["type"]) if m.get("type") else None, "expr": subst_expr(m["expr"], values)}
        if k == "context":
            return {**m, "type": t(m["type"])}
        if k == "assert":
            return {**m, "cond": subst_expr(m["cond"], values)}
        if k == "when":
            return {**m, "cond": subst_expr(m["cond"], values),
                    "body": [self.subst_member(b, types, values) for b in m["body"]]}
        return m

    def fill_record(self, rt: dict, members: list) -> None:
        rt["filling"] = True
        for m in members:
            k = m["m"]
            if k == "value":
                rt["members"].append({"kind": "dflt" if m.get("dflt") else "opt" if m.get("opt") else "req",
                                      "name": m["name"], "type": self.resolve(m["type"]),
                                      "dflt": m.get("dflt"), "menv": self})
            elif k == "derived":
                rt["members"].append({"kind": "der", "name": m["name"],
                                      "type": self.resolve(m["type"]) if m.get("type") else None,
                                      "expr": m["expr"], "menv": self,
                                      "hidden": True if m.get("hidden") else None})
            elif k == "assert":
                rt["asserts"].append({"kind": "assert", "name": m["name"], "cond": m["cond"],
                                      "tail": m.get("tail"), "origin": rt.get("name"), "menv": self})
            elif k == "when":
                rt["asserts"].append({"kind": "when", "cond": m["cond"], "body": m["body"],
                                      "origin": rt.get("name"), "menv": self})
            elif k == "context":
                rt.setdefault("ctx_decls", []).append({"variable": m["variable"], "type": self.resolve(m["type"]), "menv": self})
        self.complete_record(rt)

    def merge_isect(self, arms: list, name: Optional[str]) -> dict:
        recs = [a for a in arms if a["t"] == "rec"]
        if len(recs) != len(arms):
            return {"t": "isectN", "arms": arms}
        merged = {"t": "rec", "name": name, "open": all(r.get("open") for r in recs), "tail": None,
                  "members": [], "asserts": []}
        for r in recs:
            for m in r["members"]:
                idx = next((i for i, x in enumerate(merged["members"]) if x["name"] == m["name"]), -1)
                if idx >= 0:
                    prev = merged["members"][idx]
                    merged["members"][idx] = {**prev, "conj": (prev.get("conj") or [prev["type"]]) + [m["type"]],
                                              "kind": "req" if m["kind"] == "req" else prev["kind"]}
                else:
                    merged["members"].append(dict(m))
            merged["asserts"].extend({**a, "origin": a.get("origin") or r.get("name")} for a in r["asserts"])
        return merged


def _json_key(v: Any) -> str:
    return json.dumps(v, default=lambda o: f"{o}n" if is_int(o) else str(o), sort_keys=False)


# ---------------- helpers ----------------
def subst_expr(e: Any, values: dict) -> Any:
    """Deep-copy an expression substituting generic value parameters."""
    if not isinstance(e, dict):
        if isinstance(e, list):
            return [subst_expr(x, values) if isinstance(x, (dict, list)) else x for x in e]
        return e
    if e.get("e") == "name" and e["name"] in values:
        return {"e": "lit", "v": values[e["name"]]}
    out: dict = {}
    for k, v in e.items():
        if isinstance(v, list):
            out[k] = [subst_expr(x, values) if isinstance(x, (dict, list)) else x for x in v]
        elif isinstance(v, dict):
            out[k] = subst_expr(v, values)
        else:
            out[k] = v
    return out


_ID_RE = re.compile(r"^[_A-Za-z][_A-Za-z0-9]*$")


# ---------------- patterns: the portable core (§3.6) ----------------
# A pattern body is validated against the specification's regular-
# expression core with one fixed set of messages, so every implementation
# reports the same text whatever engine runs the accepted patterns.
# Returns the reason a body is outside the core, or None when it is inside.
PATTERN_PUNCT = "\\/.*+?()[]{}|^$-"


def _pattern_escape(cs: list, pos: list) -> int:
    """the escape at pos[0]: the code point it stands for (-1 for a class
    escape), or raises ValueError with the reason"""
    i = pos[0]
    if i + 1 >= len(cs):
        raise ValueError("trailing backslash")
    e = cs[i + 1]
    pos[0] = i + 2
    if e in "dwsDWS":
        return -1
    if e == "n":
        return 10
    if e == "t":
        return 9
    if e == "r":
        return 13
    if e in PATTERN_PUNCT:
        return ord(e)
    if e.isdigit():
        raise ValueError(f"backreference \\{e} is not supported")
    raise ValueError(f"unsupported escape \\{e}")


def pattern_error(src: str) -> Optional[str]:
    cs = list(src)
    n = len(cs)
    pos = [0]
    depth = 0
    can_repeat = False
    try:
        while pos[0] < n:
            c = cs[pos[0]]
            if c == "\\":
                _pattern_escape(cs, pos)
                can_repeat = True
            elif c == "[":
                pos[0] += 1
                if pos[0] < n and cs[pos[0]] == "^":
                    pos[0] += 1
                items = 0
                while True:
                    if pos[0] >= n:
                        return "unterminated character class"
                    if cs[pos[0]] == "]":
                        pos[0] += 1
                        break
                    if cs[pos[0]] == "\\":
                        lo = _pattern_escape(cs, pos)
                    else:
                        lo = ord(cs[pos[0]])
                        pos[0] += 1
                    if pos[0] < n and cs[pos[0]] == "-" and pos[0] + 1 < n and cs[pos[0] + 1] != "]":
                        pos[0] += 1
                        if cs[pos[0]] == "\\":
                            hi = _pattern_escape(cs, pos)
                        else:
                            hi = ord(cs[pos[0]])
                            pos[0] += 1
                        if lo < 0 or hi < 0 or lo > hi:
                            return "invalid range in character class"
                    items += 1
                if items == 0:
                    return "empty character class"
                can_repeat = True
            elif c == "]":
                return "unbalanced bracket"
            elif c == "(":
                pos[0] += 1
                if pos[0] < n and cs[pos[0]] == "?":
                    if pos[0] + 1 < n and cs[pos[0] + 1] == ":":
                        pos[0] += 2
                    else:
                        return "unsupported construct (?"
                depth += 1
                can_repeat = False
            elif c == ")":
                if depth == 0:
                    return "unbalanced parenthesis"
                depth -= 1
                pos[0] += 1
                can_repeat = True
            elif c == "|":
                pos[0] += 1
                can_repeat = False
            elif c in "*+?":
                if not can_repeat:
                    return "nothing to repeat"
                pos[0] += 1
                can_repeat = False
            elif c == "{":
                if not can_repeat:
                    return "nothing to repeat"
                m = re.match(r"\{([0-9]+)(?:(,)([0-9]*))?\}", src[pos[0]:])
                if not m:
                    return "malformed repetition"
                if m.group(3) and int(m.group(3)) < int(m.group(1)):
                    return "malformed repetition"
                pos[0] += len(m.group(0))
                can_repeat = False
            elif c == "}":
                return "malformed repetition"
            elif c in "^$":
                pos[0] += 1
                can_repeat = False
            else:
                pos[0] += 1
                can_repeat = True
    except ValueError as e:
        return str(e)
    return "unbalanced parenthesis" if depth > 0 else None


def compile_pattern(src: str):
    return re.compile(f"(?:{src})")


def path_str(segs: list, rel_root: Optional[str] = None) -> str:
    out = ""
    for i, s in enumerate(segs):
        if i == 0:
            out += "$" if (rel_root is not None and s == rel_root) else str(seg_text(s))
        elif is_int(s):
            out += f"[{s}]"
        elif isinstance(s, Key):
            out += f"[{json.dumps(s.k, ensure_ascii=False)}]"
        elif dot_spellable(s):
            out += f".{s}"
        else:
            out += f"[{json.dumps(s, ensure_ascii=False)}]"
    return out


def parse_path(s: str, root_name: str) -> list:
    """A path string from a document: `.name` is a member, `["…"]` a
    bracketed segment (a map key, or a member the dot cannot spell — the
    canonical walk, §7.5, decides which is legal where), `[n]` an index."""
    segs: list = []
    i = 0
    if s[:1] == "$":
        segs.append(root_name)
        i = 1
    else:
        m = re.match(r"[_A-Za-z][_A-Za-z0-9]*", s)
        if not m:
            raise EvalErr(f"bad path {s}")
        segs.append(m.group(0))
        i = len(m.group(0))
    while i < len(s):
        if s[i] == ".":
            m = re.match(r"[_A-Za-z][_A-Za-z0-9]*", s[i + 1:])
            if not m:
                raise EvalErr(f"bad path {s}")
            segs.append(m.group(0))
            i += 1 + len(m.group(0))
        elif s[i] == "[":
            j = s.index("]", i)
            inner = s[i + 1:j]
            segs.append(Key(json.loads(inner)) if inner.startswith('"') else int(inner))
            i = j + 1
        else:
            raise EvalErr(f"bad path {s}")
    return segs


def cmp_path(a: list, b: list) -> int:
    """Canonical path order (§7.2): segment-wise, indices numerically,
    names and keys lexicographically, a prefix first."""
    for x, y in zip(a, b):
        x, y = seg_text(x), seg_text(y)
        if is_int(x) and is_int(y):
            if x != y:
                return x - y
        elif str(x) != str(y):
            return -1 if str(x) < str(y) else 1
    return len(a) - len(b)


def sort_diags(diags: list) -> list:
    """§6.7: evaluation- and validation-time diagnostics sort by (path, id),
    path in canonical order; stable."""
    import functools

    def segs_of(p: str) -> list:
        try:
            return parse_path(p, "") if p else []
        except Exception:
            return [p]

    items = [(d, i, segs_of(d.get("path") or "")) for i, d in enumerate(diags)]

    def cmp(a, b) -> int:
        c = cmp_path(a[2], b[2])
        if c:
            return c
        ai, bi = a[0].get("id") or "", b[0].get("id") or ""
        if ai != bi:
            return -1 if ai < bi else 1
        return a[1] - b[1]
    return [x[0] for x in sorted(items, key=functools.cmp_to_key(cmp))]


def _place_of(v: Any) -> Optional[list]:
    if isinstance(v, Ref):
        return v.segs
    if isinstance(v, (RecInst, ArrV, MapV)):
        return v.path
    return None


def value_eq(a: Any, b: Any) -> bool:
    pa, pb = _place_of(a), _place_of(b)
    if (isinstance(a, Ref) or isinstance(b, Ref)) and pa is not None and pb is not None:
        return cmp_path(pa, pb) == 0
    if is_int(a) and is_int(b):
        return a == b
    if is_float(a) and is_float(b):
        return a == b
    if isinstance(a, Quantity) and isinstance(b, Quantity):
        return a.dim == b.dim and a.value == b.value
    if isinstance(a, ArrV) and isinstance(b, ArrV):
        return len(a.items) == len(b.items) and all(value_eq(x, y) for x, y in zip(a.items, b.items))
    if isinstance(a, MapV) and isinstance(b, MapV):
        if len(a.entries) != len(b.entries):
            return False
        return all(k in b.entries and value_eq(v, b.entries[k]) for k, v in a.entries.items())
    if isinstance(a, RecInst) and isinstance(b, RecInst):
        for n, s in a.slots.items():
            if s.hidden:
                continue   # a hidden member is not part of the value (D34)
            s2 = b.slots.get(n)
            v1 = ABSENT if s.state == "absent" else s.value
            v2 = ABSENT if (s2 is None or s2.state == "absent") else s2.value
            if v1 is ABSENT and v2 is ABSENT:
                continue
            if v1 is ABSENT or v2 is ABSENT:
                return False
            if not value_eq(v1, v2):
                return False
        return True
    # strict same-kind scalar equality (no int/float or bool/int crossover)
    if type(a) is not type(b):
        return False
    return a == b


def mentions_referrers(e: Any) -> bool:
    if isinstance(e, list):
        return any(mentions_referrers(x) for x in e)
    if not isinstance(e, dict):
        return False
    if e.get("e") == "referrers":
        return True
    return any(mentions_referrers(v) for v in e.values() if isinstance(v, (dict, list)))


# ---------------- lexical JSON (int/float by lexeme) ----------------
_NUM_RE = re.compile(r"-?(?:0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?")


def read_json(src: str) -> Any:
    i = 0
    n = len(src)

    def ws() -> None:
        nonlocal i
        while i < n and src[i] in " \t\r\n":
            i += 1

    def string() -> str:
        nonlocal i
        j = i + 1
        out = []
        while src[j] != '"':
            if src[j] == "\\":
                e = src[j + 1]
                if e == "n":
                    out.append("\n"); j += 2
                elif e == "t":
                    out.append("\t"); j += 2
                elif e == "r":
                    out.append("\r"); j += 2
                elif e == "b":
                    out.append("\b"); j += 2
                elif e == "f":
                    out.append("\f"); j += 2
                elif e == "u":
                    out.append(chr(int(src[j + 2:j + 6], 16))); j += 6
                else:
                    out.append(e); j += 2
            else:
                out.append(src[j]); j += 1
        i = j + 1
        return "".join(out)

    def val() -> Any:
        nonlocal i
        ws()
        c = src[i]
        if c == "{":
            i += 1
            entries: list = []
            ws()
            if src[i] == "}":
                i += 1
                return JObj(entries)
            while True:
                ws()
                k = string()
                ws()
                i += 1  # ':'
                entries.append((k, val()))
                ws()
                if src[i] == ",":
                    i += 1
                    continue
                i += 1
                return JObj(entries)
        if c == "[":
            i += 1
            items: list = []
            ws()
            if src[i] == "]":
                i += 1
                return items
            while True:
                items.append(val())
                ws()
                if src[i] == ",":
                    i += 1
                    continue
                i += 1
                return items
        if c == '"':
            return string()
        if src.startswith("true", i):
            i += 4
            return True
        if src.startswith("false", i):
            i += 5
            return False
        if src.startswith("null", i):
            i += 4
            return None
        m = _NUM_RE.match(src, i)
        if not m:
            raise EvalErr(f"bad JSON at {i}")
        i = m.end()
        return float(m.group(0)) if (m.group(1) or m.group(2)) else int(m.group(0))

    v = val()
    ws()
    if i < n:
        raise EvalErr("bad JSON: trailing characters")
    return v


# ---------------- JS-compatible number printing ----------------
def js_num_str(x: float) -> str:
    """ECMAScript Number::toString for finite doubles (shortest round trip)."""
    if x == 0:
        return "0"
    from decimal import Decimal
    sign, digs, exp = Decimal(repr(x)).as_tuple()
    digits = list(digs)
    while len(digits) > 1 and digits[-1] == 0:
        digits.pop()
        exp += 1
    k = len(digits)
    n = k + exp
    ds = "".join(str(d) for d in digits)
    if k <= n <= 21:
        body = ds + "0" * (n - k)
    elif 0 < n <= 21:
        body = ds[:n] + "." + ds[n:]
    elif -6 < n <= 0:
        body = "0." + "0" * (-n) + ds
    else:
        e = n - 1
        mant = ds[0] + ("." + ds[1:] if k > 1 else "")
        body = f"{mant}e{'+' if e > 0 else '-'}{abs(e)}"
    return ("-" if sign else "") + body


def json_str(s: str) -> str:
    return json.dumps(s, ensure_ascii=False)
