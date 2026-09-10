"""Shared executable expressions; no operation captures a record instance."""

from __future__ import annotations

from collections.abc import Callable
from typing import TYPE_CHECKING, Any

from decl.semantics import (
    ABSENT,
    ArrV,
    Closure,
    EvalErr,
    Key,
    MapV,
    NsRef,
    Pattern,
    PreArr,
    PreObj,
    PreVal,
    Quantity,
    RecInst,
    Scope,
    Segs,
    StdRef,
    is_int,
    is_str,
    mentions_referrers,
)

if TYPE_CHECKING:
    from decl.engine import Engine

Op = Callable[["Engine", Scope], Any]


class Programs:
    def __init__(self) -> None:
        self.expressions: dict[int, tuple[Any, Op]] = {}
        self.callees: dict[int, tuple[Any, Op]] = {}
        self.navigations: dict[int, tuple[Any, Op]] = {}
        self.compiled = 0
        self.compiled_schemas = 0
        self.schemas: dict[int, tuple[Any, Any, list[Any], set[str]]] = {}

    def schema(self, rt: Any) -> tuple[list[Any], set[str]]:
        hit = self.schemas.get(id(rt))
        if hit is not None and hit[1] is rt["members"] and len(hit[2]) == len(rt["members"]):
            return hit[2], hit[3]
        plans = [
            (
                m,
                m.get("conj") or [m["type"]],
                self.get(m["expr"]) if m.get("expr") else None,
                self.get(m["dflt"]) if m.get("dflt") else None,
                mentions_referrers(m.get("expr") if m["kind"] == "der" else m.get("dflt")),
            )
            for m in rt["members"]
        ]
        names = {m["name"] for m in rt["members"]}
        self.schemas[id(rt)] = (rt, rt["members"], plans, names)
        self.compiled_schemas += 1
        return plans, names

    def get(self, e: dict[str, Any]) -> Op:
        hit = self.expressions.get(id(e))
        if hit is not None:
            return hit[1]
        op = self.compile(e)
        self.expressions[id(e)] = (e, op)
        self.compiled += 1
        return op

    def callee(self, e: dict[str, Any]) -> Op:
        hit = self.callees.get(id(e))
        if hit is not None:
            return hit[1]
        op: Op
        if e["e"] == "member":
            base, name = self.callee(e["x"]), e["name"]

            def member_callee(eng: Engine, sc: Scope) -> Any:
                x = base(eng, sc)
                if isinstance(x, StdRef):
                    return StdRef([*x.path, name])
                if isinstance(x, NsRef):
                    return eng.ns_value(x, name, sc)
                return eng.access(eng.deref(x), name)

            op = member_callee
        else:
            op = self.get(e)
        self.callees[id(e)] = (e, op)
        return op

    def nav(self, e: dict[str, Any]) -> Op:
        hit = self.navigations.get(id(e))
        if hit is not None:
            return hit[1]
        kind = e["e"]
        if kind == "paren":
            op = self.nav(e["x"])
        elif kind == "if":
            c, t, f = self.get(e["c"]), self.nav(e["t"]), self.nav(e["f"])

            def conditional(eng: Engine, sc: Scope) -> Any:
                return t(eng, sc) if eng.truthy(c(eng, sc)) else f(eng, sc)

            op = conditional
        elif kind == "member":
            base, name = self.nav(e["x"]), e["name"]

            def member(eng: Engine, sc: Scope) -> Any:
                raw = base(eng, sc)
                if isinstance(raw, Segs):
                    return Segs([*raw.segs, name])
                x = eng.deref(raw)
                v = eng.access(x, name)
                return Segs([*x.path, name]) if v is ABSENT and isinstance(x, RecInst) else v

            op = member
        elif kind == "index":
            base, index, ordinary = self.nav(e["x"]), self.get(e["i"]), self.get(e)

            def indexed(eng: Engine, sc: Scope) -> Any:
                raw, i = base(eng, sc), index(eng, sc)
                seg = int(i) if is_int(i) else Key(i)
                if isinstance(raw, Segs):
                    return Segs([*raw.segs, seg])
                x = eng.deref(raw)
                if isinstance(x, ArrV):
                    n = int(i)
                    return x.items[n] if 0 <= n < len(x.items) else Segs([*x.path, n])
                if isinstance(x, MapV):
                    return x.entries[i] if i in x.entries else Segs([*x.path, Key(i)])
                if isinstance(x, RecInst):
                    v = eng.access(x, i)
                    return Segs([*x.path, i]) if v is ABSENT else v
                return ordinary(eng, sc)

            op = indexed
        else:
            op = self.get(e)
        self.navigations[id(e)] = (e, op)
        return op

    def compile(self, e: dict[str, Any]) -> Op:
        k = e["e"]
        if k == "lit":
            v = e["v"]
            return lambda eng, sc: v
        if k == "pattern":
            return lambda eng, sc: Pattern(e["re"])
        if k == "unitlit":

            def unit(eng: Engine, sc: Scope) -> Any:
                try:
                    u = eng.env.unit_info(e["unit"])
                except RuntimeError as error:
                    raise EvalErr(str(error)) from error
                return Quantity(u["key"], e["num"] * u["to_base"])

            return unit
        if k == "paren":
            return self.get(e["x"])
        if k == "name":
            name = e["name"]
            return lambda eng, sc: eng.name_value(name, sc)
        if k == "ctx":
            name = e["name"]
            return lambda eng, sc: eng.context_value(name, sc)
        if k == "referrers":
            return lambda eng, sc: eng.referrers(e["type"], e["member"], sc)
        if k == "obj":
            entries = e["entries"]
            return lambda eng, sc: PreObj([(en["key"], PreVal(en["val"], sc)) for en in entries])
        if k == "arr":
            items = e["items"]
            return lambda eng, sc: PreArr([(it["spread"], PreVal(it["expr"], sc)) for it in items])
        if k == "spread":

            def spread(eng: Engine, sc: Scope) -> Any:
                raise EvalErr("spread outside an object literal")

            return spread
        if k == "template":
            parts = [p if is_str(p) else self.get(p) for p in e["parts"]]
            return lambda eng, sc: "".join(
                p if isinstance(p, str) else eng.to_str(p(eng, sc)) for p in parts
            )
        if k == "if":
            c, t, f = self.get(e["c"]), self.get(e["t"]), self.get(e["f"])
            return lambda eng, sc: t(eng, sc) if eng.truthy(c(eng, sc)) else f(eng, sc)
        if k == "un":
            x, operator = self.get(e["x"]), e["op"]

            def unary(eng: Engine, sc: Scope) -> Any:
                v = x(eng, sc)
                if operator == "!":
                    return not eng.truthy(v)
                if operator == "-":
                    if v is ABSENT:
                        raise EvalErr("absent consumed")
                    return Quantity(v.dim, -v.value) if isinstance(v, Quantity) else -v
                if operator == "~":
                    return ~v
                raise EvalErr("un")

            return unary
        if k == "bin":
            operator = e["op"]
            if operator == "|>":
                r = e["r"]
                return self.get(
                    {
                        "e": "call",
                        "fn": r["fn"] if r["e"] == "call" else r,
                        "args": [e["l"], *(r["args"] if r["e"] == "call" else [])],
                    }
                )
            l, r = self.get(e["l"]), self.get(e["r"])
            if operator == "&&":
                return lambda eng, sc: eng.truthy(r(eng, sc)) if eng.truthy(l(eng, sc)) else False
            if operator == "||":
                return lambda eng, sc: True if eng.truthy(l(eng, sc)) else eng.truthy(r(eng, sc))
            if operator == "??":

                def coalesce(eng: Engine, sc: Scope) -> Any:
                    v = l(eng, sc)
                    return r(eng, sc) if v is ABSENT or v is None else v

                return coalesce
            return lambda eng, sc: eng.apply_bin(operator, l(eng, sc), r(eng, sc))
        if k == "member":
            base, name, safe = self.get(e["x"]), e["name"], e.get("safe", False)

            def access(eng: Engine, sc: Scope) -> Any:
                v = base(eng, sc)
                if isinstance(v, NsRef):
                    return eng.ns_value(v, name, sc)
                if safe and (v is None or v is ABSENT):
                    return ABSENT
                return eng.access(eng.deref(v), name)

            return access
        if k == "index":
            base, index = self.get(e["x"]), self.get(e["i"])

            def at(eng: Engine, sc: Scope) -> Any:
                v, i = eng.mat_val(base(eng, sc)), index(eng, sc)
                if isinstance(v, ArrV):
                    n = int(i)
                    if n < 0 or n >= len(v.items):
                        raise EvalErr(f"index {n} out of bounds", "E5005")
                    return v.items[n]
                if isinstance(v, MapV):
                    return v.entries.get(i, ABSENT)
                if isinstance(v, RecInst):
                    return eng.access(v, i)
                raise EvalErr("index on non-collection")

            return at
        if k == "call":
            args, fn = [self.get(a) for a in e["args"]], self.callee(e["fn"])

            def call(eng: Engine, sc: Scope) -> Any:
                values = [a(eng, sc) for a in args]
                return eng.call(fn(eng, sc), values, sc)

            return call
        if k == "lambda":
            self.get(e["body"])
            return lambda eng, sc: Closure(e["params"], e["body"], sc)
        if k == "with":
            base, patch = self.get(e["base"]), self.get(e["patch"])

            def patched(eng: Engine, sc: Scope) -> Any:
                v = eng.deref(base(eng, sc))
                if not isinstance(v, (PreObj, MapV, RecInst)):
                    raise EvalErr("with on non-record")
                return eng.with_value(v, patch(eng, sc))

            return patched
        if k == "match":
            subject = self.get(e["subject"])
            arms = [{**a, "op": self.get(a["body"])} for a in e["arms"]]

            def match(eng: Engine, sc: Scope) -> Any:
                v = eng.deref(subject(eng, sc))

                def run(arm: Any) -> Any:
                    locals_ = dict(sc.locals)
                    locals_[arm["v"]] = v
                    return arm["op"](eng, sc.with_locals(locals_))

                fallback = None
                for arm in arms:
                    if arm.get("type") is None:
                        fallback = arm
                    elif eng.member_of(v, (sc.menv or eng.env).resolve(arm["type"]), sc):
                        return run(arm)
                if fallback is not None:
                    return run(fallback)
                raise EvalErr("match: no arm matched")

            return match
        if k in ("comp", "mapcomp"):
            clauses = [
                (c["v"], self.get(c["iter"]), [self.get(f) for f in c["filters"]])
                for c in e["clauses"]
            ]
            if k == "comp":
                self.get(e["head"])
            key = self.get(e["key"]) if k == "mapcomp" else None
            val = self.get(e["val"]) if k == "mapcomp" else None

            def comprehension(eng: Engine, sc: Scope) -> Any:
                entries: list[Any] = []
                items: list[Any] = []

                def visit(ci: int, scope: Scope) -> None:
                    if ci == len(clauses):
                        if k == "comp":
                            items.append((False, PreVal(e["head"], scope)))
                        else:
                            assert key is not None and val is not None
                            n = key(eng, scope)
                            if not is_str(n):
                                raise EvalErr("map key must be string")
                            if any(kk == n for kk, _ in entries):
                                raise EvalErr(f"duplicate key {n}", "E5004")
                            entries.append((n, val(eng, scope)))
                        return
                    name, iterator, filters = clauses[ci]
                    for item in eng.iterate(iterator(eng, scope)):
                        locals_ = dict(scope.locals)
                        locals_[name] = item
                        nxt = scope.with_locals(locals_)
                        if all(eng.truthy(f(eng, nxt)) for f in filters):
                            visit(ci + 1, nxt)

                visit(0, sc)
                return PreArr(items) if k == "comp" else PreObj(entries)

            return comprehension
        raise EvalErr(f"ev: unhandled {k}")
