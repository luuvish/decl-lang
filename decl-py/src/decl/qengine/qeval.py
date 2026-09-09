"""The decl layer of the query engine (qengine/DESIGN.md), the port of
decl-ts/src/qengine/qeval.ts and decl-rs/src/qengine/qeval.rs: compile
expressions to query computes and evaluate a universe's outputs through the
incremental core (db.py). Value semantics — equality, serialization, type
binding, arithmetic — are reused from the value layer (the ``Engine`` helper);
only the evaluation strategy is new. A form not yet compiled raises
``Unsupported`` so the caller falls back to the tree walker.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from decl.engine import _UNDEF, Engine
from decl.qengine.db import Db, QCycle
from decl.semantics import (
    ABSENT,
    ArrV,
    Closure,
    DeferSig,
    EvalErr,
    MapV,
    NsRef,
    Pattern,
    Quantity,
    RangeV,
    RecInst,
    Ref,
    Scope,
    Segs,
    Slot,
    StdRef,
    Taint,
    compile_pattern,
    is_int,
    is_str,
    path_str,
    pattern_error,
    seg_text,
    sort_diags,
    value_eq,
)
from decl.semantics import (
    Key as KeySeg,
)

# an expression or member form this stage of the port does not compile yet
QUNSUP = "__QUNSUP__"


class Unsupported(Exception):
    def __init__(self, what: str) -> None:
        super().__init__(what)
        self.what = what


def _unsup(msg: str) -> EvalErr:
    """the value-level error that signals a runtime-reached unhandled form; the
    caller turns its sentinel code back into a fall back to the tree walker"""
    return EvalErr(f"qeval unsupported: {msg}", QUNSUP)


# comprehension loop variables in scope
Locals = dict[str, Any]
# a compiled expression: a callable over the query engine and local bindings
Op = Callable[["QEval", Locals], Any]


class CCtx:
    """the lexical context a compile happens in"""

    __slots__ = ("entry_env", "locals", "menv", "root_name", "self_inst")

    def __init__(
        self, self_inst: Any, root_name: str, locals_: set[str], menv: Any, entry_env: Any
    ) -> None:
        self.self_inst = self_inst  # the record instance being constructed (§4.3)
        self.root_name = root_name  # the enclosing evaluation root ($root, ctx)
        self.locals = locals_  # comprehension loop variables in scope (a set)
        self.menv = menv  # the module scope (consts, funcs, unit table)
        self.entry_env = entry_env  # the universe entry (its consts are query-native)


def _with_locals(c: CCtx, locals_: set[str]) -> CCtx:
    return CCtx(c.self_inst, c.root_name, locals_, c.menv, c.entry_env)


def _has_rec(t: dict[str, Any], seen: set[int]) -> bool:
    """does a type contain a record anywhere (so its values live in the query
    graph)? `seen` breaks the cycle of a type that names itself (§3.1)"""
    if id(t) in seen:
        return False
    seen.add(id(t))
    k = t["t"]
    if k == "rec":
        return True
    if k == "arr":
        return _has_rec(t["elem"], seen)
    if k == "map":
        return _has_rec(t["val"], seen)
    if k == "union":
        return any(_has_rec(a, seen) for a in t["arms"])
    return False


def _apply_un(q: QEval, op: str, x: Any) -> Any:
    if op == "!":
        return not q.helper.truthy(x)
    if op == "-":
        if x is ABSENT:
            raise EvalErr("absent consumed")
        if isinstance(x, Quantity):
            return Quantity(x.dim, -x.value)
        return -x
    if op == "~":
        return ~x
    raise EvalErr("un")


# ---- comprehension / match helpers -----------------------------------------


def _compile_clauses(clauses: list[Any], all_vars: list[str], c: CCtx) -> list[Any]:
    """each clause sees the prior loop variables in its range, and its own
    variable in its filters (§4.8)"""
    out = []
    for i, cl in enumerate(clauses):
        prior = set(c.locals) | set(all_vars[:i])
        iter_op = compile(cl["iter"], _with_locals(c, prior))
        with_this = prior | {cl["v"]}
        filters = [compile(f, _with_locals(c, with_this)) for f in cl["filters"]]
        out.append((cl["v"], iter_op, filters))
    return out


def _run_comp(q: QEval, ccs: list[Any], i: int, loc: Locals, head: Op, out: list[Any]) -> None:
    if i == len(ccs):
        out.append(head(q, loc))
        return
    var, iter_op, filters = ccs[i]
    for el in q.helper.iterate(q.helper.mat_val(iter_op(q, loc))):
        l2 = dict(loc)
        l2[var] = el
        if all(q.helper.truthy(f(q, l2)) for f in filters):
            _run_comp(q, ccs, i + 1, l2, head, out)


def _run_mapcomp(
    q: QEval, ccs: list[Any], i: int, loc: Locals, key: Op, val: Op, out: dict[str, Any]
) -> None:
    if i == len(ccs):
        k = key(q, loc)
        if not is_str(k):
            raise EvalErr("map key must be string")
        if k in out:
            raise EvalErr(f"duplicate key {k}", "E5004")
        out[k] = val(q, loc)
        return
    var, iter_op, filters = ccs[i]
    for el in q.helper.iterate(q.helper.mat_val(iter_op(q, loc))):
        l2 = dict(loc)
        l2[var] = el
        if all(q.helper.truthy(f(q, l2)) for f in filters):
            _run_mapcomp(q, ccs, i + 1, l2, key, val, out)


def _compile_ctx(nm: str, c: CCtx) -> Op:
    """a context variable (§7.3): $this/$parent/$root are references to the
    containing instances; $key/$path are plain values"""
    self_inst = c.self_inst
    root_name = c.root_name

    if nm == "$this":

        def this_op(q: QEval, _l: Locals) -> Any:
            if self_inst is None:
                raise EvalErr("$this outside a record instance", "E4090")
            return Ref(self_inst.path)

        return this_op
    if nm == "$parent":

        def parent_op(q: QEval, _l: Locals) -> Any:
            if self_inst is None or self_inst.parent is None:
                raise EvalErr("$parent: the evaluation root has no owner", "E4090")
            return Ref(self_inst.parent.path)

        return parent_op
    if nm == "$root":

        def root_op(q: QEval, _l: Locals) -> Any:
            if not root_name or root_name not in q.helper.roots_map:
                raise EvalErr("$root outside an evaluation root", "E4090")
            return Ref([root_name])

        return root_op
    if nm == "$key":

        def key_op(q: QEval, _l: Locals) -> Any:
            if (
                self_inst is None
                or self_inst.parent is None
                or len(self_inst.path) < len(self_inst.parent.path) + 2
            ):
                raise EvalErr("$key: the instance is not a collection element", "E4090")
            return seg_text(self_inst.path[-1])

        return key_op
    if nm == "$path":

        def path_op(q: QEval, _l: Locals) -> Any:
            if self_inst is None:
                raise EvalErr("$path outside a record instance", "E4090")
            return path_str(self_inst.path)

        return path_op
    raise Unsupported(f"ctx {nm}")


# ---- place navigation for ref<T> members (§7.4) -----------------------------


def _compile_nav(e: dict[str, Any], c: CCtx) -> Op:
    """navigate an expression as a place: a step past a missing member/key/index
    still names the location (a Segs value); a present step yields the value
    there. Mirrors the value layer's ev_nav."""
    k = e["e"]
    if k == "if":
        cop = compile(e["c"], c)
        top = _compile_nav(e["t"], c)
        fop = _compile_nav(e["f"], c)
        return lambda q, l: top(q, l) if q.helper.truthy(cop(q, l)) else fop(q, l)
    if k == "paren":
        return _compile_nav(e["x"], c)
    if k == "member":
        nm = e["name"]
        xop = _compile_nav(e["x"], c)

        def member_nav(q: QEval, l: Locals) -> Any:
            x0 = xop(q, l)
            if isinstance(x0, Segs):
                return Segs([*x0.segs, nm])
            x = q.helper.deref(x0)
            v = q.helper.access(x, nm)
            if v is ABSENT and isinstance(x, RecInst):
                return Segs([*x.path, nm])
            return v

        return member_nav
    if k == "index":
        normal = compile(e, c)  # a non-collection base falls to normal eval
        xop = _compile_nav(e["x"], c)
        iop = compile(e["i"], c)

        def index_nav(q: QEval, l: Locals) -> Any:
            x0 = xop(q, l)
            iv = iop(q, l)
            if isinstance(x0, Segs):
                seg = KeySeg(iv) if is_str(iv) else max(int(iv), 0)
                return Segs([*x0.segs, seg])
            x = q.helper.deref(x0)
            if isinstance(x, ArrV):
                n = int(iv)
                if 0 <= n < len(x.items):
                    return x.items[n]
                return Segs([*x.path, max(n, 0)])
            if isinstance(x, MapV):
                if not is_str(iv):
                    raise EvalErr("map index needs a string")
                if iv in x.entries:
                    return x.entries[iv]
                return Segs([*x.path, KeySeg(iv)])
            if isinstance(x, RecInst):
                v = q.helper.access(x, iv)
                return Segs([*x.path, iv]) if v is ABSENT else v
            return normal(q, l)

        return index_nav
    return compile(e, c)


def _compile_place(e: dict[str, Any], c: CCtx) -> Callable[[QEval, Locals], list[Any] | None]:
    """the path segments a ref-position expression denotes, or None if not a place"""
    nav = _compile_nav(e, c)

    def place(q: QEval, l: Locals) -> Any:
        v = nav(q, l)
        if isinstance(v, Segs):
            return list(v.segs)
        if isinstance(v, Ref):
            return list(v.segs)
        if isinstance(v, (RecInst, ArrV, MapV)):
            return list(v.path)
        return None

    return place


def _compile_callee(e: dict[str, Any], c: CCtx) -> Op:
    """a call's callee (§4.9): a member chain over `std` builds a std path
    (std.math.min) rather than a data access; a namespace export resolves
    through the value layer; anything else compiles normally"""
    if e["e"] == "member":
        nm = e["name"]
        xop = _compile_callee(e["x"], c)

        def callee(q: QEval, l: Locals) -> Any:
            x = xop(q, l)
            if isinstance(x, StdRef):
                return StdRef([*x.path, nm])
            if isinstance(x, NsRef):
                sc = Scope(None, {}, "", q.env)
                return q.helper.mat_val(q.helper.ns_value(x, nm, sc))
            d = q.helper.deref(x) if isinstance(x, Ref) else x
            return q.helper.access(d, nm)

        return callee
    return compile(e, c)


# ---- compile an expression to a query compute -------------------------------


def compile(e: dict[str, Any], c: CCtx) -> Op:
    k = e["e"]
    if k == "lit":
        v = e["v"]
        return lambda q, l: v
    if k == "pattern":
        re = e["re"]
        return lambda q, l: Pattern(re)
    if k == "unitlit":
        num = e["num"]
        unit = e["unit"]

        def unit_op(q: QEval, _l: Locals) -> Any:
            try:
                u = q.env.unit_info(unit)
            except RuntimeError as err:
                raise EvalErr(str(err)) from err
            return Quantity(u["key"], num * u["to_base"])

        return unit_op
    if k == "paren":
        return compile(e["x"], c)
    if k == "un":
        op = e["op"]
        xc = compile(e["x"], c)
        return lambda q, l: _apply_un(q, op, xc(q, l))
    if k == "if":
        cc = compile(e["c"], c)
        tc = compile(e["t"], c)
        fc = compile(e["f"], c)
        return lambda q, l: tc(q, l) if q.helper.truthy(cc(q, l)) else fc(q, l)
    if k == "bin":
        return _compile_bin(e, c)
    if k == "name":
        return _compile_name(e["name"], c)
    if k == "member":
        nm = e["name"]
        safe = e.get("safe")
        xc = compile(e["x"], c)

        def member_op(q: QEval, l: Locals) -> Any:
            x0 = xc(q, l)
            if isinstance(x0, NsRef):
                sc = Scope(None, {}, "", q.env)
                return q.helper.mat_val(q.helper.ns_value(x0, nm, sc))
            if safe and (x0 is None or x0 is ABSENT):
                return ABSENT
            return q.helper.access(q.helper.deref(x0), nm)

        return member_op
    if k == "index":
        xc = compile(e["x"], c)
        ic = compile(e["i"], c)

        def index_op(q: QEval, l: Locals) -> Any:
            x = q.helper.mat_val(xc(q, l))  # deref + flatten a lazy prevalue
            i = ic(q, l)
            if isinstance(x, ArrV):
                n = int(i)
                if n < 0 or n >= len(x.items):
                    raise EvalErr(f"index {n} out of bounds", "E5005")
                return x.items[n]
            if isinstance(x, MapV):
                return x.entries.get(i, ABSENT)
            if isinstance(x, RecInst):
                return q.helper.access(x, i)
            raise EvalErr("index on non-collection")

        return index_op
    if k == "arr":
        parts = [(it["spread"], compile(it["expr"], c)) for it in e["items"]]

        def arr_op(q: QEval, l: Locals) -> Any:
            items: list[Any] = []
            for spread, op in parts:
                v = op(q, l)
                if spread:
                    items.extend(q.helper.iterate(q.helper.mat_val(v)))
                else:
                    items.append(v)
            return ArrV(items, [])

        return arr_op
    if k == "comp":
        all_vars = [cl["v"] for cl in e["clauses"]]
        head_op = compile(e["head"], _with_locals(c, set(c.locals) | set(all_vars)))
        ccs = _compile_clauses(e["clauses"], all_vars, c)

        def comp_op(q: QEval, l: Locals) -> Any:
            out: list[Any] = []
            _run_comp(q, ccs, 0, l, head_op, out)
            return ArrV(out, [])

        return comp_op
    if k == "obj":
        return _compile_obj(e, c)
    if k == "mapcomp":
        all_vars = [cl["v"] for cl in e["clauses"]]
        scope = set(c.locals) | set(all_vars)
        key_op = compile(e["key"], _with_locals(c, scope))
        val_op = compile(e["val"], _with_locals(c, scope))
        ccs = _compile_clauses(e["clauses"], all_vars, c)

        def mapcomp_op(q: QEval, l: Locals) -> Any:
            out: dict[str, Any] = {}
            _run_mapcomp(q, ccs, 0, l, key_op, val_op, out)
            return MapV(out, [])

        return mapcomp_op
    if k == "template":
        tparts: list[Any] = [p if is_str(p) else compile(p, c) for p in e["parts"]]

        def template_op(q: QEval, l: Locals) -> Any:
            return "".join(p if is_str(p) else q.helper.to_str(p(q, l)) for p in tparts)

        return template_op
    if k == "match":
        return _compile_match(e, c)
    if k == "ctx":
        return _compile_ctx(e["name"], c)
    if k == "referrers":
        ty = e["type"]
        member = e["member"]
        self_inst = c.self_inst

        def referrers_op(q: QEval, _l: Locals) -> Any:
            sc = Scope(self_inst, {}, "", q.env)
            return q.helper.referrers(ty, member, sc)

        return referrers_op
    if k == "call":
        fn_op = _compile_callee(e["fn"], c)
        arg_ops = [compile(a, c) for a in e["args"]]

        def call_op(q: QEval, l: Locals) -> Any:
            fn = fn_op(q, l)
            args = [op(q, l) for op in arg_ops]
            sc = Scope(None, {}, "", q.env)
            return q.helper.call(fn, args, sc)

        return call_op
    if k == "lambda":
        params = e["params"]
        body = e["body"]
        menv = c.menv
        root_name = c.root_name
        self_inst = c.self_inst

        def lambda_op(q: QEval, l: Locals) -> Any:
            # a closure over the current locals; its body runs through the value
            # layer, which is pure over the closure's parameters and captures
            return Closure(params, body, Scope(self_inst, dict(l), root_name, menv))

        return lambda_op
    if k == "with":
        menv = c.menv
        root_name = c.root_name
        self_inst = c.self_inst

        def with_op(q: QEval, l: Locals) -> Any:
            sc = Scope(self_inst, dict(l), root_name, menv)
            return q.helper.mat_val(q.helper.ev(e, sc))

        return with_op
    raise Unsupported(k)


def _compile_bin(e: dict[str, Any], c: CCtx) -> Op:
    op = e["op"]
    if op == "|>":
        r = e["r"]
        call = (
            {"e": "call", "fn": r["fn"], "args": [e["l"], *r["args"]]}
            if r["e"] == "call"
            else {"e": "call", "fn": r, "args": [e["l"]]}
        )
        return compile(call, c)
    lc = compile(e["l"], c)
    rc = compile(e["r"], c)
    if op == "&&":
        return lambda q, l: q.helper.truthy(rc(q, l)) if q.helper.truthy(lc(q, l)) else False
    if op == "||":
        return lambda q, l: True if q.helper.truthy(lc(q, l)) else q.helper.truthy(rc(q, l))
    if op == "??":

        def coalesce(q: QEval, l: Locals) -> Any:
            v = lc(q, l)
            return rc(q, l) if (v is ABSENT or v is None) else v

        return coalesce
    return lambda q, l: _binop_val(q, op, lc(q, l), rc(q, l))


def _binop_val(q: QEval, op: str, l: Any, r: Any) -> Any:
    """value-level binary operators (§4.4—4.6), mirroring the value layer's
    binop over already-evaluated operands"""
    import math

    if op in ("..", "..<"):
        return RangeV(l, r, op == "..<")
    if op == "matches":
        if not is_str(l) or not isinstance(r, Pattern):
            raise EvalErr("matches needs a string and a pattern")
        bad = pattern_error(r.re)
        if bad:
            raise EvalErr(f"malformed pattern /{r.re}/: {bad}", "E4119")
        return compile_pattern(r.re).fullmatch(l) is not None
    if op == "==":
        return value_eq(q.helper.mat_val(l), q.helper.mat_val(r))
    if op == "!=":
        return not value_eq(q.helper.mat_val(l), q.helper.mat_val(r))
    if op == "in":
        if isinstance(r, Ref):
            r = q.helper.deref(r)
        if isinstance(r, RangeV):
            return l >= r.lo and (l < r.hi if r.excl else l <= r.hi)
        if isinstance(r, ArrV):
            return any(value_eq(l, x) for x in r.items)
        if isinstance(r, MapV):
            return l in r.entries
        if isinstance(r, RecInst):
            return l in r.slots and q.helper.force_state(r, l) != "absent"
        raise EvalErr("in: bad container")
    if l is ABSENT or r is ABSENT:
        raise EvalErr("absent consumed")
    if (isinstance(l, Quantity) or isinstance(r, Quantity)) and op in (
        "+",
        "-",
        "*",
        "/",
        "<",
        "<=",
        ">",
        ">=",
    ):
        return q.helper.q_arith(op, l, r)
    both_i = is_int(l) and is_int(r)
    both_f = isinstance(l, float) and isinstance(r, float)
    both_s = is_str(l) and is_str(r)
    if op == "+":
        if both_s or both_i or both_f:
            return l + r
    elif op == "-":
        if both_i or both_f:
            return l - r
    elif op == "*":
        if both_i or both_f:
            return l * r
    elif op == "/":
        if both_i:
            if r == 0:
                raise EvalErr("division by zero", "E5001")
            qv = abs(l) // abs(r)
            return qv if (l < 0) == (r < 0) else -qv
        if both_f:
            if r == 0:
                raise EvalErr("division by zero", "E5001")
            qv = l / r
            if not math.isfinite(qv):
                raise EvalErr("non-finite", "E5002")
            return qv
    elif op == "%":
        if both_i:
            if r == 0:
                raise EvalErr("mod zero", "E5001")
            m = abs(l) % abs(r)
            return -m if l < 0 else m
    elif op in ("<", "<=", ">", ">="):
        if both_i or both_f or both_s:
            return {"<": l < r, "<=": l <= r, ">": l > r}.get(op, l >= r)
    elif op == "&":
        if both_i:
            return l & r
    elif op == "|":
        if both_i:
            return l | r
    elif op == "^":
        if both_i:
            return l ^ r
    elif op == "<<":
        if both_i:
            if r < 0:
                raise EvalErr("negative shift count", "E5003")
            return l << r
    elif op == ">>" and both_i:
        if r < 0:
            raise EvalErr("negative shift count", "E5003")
        return l >> r
    raise EvalErr(f"bad operands for {op}")


def _compile_name(nm: str, c: CCtx) -> Op:
    if nm in c.locals:
        return lambda q, l: l.get(nm, ABSENT)
    # a member of the enclosing record — or an ancestor, nearest first (§8
    # scoping) — shadows module names; read it through access so an absent
    # optional yields ABSENT
    cur = c.self_inst
    while cur is not None:
        if nm in cur.slots:
            owner = cur
            return lambda q, _l: q.helper.access(owner, nm)
        cur = cur.parent
    # an entry-module const is query-native (memoized here)
    if nm in c.menv.consts and c.menv is c.entry_env:
        key = f"const:{nm}"
        return lambda q, _l: q.query(key)
    # otherwise a module value (a function's closure, an import, a namespace,
    # another module's const), `std`, another evaluation root, or an input —
    # resolved through the value layer, in the tree walker's order
    menv = c.menv
    root_name = c.root_name

    def name_op(q: QEval, _l: Locals) -> Any:
        bound = q.helper.module_value(menv, nm, root_name)
        if bound is not _UNDEF:
            return bound
        if nm == "std":
            return StdRef([])
        if nm in q.helper.roots_map:
            return q.helper.roots_map[nm]
        inp = q.helper.demand_input(menv, nm)
        if inp is not _UNDEF:
            return inp
        raise _unsup(f"name {nm}")

    return name_op


def _compile_obj(e: dict[str, Any], c: CCtx) -> Op:
    """an object literal reaches compile only in a map-typed position (a
    record-typed one is dispatched to bind_record); build a map value, which the
    type binder validates against map<K, V>. A spread entry copies the entries
    of an object-valued expression in place (§4.2)."""
    parts = []
    for en in e["entries"]:
        val = en["val"]
        if val["e"] == "spread":
            parts.append((True, None, compile(val["expr"], c)))
        else:
            parts.append((False, en["key"], compile(val, c)))

    def obj_op(q: QEval, l: Locals) -> Any:
        entries: dict[str, Any] = {}

        def put(k: Any, v: Any) -> None:
            if k in entries:
                raise EvalErr(f"duplicate key {k}", "E5004")
            entries[k] = v

        for spread, key, op in parts:
            if spread:
                s = op(q, l)
                if isinstance(s, Ref):
                    s = q.helper.deref(s)
                for k, v in q.helper.spread_entries(s):
                    put(k, v)
            else:
                put(key, op(q, l))
        return MapV(entries, [])

    return obj_op


def _compile_match(e: dict[str, Any], c: CCtx) -> Op:
    subj = compile(e["subject"], c)
    arms = []
    for a in e["arms"]:
        rt = None if a.get("type") is None else c.menv.resolve(a["type"])
        body = compile(a["body"], _with_locals(c, set(c.locals) | {a["v"]}))
        arms.append((a["v"], rt, body))

    def match_op(q: QEval, l: Locals) -> Any:
        subjv = subj(q, l)
        if isinstance(subjv, Ref):
            subjv = q.helper.deref(subjv)
        sc = Scope(None, {}, "", q.env)
        catch = None
        for var, rt, body in arms:
            if rt is None:
                catch = (var, body)
                continue
            if q.helper.member_of(subjv, rt, sc):
                l2 = dict(l)
                l2[var] = subjv
                return body(q, l2)
        if catch is not None:
            l2 = dict(l)
            l2[catch[0]] = subjv
            return catch[1](q, l2)
        raise EvalErr("match: no arm matched")

    return match_op


NO_LOCALS: Locals = {}


class QReport:
    __slots__ = ("diagnostics", "ok", "outputs")

    def __init__(self, ok: bool, outputs: list[Any], diagnostics: list[Any]) -> None:
        self.ok = ok
        self.outputs = outputs
        self.diagnostics = diagnostics


class BoundSpec:
    """a bound input document: its name, raw value, and declaring module scope"""

    __slots__ = ("menv", "name", "raw")

    def __init__(self, name: str, raw: Any, menv: Any) -> None:
        self.name = name
        self.raw = raw
        self.menv = menv


class QEval:
    """the decl-layer evaluator over one round of a universe. Its records are
    built into the shared env and forced through `helper` — the same value layer
    and rounds machinery the tree walker uses; only slot computes differ (a
    bridge into the query graph)."""

    __slots__ = ("const_ops", "db", "env", "helper", "slot_jobs")

    def __init__(self, env: Any, helper: Engine) -> None:
        self.env = env
        self.helper = helper
        self.const_ops: dict[str, Op] = {}
        self.slot_jobs: dict[str, Op] = {}
        self.db = Db(self._resolve)

    def _resolve(self, key: str) -> Callable[[Db], Any] | None:
        op = self._op_for(key)
        if op is None:
            return None
        return lambda _db: op(self, NO_LOCALS)

    def _bridge(self, key: str) -> Callable[[], Any]:
        """a thunk the value layer runs to force a query-graph slot"""
        return lambda: self.query(key)

    def query(self, key: str) -> Any:
        """read a query's value, translating the core's cycle into an E5007 error"""
        try:
            return self.db.query(key)
        except QCycle as cyc:
            a, b = _member_of_slot_key(cyc.keys[0]), _member_of_slot_key(cyc.keys[-1])
            raise EvalErr(f"dependency cycle: {a} -> {b}", "E5007") from None

    def _op_for(self, key: str) -> Op | None:
        if key.startswith("slot:"):
            return self.slot_jobs.get(key)
        if key.startswith("const:"):
            name = key[len("const:") :]
            if name in self.const_ops:
                return self.const_ops[name]
            con = self.env.consts.get(name)
            if con is None:
                return None
            c = CCtx(None, "", set(), self.env, self.env)
            try:
                op = compile(con["expr"], c)
            except Unsupported:
                return None
            self.const_ops[name] = op
            return op
        return None

    # ---- record binding -----------------------------------------------------

    def _diag(self, message: str, path: list[Any], code: str, root_name: str) -> None:
        self.env.report(
            {
                "severity": "error",
                "message": message,
                "path": path_str(path),
                "code": code,
                "by": f"root:{root_name}",
            }
        )

    def bind_record(
        self, entries: list[Any], rt: dict[str, Any], path: list[Any], parent: Any, menv: Any
    ) -> Any:
        """Bind an object literal to a record type: a RecInst whose members are
        slot queries. A ref<T> member holds a navigation checked for integrity; a
        derived member may restate a supplied value; a record-typed member
        recurses; a scalar or conjunction member type-binds through the value
        layer. Two passes so a member expression can resolve a sibling declared
        later (§4.3)."""
        inst_id = path_str(path)
        root_name = seg_text(path[0])
        supplied = {k: v for k, v in entries}
        inst = RecInst(rt.get("name"), rt, path, parent)
        inst.entry_order = [k for k, _ in entries]
        inst.menv = menv
        inst.eng = self.helper
        self.env.registry.append(inst)
        cctx = CCtx(inst, root_name, set(), menv, self.env)
        members = rt["members"]

        # pass 1: decide each slot's kind and initial state and declare it, so a
        # member expression compiled in pass 2 can resolve its sibling names
        plans: list[Any] = []
        for m in members:
            member_path = [*path, m["name"]]
            key = f"slot:{inst_id}.{m['name']}"
            has = supplied.get(m["name"])
            types = m.get("conj") or ([m["type"]] if m.get("type") is not None else [])

            is_ref = m.get("type") is not None and m["type"]["t"] == "ref" and m.get("conj") is None
            if is_ref:
                if m["kind"] == "der":
                    place = m.get("expr")
                elif has is not None:
                    place = has
                elif m["kind"] == "dflt":
                    place = m.get("dflt")
                else:
                    place = None
                if place is None:
                    if m["kind"] == "opt":
                        inst.slots[m["name"]] = Slot("opt", "absent")
                    else:
                        self._diag(
                            f"required member {m['name']} missing", member_path, "E4002", root_name
                        )
                        inst.slots[m["name"]] = Slot("req", "invalid")
                    continue
                inst.slots[m["name"]] = Slot(m["kind"], "unforced", hidden=bool(m.get("hidden")))
                plans.append(("ref", m["name"], key, place, None, None))
                continue

            if m["kind"] == "der":
                if has is not None and m.get("hidden"):
                    self._diag(
                        f"hidden member {m['name']} supplied", member_path, "E4006", root_name
                    )
                    inst.slots[m["name"]] = Slot("der", "invalid", hidden=True)
                    continue
                member_rt = m.get("type")
                rec_bearing = member_rt is not None and _has_rec(member_rt, set())
                if has is not None and rec_bearing:
                    raise _unsup("restated derived record")
                inst.slots[m["name"]] = Slot("der", "unforced", hidden=bool(m.get("hidden")))
                plans.append(("der", m["name"], key, m["expr"], member_rt, (has, member_path)))
                continue

            if has is not None:
                inst.slots[m["name"]] = Slot(m["kind"], "unforced", hidden=bool(m.get("hidden")))
                plans.append(("supply", m["name"], key, has, types, member_path))
            elif m["kind"] == "dflt":
                inst.slots[m["name"]] = Slot("dflt", "unforced")
                plans.append(("supply", m["name"], key, m["dflt"], types, member_path))
            elif m["kind"] == "opt":
                inst.slots[m["name"]] = Slot("opt", "absent")
            else:
                self._diag(f"required member {m['name']} missing", member_path, "E4002", root_name)
                inst.slots[m["name"]] = Slot("req", "invalid")

        # pass 2: build each live slot's compute op and hand it a bridge
        for kind, name, key, *rest in plans:
            job = self._build_plan(kind, key, rest, inst, cctx, root_name)
            self.slot_jobs[key] = job
            inst.slots[name].compute = self._bridge(key)

        # supplied keys not declared by the type: an error on a closed record
        for k, _ in entries:
            if any(m["name"] == k for m in members):
                continue
            if rt.get("open"):
                raise _unsup("open-record extras")
            nm = f" {rt['name']}" if rt.get("name") else ""
            self._diag(
                f"undeclared member {k} on closed record{nm}", [*path, k], "E4003", root_name
            )
        return inst

    def _build_plan(
        self, kind: str, key: str, rest: list[Any], inst: RecInst, cctx: CCtx, root_name: str
    ) -> Op:
        if kind == "ref":
            place_expr = rest[0]
            try:
                place_op = _compile_place(place_expr, cctx)
            except Unsupported as u:
                raise _unsup(u.what) from None

            def ref_job(q: QEval, l: Locals) -> Any:
                segs = place_op(q, l)
                if segs is None:
                    raise EvalErr("not a place in ref position")
                if q.helper.resolve_segs(segs) is _UNDEF:
                    raise EvalErr(f"dangling reference {path_str(segs)}", "E6002")
                return Ref(segs)

            return ref_job

        if kind == "der":
            expr, member_rt, (restate, member_path) = rest
            try:
                if member_rt is not None:
                    value_op = self._bind_value(expr, member_rt, list(member_path), inst, cctx)
                else:
                    value_op = compile(expr, cctx)
                restate_op = compile(restate, cctx) if restate is not None else None
            except Unsupported as u:
                raise _unsup(u.what) from None
            name = seg_text(member_path[-1])
            mp = list(member_path)

            def der_job(q: QEval, l: Locals) -> Any:
                v = value_op(q, l)
                if restate_op is not None:
                    raw_r = restate_op(q, l)
                    if member_rt is not None:
                        sc = Scope(inst, dict(l), root_name, q.env)
                        restated = q.helper.bind(raw_r, member_rt, mp, inst, sc)
                    else:
                        restated = raw_r
                    if not value_eq(v, restated):
                        raise EvalErr(
                            f"derived member {name} restated with a differing value", "E4005"
                        )
                return v

            return der_job

        # supply
        expr, types, member_path = rest
        try:
            return self._supply_produce(expr, types, list(member_path), inst, cctx)
        except Unsupported as u:
            raise _unsup(u.what) from None

    def _supply_produce(
        self,
        val: dict[str, Any],
        types: list[Any],
        member_path: list[Any],
        inst: RecInst,
        cctx: CCtx,
    ) -> Op:
        """a single type binds through bind_value (records stay in the query
        graph); a conjunction (§3.11) validates the raw value against each
        conjunct through the value layer"""
        if len(types) == 1:
            return self._bind_value(val, types[0], member_path, inst, cctx)
        op = compile(val, cctx)
        root_name = cctx.root_name
        mp = member_path

        def conj_op(q: QEval, l: Locals) -> Any:
            raw = op(q, l)
            v = raw
            sc = Scope(inst, dict(l), root_name, q.env)
            for ty in types:
                v = q.helper.bind(raw, ty, mp, inst, sc)
            return v

        return conj_op

    def _bind_value(
        self, val: dict[str, Any], ty: dict[str, Any], path: list[Any], parent: Any, cctx: CCtx
    ) -> Op:
        """Bind a value expression to a type, keeping records in the query graph.
        A record literal recurses into bind_record; a literal array or map whose
        elements contain records builds each element at its own path; anything
        else compiles and type-binds through the value layer."""
        t = ty["t"]
        if t == "rec" and val["e"] == "obj":
            entries = [(en["key"], en["val"]) for en in val["entries"]]
            menv = cctx.menv
            return lambda q, _l: q.bind_record(entries, ty, list(path), parent, menv)
        if (
            t == "arr"
            and val["e"] == "arr"
            and not any(it["spread"] for it in val["items"])
            and _has_rec(ty["elem"], set())
        ):
            elem = ty["elem"]
            lo, hi = ty.get("lo"), ty.get("hi")
            elem_ops = [
                self._bind_value(it["expr"], elem, [*path, i], parent, cctx)
                for i, it in enumerate(val["items"])
            ]
            apath = list(path)

            def arr_op(q: QEval, l: Locals) -> Any:
                items = [op(q, l) for op in elem_ops]
                if lo is not None:
                    n = len(items)
                    h = hi if hi is not None else float("inf")
                    if n < lo or n > h:
                        raise EvalErr(f"array size {n} outside {lo}..{hi}")
                return ArrV(items, list(apath))

            return arr_op
        if (
            t == "map"
            and val["e"] == "obj"
            and not any(en["val"]["e"] == "spread" for en in val["entries"])
            and _has_rec(ty["val"], set())
        ):
            vty = ty["val"]
            entry_ops = [
                (
                    en["key"],
                    self._bind_value(en["val"], vty, [*path, KeySeg(en["key"])], parent, cctx),
                )
                for en in val["entries"]
            ]
            mpath = list(path)

            def map_op(q: QEval, l: Locals) -> Any:
                entries: dict[str, Any] = {}
                for k, op in entry_ops:
                    entries[k] = op(q, l)
                return MapV(entries, list(mpath))

            return map_op
        # otherwise compile + type-bind through the value layer
        op = compile(val, cctx)
        root_name = seg_text(path[0])

        def bound_op(q: QEval, l: Locals) -> Any:
            raw = op(q, l)
            sc = Scope(parent, dict(l), root_name, q.env)
            return q.helper.bind(raw, ty, path, parent, sc)

        return bound_op

    # ---- roots --------------------------------------------------------------

    def bind_roots(self, roots: list[Any], unsupported: list[Any]) -> None:
        """Bind every output root for one round into the shared env. A
        record-bearing literal root ({..}/[..]) builds through the query graph;
        any other root binds through the value layer, which handles its own
        deferral across rounds. An unhandled form trips `unsupported`."""
        for name, ty_ast, expr, menv in roots:
            try:
                rt = menv.resolve(ty_ast)
            except Exception:
                continue  # a type that does not resolve: the checker's domain
            literal = expr["e"] in ("obj", "arr")
            if _has_rec(rt, set()) and literal:
                c = CCtx(None, name, set(), menv, self.env)
                path = [name]
                try:
                    op = self._bind_value(expr, rt, path, None, c)
                except Unsupported:
                    unsupported.append(True)
                    continue
                try:
                    v = op(self, NO_LOCALS)
                    self.env.roots[name] = v
                except EvalErr as ex:
                    if ex.code == QUNSUP:
                        unsupported.append(True)
                    else:
                        self.env.report(
                            {
                                "severity": "error",
                                "message": ex.msg,
                                "path": name,
                                "code": ex.code,
                            }
                        )
                except (Taint, DeferSig):
                    pass  # reported / handled by the round
            else:
                sc = Scope(None, {}, name, menv)
                self.helper.bind_root(name, expr, rt, sc, True)


def _member_of_slot_key(key: str) -> str:
    """the trailing member name of a slot key (slot:hub.ports["a"].sel$ -> sel$)"""
    i = key.rfind(".")
    return key[i + 1 :] if i >= 0 else key


# ---- entry points -----------------------------------------------------------


def _eval_rounds(
    entry: Any,
    roots: list[Any],
    binds: list[Any],
    hook_mods: list[Any],
    serialize_outputs: bool = True,
) -> tuple[QReport, Engine]:
    """The rounds driver shared by the single-module and universe entries: bind
    the bound inputs and roots through a fresh QEval each round, forcing through
    the value layer's rounds machinery (Engine.evaluate) until $referrers
    settles."""
    unsupported: list[Any] = []

    def bind(eng: Engine) -> None:
        ev = QEval(entry, eng)
        # §4.13 elaboration hooks (a const in a type position, a unit factor) for
        # every module point at this round's value layer, as run_universe does;
        # the entry's are already set by Engine.__init__ (as run_pipeline relies on)
        for m in hook_mods:
            m.const_eval = (lambda e_: lambda n: eng.force_const_in(e_, n, ""))(m)
            m.expr_eval = (lambda e_: lambda x: eng.ev(x, Scope(None, {}, "", e_)))(m)
        # a bound input document is a root of the universe, available before the
        # outputs that read it (§9.2); it binds through the value layer
        for b in binds:
            decl = b.menv.inputs.get(b.name)
            if decl is None:
                continue
            sc = Scope(None, {}, b.name, b.menv)
            try:
                rt = b.menv.resolve(decl["type"])
            except Exception as ex:
                entry.report(
                    {"severity": "error", "message": str(ex), "path": b.name, "code": None}
                )
                continue
            eng.bind_root(b.name, b.raw, rt, sc, False)
        ev.bind_roots(roots, unsupported)

    eng = Engine.evaluate(entry, bind)
    eng.validate_all("")
    diags = sort_diags(list(entry.diagnostics))  # §6.7
    entry.diagnostics[:] = diags
    if unsupported or any(d.get("code") == QUNSUP for d in diags):
        raise Unsupported("form")
    ok = not any(d["severity"] == "error" for d in diags)
    outputs = []
    if ok and serialize_outputs:
        for name, _, _, _ in roots:
            if name in entry.roots:
                outputs.append({"name": name, "json": eng.serialize(entry.roots[name], name)})
    return QReport(ok, outputs, diags), eng


def qevaluate(env: Any) -> QReport:
    """Evaluate a single module's outputs (its own env is the whole universe)."""
    roots = [(o["name"], o["type"], o["expr"], env) for o in env.outputs]
    report, _eng = _eval_rounds(env, roots, [], [])
    return report


def qevaluate_universe(
    mods: list[Any], entry: Any, binds: list[Any], serialize_outputs: bool = True
) -> tuple[QReport, Engine]:
    """Evaluate a whole multi-module universe (§8.8): every module's outputs are
    roots, each bound in its own module scope into the entry universe; bound
    input documents are roots too. Returns the report and the settled round's
    value layer so the caller can serialize through it as run_universe does."""
    roots = []
    for m in mods:
        for o in m.outputs:
            roots.append((o["name"], o["type"], o["expr"], m))
    return _eval_rounds(entry, roots, binds, mods, serialize_outputs)
