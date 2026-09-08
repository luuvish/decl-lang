"""Binding, evaluation, validation, and serialization — a faithful port of
the reference implementation's engine.ts. Semantics are the spec's:
lazy slots with cycle detection, taint / root-cause diagnostics,
$referrers universe ordering, canonical JSON output."""

from __future__ import annotations

import contextlib
import math
from collections.abc import Callable
from typing import Any

from .semantics import (
    ABSENT,
    ArrV,
    Closure,
    DeferSig,
    Env,
    EvalErr,
    JObj,
    Key,
    MapV,
    NatFn,
    NsRef,
    Pattern,
    PreArr,
    PreObj,
    PreVal,
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
    dot_spellable,
    is_bool,
    is_float,
    is_int,
    is_str,
    js_num_str,
    json_str,
    key_of_vec,
    leaf_recs,
    mentions_referrers,
    parse_path,
    path_str,
    pattern_error,
    seg_text,
    value_eq,
    vec_combine,
    vec_of_key,
)


def _member_of_key(key: str) -> str:
    """the trailing member of a slot key (`r.ports["a"].sel$` -> `sel$`)"""
    if key.endswith('"]'):
        i = key.rfind('["')
        if i >= 0:
            return key[i + 2 : -2]
    j = key.rfind(".")
    return key[j + 1 :] if j >= 0 else key


def _is_num(v: Any) -> bool:
    return is_int(v) or is_float(v)


def _is_orderable(x: Any) -> bool:
    """the values `std.array.sort` orders: one primitive kind per call (§13.2)"""
    return is_int(x) or is_float(x) or is_str(x)


def _sort_keyed(keyed: list[tuple[Any, Any]]) -> list[Any]:
    """a stable sort by key: int and float by value, strings by code point (§4.5);
    a mix of kinds is a domain error"""
    if any(is_str(k) for k, _ in keyed) and not all(is_str(k) for k, _ in keyed):
        raise EvalErr("std.array.sort: elements must be of one kind", "E5008")
    return [x for _, x in sorted(keyed, key=lambda e: e[0])]


def _refs_in(v: Any, out: list[Any]) -> None:
    """the references under a value: arrays and maps are traversed, records are not (§7.6)"""
    if isinstance(v, Ref):
        out.append(v.segs)
    elif isinstance(v, ArrV):
        for x in v.items:
            _refs_in(x, out)
    elif isinstance(v, MapV):
        for x in v.entries.values():
            _refs_in(x, out)


def _group_by_type(insts: list[Any]) -> dict[str, list[Any]]:
    out: dict[str, list[Any]] = {}
    for inst in insts:
        if inst.type_name is None:
            continue
        out.setdefault(inst.type_name, []).append(inst)
    return out


def _refs_key(refs: list[Any]) -> str:
    return "\n".join(sorted(path_str(r) for r in refs))


def _edges_key(edges: dict[str, Any]) -> str:
    """a fingerprint of the queried edges, for detecting a round that repeats a
    previous one (a non-settling cycle, §7.6)"""
    parts = []
    for k in sorted(edges):
        e = edges[k]
        inner = ";".join(f"{pk}:{_refs_key(e[pk]['refs'])}" for pk in sorted(e))
        parts.append(f"{k}={inner}")
    return "|".join(parts)


def _edge_eq(a: dict[str, Any], b: dict[str, Any]) -> bool:
    if len(a) != len(b):
        return False
    for k, e in a.items():
        f = b.get(k)
        if f is None or _refs_key(e["refs"]) != _refs_key(f["refs"]):
            return False
    return True


def _segs_key(p: list[Any]) -> list[Any]:
    return [(0, s) if is_int(s) else (1, str(s)) for s in p]


def _edge_diff(a: dict[str, Any], b: dict[str, Any]) -> str:
    """the first referrer (canonical order) whose entry differs between two edges"""
    paths = sorted(({**b, **a}[k]["path"] for k in set(a) | set(b)), key=_segs_key)
    for p in paths:
        k = path_str(p)
        e, f = a.get(k), b.get(k)
        if e is None or f is None or _refs_key(e["refs"]) != _refs_key(f["refs"]):
            return k
    return ""


class Engine:
    ROUNDS = 8

    def __init__(self, env: Env) -> None:
        self.env = env
        self.deferred_slots: list[Any] = []
        self.no_reg = 0
        self.phase = 1
        self.failed_inputs: set[Any] = set()
        # (name, raw, rt, sc, via_expr): roots that deferred through $referrers in phase 1
        self.deferred_roots: list[Any] = []
        # ---- dependency tracking (Phase 6 foundations; docs/tooling/02_repl.md §6) ----
        # every evaluation step — a slot being computed (`path.member`), a root
        # being bound (`root:name`), an instance's asserts (`assert:path`) —
        # records what it read: slots, roots, and `$referrers` queries by type
        # (`referrers:Type`); diagnostics carry the step that produced them
        self.reads: dict[str, Any] = {}
        # dependency tracking is recorded only for a session; a one-shot
        # evaluate/validate never reads it, so it is off by default — avoiding a
        # per-slot set allocation that dominates at scale (F21)
        self.track = False
        self.computing: list[Any] = []
        self.slots_by_key: dict[str, Any] = {}
        # ---- `$referrers` is answered in rounds (§7.6) ----
        # a round answers from a fixed snapshot — round 1 from what phase 1
        # materialized, every later round from the previous round's whole
        # universe, kept frozen in `prev` — never from what it creates itself;
        # rounds repeat until the queried edges stop changing (`Engine.evaluate`)
        self.prev: Engine | None = None
        self.frozen_roots: dict[str, Any] | None = None
        self.frozen_registry: list[Any] | None = None
        self.settled = False  # the last round: a reference into the snapshot resolves live
        self.snap: dict[str, Any] | None = None  # {"insts": by type, "edges": by `T|m`}
        self.queried: set[str] = set()
        # an inverse index of each edge, `T|m` -> (target path -> its referrers'
        # paths): built once so a `$referrers` query is a lookup, not a scan of
        # every candidate's refs (§7.6). Fresh per round with the engine.
        self.ref_index: dict[str, dict[str, list[Any]]] = {}
        # the edges being computed: a query for one of them from inside its own
        # computation is unanswerable in this round (the member is excluded)
        self.computing_edges: set[str] = set()
        # the depth of the forcing stack when each edge computation began: a
        # slot forcing below that depth waits on the answer the edge serves
        self.edge_bases: list[int] = []
        self.const_envs: list[Env] = []
        env.tagger = lambda: self.computing[-1] if self.computing else None
        env.const_eval = lambda name: self.force_const_in(env, name, "")
        env.expr_eval = lambda e: self.ev(e, Scope(None, {}, ""))

    @property
    def roots_map(self) -> dict[str, Any]:
        """the roots this engine's expressions read: its own universe once frozen"""
        return self.frozen_roots if self.frozen_roots is not None else self.env.roots

    @staticmethod
    def slot_key(inst: RecInst, name: str) -> str:
        # inst.path is immutable, so path_str is computed once per instance (F21)
        ps = inst._ps
        if ps is None:
            ps = path_str(inst.path)
            inst._ps = ps
        return f"{ps}.{name}"

    def record(self, read: str) -> None:
        if self.track and self.computing:
            self.reads[self.computing[-1]].add(read)

    def step(self, key: str, f: Any) -> Any:
        self.computing.append(key)
        if self.track:
            self.reads[key] = set()
        try:
            return f()
        finally:
            self.computing.pop()

    # ---------- expression evaluation ----------
    def ev(self, e: dict[str, Any], sc: Scope) -> Any:
        k = e["e"]
        if k == "lit":
            return e["v"]
        if k == "pattern":
            return Pattern(e["re"])
        if k == "unitlit":
            try:
                u = self.env.unit_info(e["unit"])
            except RuntimeError as err:
                raise EvalErr(str(err)) from err
            return Quantity(u["key"], e["num"] * u["to_base"])
        if k == "paren":
            return self.ev(e["x"], sc)
        if k == "mapcomp":
            entries: list[Any] = []

            def rec(ci: int, locals_: dict[str, Any]) -> None:
                if ci == len(e["clauses"]):
                    key = self.ev(e["key"], sc.with_locals(locals_))
                    if not is_str(key):
                        raise EvalErr("map key must be string")
                    if any(kk == key for kk, _ in entries):
                        raise EvalErr(f"duplicate key {key}", "E5004")
                    entries.append((key, self.ev(e["val"], sc.with_locals(locals_))))
                    return
                cl = e["clauses"][ci]
                for el in self.iterate(self.ev(cl["iter"], sc.with_locals(locals_))):
                    l2 = dict(locals_)
                    l2[cl["v"]] = el
                    if all(self.truthy(self.ev(f, sc.with_locals(l2))) for f in cl["filters"]):
                        rec(ci + 1, l2)

            rec(0, sc.locals)
            return PreObj(entries)
        if k == "template":
            return "".join(p if is_str(p) else self.to_str(self.ev(p, sc)) for p in e["parts"])
        if k == "name":
            name = e["name"]
            if name in sc.locals:
                return sc.locals[name]
            if sc.inst is not None:
                v = self.slot_lookup(sc.inst, name)
                if v is not _UNDEF:
                    return v
            menv = sc.menv or self.env
            bound = self.module_value(menv, name, sc.root_name)
            if bound is not _UNDEF:
                return bound
            if name == "std":
                return StdRef([])
            if name in self.roots_map:
                self.record(f"root:{name}")
                return self.roots_map[name]
            inp = self.demand_input(menv, name)
            if inp is not _UNDEF:
                return inp
            raise EvalErr(f"unknown name {name}")
        if k == "ctx":
            # $this / $parent / $root are references (§7.3): each denotes an
            # instance that contains the current one, so a value reading would
            # be a self-containing value; $key and $path are plain values
            n = e["name"]
            inst = sc.inst
            if n == "$this":
                if inst is None:
                    raise EvalErr("$this outside a record instance", "E4090")
                return Ref(inst.path)
            if n == "$parent":
                if inst is None or inst.parent is None:
                    raise EvalErr("$parent: the evaluation root has no owner", "E4090")
                return Ref(inst.parent.path)
            if n == "$root":
                if not sc.root_name or sc.root_name not in self.roots_map:
                    raise EvalErr("$root outside an evaluation root", "E4090")
                return Ref([sc.root_name])
            if n == "$key":
                # the key or index under which $this sits in its parent's
                # collection: the last path segment, present only when the
                # instance is a collection element (not a direct member)
                if (
                    inst is None
                    or inst.parent is None
                    or len(inst.path) < len(inst.parent.path) + 2
                ):
                    raise EvalErr("$key: the instance is not a collection element", "E4090")
                return seg_text(inst.path[-1])
            if n == "$path":
                if inst is None:
                    raise EvalErr("$path outside a record instance", "E4090")
                return path_str(inst.path)
            raise EvalErr(f"unsupported context var {n}")
        if k == "referrers":
            return self.referrers(e["type"], e["member"], sc)
        if k == "obj":
            return PreObj([(en["key"], PreVal(en["val"], sc)) for en in e["entries"]])
        if k == "spread":
            raise EvalErr("spread outside an object literal")
        if k == "arr":
            return PreArr([(it["spread"], PreVal(it["expr"], sc)) for it in e["items"]])
        if k == "comp":
            items: list[Any] = []

            def rec2(ci: int, locals_: dict[str, Any]) -> None:
                if ci == len(e["clauses"]):
                    items.append((False, PreVal(e["head"], sc.with_locals(locals_))))
                    return
                cl = e["clauses"][ci]
                for el in self.iterate(self.ev(cl["iter"], sc.with_locals(locals_))):
                    l2 = dict(locals_)
                    l2[cl["v"]] = el
                    if all(self.truthy(self.ev(f, sc.with_locals(l2))) for f in cl["filters"]):
                        rec2(ci + 1, l2)

            rec2(0, sc.locals)
            return PreArr(items)
        if k == "if":
            return self.ev(e["t"], sc) if self.truthy(self.ev(e["c"], sc)) else self.ev(e["f"], sc)
        if k == "match":
            subj = self.deref(self.ev(e["subject"], sc))

            def run(arm: dict[str, Any]) -> Any:
                l2 = dict(sc.locals)
                l2[arm["v"]] = subj
                return self.ev(arm["body"], sc.with_locals(l2))

            catch_all = None
            for arm in e["arms"]:
                if arm.get("type") is None:
                    catch_all = arm
                    continue
                if self.member_of(subj, (sc.menv or self.env).resolve(arm["type"]), sc):
                    return run(arm)
            if catch_all is not None:
                return run(catch_all)
            raise EvalErr("match: no arm matched")
        if k == "lambda":
            return Closure(e["params"], e["body"], sc)
        if k == "un":
            x = self.ev(e["x"], sc)
            op = e["op"]
            if op == "!":
                return not self.truthy(x)
            if op == "-":
                if x is ABSENT:
                    raise EvalErr("absent consumed")
                if isinstance(x, Quantity):
                    return Quantity(x.dim, -x.value)
                return -x
            if op == "~":
                return ~x
            raise EvalErr("un")
        if k == "bin":
            if e["op"] == "|>":
                r = e["r"]
                call = (
                    {"e": "call", "fn": r["fn"], "args": [e["l"]] + r["args"]}
                    if r["e"] == "call"
                    else {"e": "call", "fn": r, "args": [e["l"]]}
                )
                return self.ev(call, sc)
            return self.binop(e["op"], e["l"], e["r"], sc)
        if k == "member":
            x0 = self.ev(e["x"], sc)
            if isinstance(x0, NsRef):
                return self.ns_value(x0, e["name"], sc)
            if e.get("safe") and (x0 is None or x0 is ABSENT):
                return ABSENT
            return self.access(self.deref(x0), e["name"])
        if k == "index":
            # a literal (or document) operand is materialized before it is indexed
            x = self.mat_val(self.ev(e["x"], sc))
            i = self.ev(e["i"], sc)
            if isinstance(x, ArrV):
                n = int(i)
                if n < 0 or n >= len(x.items):
                    raise EvalErr(f"index {n} out of bounds", "E5005")
                return x.items[n]
            if isinstance(x, MapV):
                return x.entries.get(i, ABSENT)
            if isinstance(x, RecInst):
                return self.access(x, i)
            raise EvalErr("index on non-collection")
        if k == "call":
            args = [self.ev(a, sc) for a in e["args"]]
            fn = self.ev_callee(e["fn"], sc)
            return self.call(fn, args, sc)
        if k == "with":
            base = self.deref(self.ev(e["base"], sc))
            if isinstance(base, (PreObj, MapV)):
                # an unbound literal (e.g. a constructor func's result), or a literal
                # no record type claimed: its entries, updated, still unbound
                patch = self.ev(e["patch"], sc)
                entries = (
                    self.entries_of(base)
                    if isinstance(base, PreObj)
                    else list(base.entries.items())
                )
                for pk, pv in self.entries_of(patch):
                    idx = next((j for j, (n, _) in enumerate(entries) if n == pk), -1)
                    if idx >= 0:
                        entries[idx] = (pk, pv)
                    else:
                        entries.append((pk, pv))
                return PreObj(entries)
            if not isinstance(base, RecInst):
                raise EvalErr("with on non-record")
            patch = self.ev(e["patch"], sc)
            entries = []
            for n in base.entry_order:
                if n in base.extras:
                    entries.append((n, base.extras[n]))
                    continue
                s = base.slots[n]
                if s.kind == "der" or s.state == "absent":
                    continue
                entries.append((n, self.force_slot(base, n)))
            for m in base.rt["members"]:
                if (
                    m["kind"] == "dflt"
                    and not any(k_ == m["name"] for k_, _ in entries)
                    and base.slots.get(m["name"]) is not None
                    and base.slots[m["name"]].state != "absent"
                ):
                    entries.append((m["name"], self.force_slot(base, m["name"])))
            for pk, pv in self.entries_of(patch):
                idx = next((j for j, (n, _) in enumerate(entries) if n == pk), -1)
                if idx >= 0:
                    entries[idx] = (pk, pv)
                else:
                    entries.append((pk, pv))
            return PreObj(entries)
        raise EvalErr(f"ev: unhandled {k}")

    def entries_of(self, pre: PreObj) -> list[Any]:
        """the entries of an unbound object literal with its spreads (§4.2)
        expanded in place: a spread copies the entries of an object-valued
        operand — a record's value members (hidden ones are not entries), a
        map's entries, or another literal's — and a key produced twice is
        E5004. Spread operands evaluate in the literal's own scope."""

        def is_spread(v: Any) -> bool:
            return isinstance(v, PreVal) and v.expr.get("e") == "spread"

        if not any(is_spread(v) for _, v in pre.entries):
            return list(pre.entries)
        out: list[Any] = []

        def put(k_: str, v: Any) -> None:
            if any(n == k_ for n, _ in out):
                raise EvalErr(f"duplicate key {k_}", "E5004")
            out.append((k_, v))

        for k_, v in pre.entries:
            if not is_spread(v):
                put(k_, v)
                continue
            s = self.deref(self.ev(v.expr["expr"], v.scope))
            for k2, v2 in self.spread_entries(s):
                put(k2, v2)
        return out

    def spread_entries(self, s: Any) -> list[Any]:
        if isinstance(s, PreObj):
            return self.entries_of(s)
        if isinstance(s, JObj):
            return list(s.entries)
        if isinstance(s, MapV):
            return list(s.entries.items())
        if isinstance(s, RecInst):
            # the value's entries in serialization order (§10.3): supplied members
            # in document order, then the members evaluation materialized, in
            # declaration order — every value member, hidden ones never
            entries: list[Any] = []
            seen: set[str] = set()
            for n in [*s.entry_order, *(m["name"] for m in s.rt["members"])]:
                if n in seen:
                    continue
                seen.add(n)
                if n in s.extras:
                    entries.append((n, s.extras[n]))
                    continue
                slot = s.slots.get(n)
                if slot is None or slot.hidden:
                    continue
                v = self.force_slot(s, n)
                if slot.state != "absent":
                    entries.append((n, v))
            return entries
        raise EvalErr("spread of a non-object value")

    def mat_val(self, v: Any) -> Any:
        d = self.deref(v)
        if isinstance(d, (PreObj, PreArr)):
            return self.mat_pre(d)
        if isinstance(d, JObj):
            return self.materialize(d, [], None, None)
        return d

    def mat_pre(self, d: Any) -> Any:
        """an unmaterialized literal read as a value is materialized once: its
        elements are pure, and a chain of spreads (a fold accumulating an array)
        would otherwise re-evaluate every level at every read"""
        if d.flat_value is None:
            d.flat_value = self.materialize(d, [], None, None)
        return d.flat_value

    def member_of(self, v: Any, rt: dict[str, Any], sc: Scope) -> bool:
        from .subsume import subsumes

        if isinstance(v, RecInst):
            return subsumes(self.env, v.rt, rt)
        mark = len(self.env.diagnostics)
        self.no_reg += 1
        try:
            self.bind(v, rt, ["<match>"], None, sc)
            return True
        except Exception:
            return False
        finally:
            self.no_reg -= 1
            del self.env.diagnostics[mark:]

    def ev_callee(self, e: dict[str, Any], sc: Scope) -> Any:
        if e["e"] == "member":
            x = self.ev_callee(e["x"], sc)
            if isinstance(x, StdRef):
                return StdRef([*x.path, e["name"]])
            if isinstance(x, NsRef):
                return self.ns_value(x, e["name"], sc)
            return self.access(self.deref(x), e["name"])
        return self.ev(e, sc)

    def module_value(self, menv: Env, name: str, root_name: str) -> Any:
        if name in menv.consts:
            return self.force_const_in(menv, name, root_name)
        if name in menv.funcs:
            f = menv.funcs[name]
            return Closure(
                [p["name"] for p in f["params"]], f["body"], Scope(None, {}, root_name, menv)
            )
        im = menv.imports.get(name)
        if im is not None:
            v = self.module_value(im["env"], im["name"], root_name)
            if v is not _UNDEF:
                return v
            if im["name"] in self.roots_map:
                self.record(f"root:{im['name']}")
                return self.roots_map[im["name"]]  # imported output/input root
            return self.demand_input(im["env"], im["name"])
        ns = menv.namespaces.get(name)
        if ns is not None:
            return NsRef(ns["env"], ns["exports"])
        return _UNDEF

    def ns_value(self, ns: NsRef, name: str, sc: Scope) -> Any:
        ex = ns.exports.get(name)
        if ex is None:
            raise EvalErr(f"namespace has no export {name}")
        v = self.module_value(ex["env"], ex["name"], sc.root_name)
        if v is not _UNDEF:
            return v
        if ex["name"] in self.roots_map:
            self.record(f"root:{ex['name']}")
            return self.roots_map[ex["name"]]
        inp = self.demand_input(ex["env"], ex["name"])
        if inp is not _UNDEF:
            return inp
        raise EvalErr(f"{name} is not a value")

    # an input demanded by evaluation (§5.6, §9.4): the bound document if
    # the tool bound one, else its fallback — bound on first demand and
    # memoized as a root; a fallback-less unbound input is E5006 at the
    # demanding path. Returns _UNDEF when `name` is not an input.
    def demand_input(self, menv: Env, name: str) -> Any:
        decl = menv.inputs.get(name)
        if decl is None:
            return _UNDEF
        self.record(f"root:{name}")
        if name in self.roots_map:
            return self.roots_map[name]
        if name in self.failed_inputs:
            raise Taint()
        if decl.get("fallback") is None:
            raise EvalErr(f"input {name} is not bound", "E5006")
        sc = Scope(None, {}, name, menv)
        try:
            v = self.step(
                f"root:{name}",
                lambda: self.bind(
                    self.ev(decl["fallback"], sc), menv.resolve(decl["type"]), [name], None, sc
                ),
            )
            self.env.roots[name] = v
            return v
        except Taint:
            self.failed_inputs.add(name)
            raise

    # bind an evaluation root (an output's expression, or an input's
    # document / fallback): a failing root is reported at its own path and
    # left unset — its demanders are tainted, nothing else is
    def bind_root(self, name: str, raw: Any, rt: dict[str, Any], sc: Scope, via_expr: bool) -> None:
        try:
            v = self.step(
                f"root:{name}",
                lambda: (
                    self.bind(self.ev(raw, sc), rt, [name], None, sc)
                    if via_expr
                    else self.bind(raw, rt, [name], None, sc)
                ),
            )
            self.env.roots[name] = v
        except EvalErr as e:
            self.env.report({"severity": "error", "message": e.msg, "path": name, "code": e.code})
        except DeferSig:
            if self.phase < 2:
                self.deferred_roots.append((name, raw, rt, sc, via_expr))
        except Taint:
            pass

    def bind_deferred_roots(self) -> None:
        """phase 2: the roots that deferred are bound again (after the deferred slots)"""
        pending, self.deferred_roots = self.deferred_roots, []
        for name, raw, rt, sc, via_expr in pending:
            self.bind_root(name, raw, rt, sc, via_expr)

    def q_arith(self, op: str, l: Any, r: Any) -> Any:
        if op in ("+", "-"):
            if not isinstance(l, Quantity) or not isinstance(r, Quantity):
                raise EvalErr(f"`{op}` mixes quantity and plain number")
            if l.dim != r.dim:
                raise EvalErr(f"quantity dimension mismatch: {l.dim or '1'} vs {r.dim or '1'}")
            return Quantity(l.dim, l.value + r.value if op == "+" else l.value - r.value)
        if op in ("<", "<=", ">", ">="):
            if not isinstance(l, Quantity) or not isinstance(r, Quantity) or l.dim != r.dim:
                raise EvalErr("quantity dimension mismatch in comparison")
            return {"<": l.value < r.value, "<=": l.value <= r.value, ">": l.value > r.value}.get(
                op, l.value >= r.value
            )
        lm = l.value if isinstance(l, Quantity) else (float(l) if _is_num(l) else None)
        rm = r.value if isinstance(r, Quantity) else (float(r) if _is_num(r) else None)
        if lm is None or rm is None:
            raise EvalErr(f"bad operands for {op}")
        if op == "/" and rm == 0:
            raise EvalErr("division by zero", "E5001")
        vec = vec_combine(
            vec_of_key(l.dim) if isinstance(l, Quantity) else {},
            vec_of_key(r.dim) if isinstance(r, Quantity) else {},
            1 if op == "*" else -1,
        )
        value = lm * rm if op == "*" else lm / rm
        if not math.isfinite(value):
            raise EvalErr("non-finite", "E5002")
        key = key_of_vec(vec)
        return value if key == "" else Quantity(key, value)

    def iterate(self, v: Any) -> list[Any]:
        if isinstance(v, (PreArr, PreObj)):
            return self.mat_arr(v)
        if isinstance(v, ArrV):
            return v.items
        if isinstance(v, RangeV):
            hi = v.hi if v.excl else v.hi + 1
            return list(range(v.lo, hi))
        raise EvalErr("not iterable")

    def truthy(self, v: Any) -> bool:
        if is_bool(v):
            return v
        raise EvalErr("non-bool condition")

    def binop(self, op: str, le: dict[str, Any], re_: dict[str, Any], sc: Scope) -> Any:
        if op == "&&":
            return self.truthy(self.ev(re_, sc)) if self.truthy(self.ev(le, sc)) else False
        if op == "||":
            return True if self.truthy(self.ev(le, sc)) else self.truthy(self.ev(re_, sc))
        if op == "??":
            l = self.ev(le, sc)
            return self.ev(re_, sc) if (l is ABSENT or l is None) else l
        l, r = self.ev(le, sc), self.ev(re_, sc)
        if op in ("..", "..<"):
            return RangeV(l, r, op == "..<")
        if op == "matches":
            if not is_str(l) or not isinstance(r, Pattern):
                raise EvalErr("matches needs a string and a pattern")
            bad = pattern_error(r.re)
            if bad:
                raise EvalErr(f"malformed pattern /{r.re}/: {bad}", "E4119")
            return compile_pattern(r.re).fullmatch(l) is not None
        # a literal operand is materialized: equality is structural over values (§4.5)
        if op == "==":
            return value_eq(self.mat_val(l), self.mat_val(r))
        if op == "!=":
            return not value_eq(self.mat_val(l), self.mat_val(r))
        if op == "in":
            if isinstance(r, Ref):
                r = self.deref(
                    r
                )  # the container may be reached through a reference ($this, $parent)
            if isinstance(r, RangeV):
                return l >= r.lo and (l < r.hi if r.excl else l <= r.hi)
            if isinstance(r, (PreArr, PreObj)):
                return any(value_eq(l, x) for x in self.mat_arr(r))
            if isinstance(r, ArrV):
                return any(value_eq(l, x) for x in r.items)
            if isinstance(r, MapV):
                return l in r.entries
            if isinstance(r, RecInst):
                return l in r.slots and self.force_state(r, l) != "absent"
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
            return self.q_arith(op, l, r)
        both_i = is_int(l) and is_int(r)
        both_f = is_float(l) and is_float(r)
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
                q = abs(l) // abs(r)
                return q if (l < 0) == (r < 0) else -q
            if both_f:
                if r == 0:
                    raise EvalErr("division by zero", "E5001")
                q = l / r
                if not math.isfinite(q):
                    raise EvalErr("non-finite", "E5002")
                return q
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

    def to_str(self, v: Any) -> str:
        if is_str(v):
            return v
        if is_bool(v):
            return "true" if v else "false"
        if is_int(v):
            return str(v)
        if is_float(v):
            return js_num_str(v)
        raise EvalErr("template: non-convertible")

    def deref(self, v: Any) -> Any:
        if isinstance(v, Ref):
            if v.snap is not None and not self.settled:
                target = v.snap.muted(lambda: v.snap.resolve_segs(v.segs))
            else:
                target = self.resolve_segs(v.segs)
            if target is _UNDEF:
                raise EvalErr(f"dangling reference {path_str(v.segs)}", "E6002")
            return target
        return v

    # the value at a place; _UNDEF when there is none (an absent optional
    # member counts as none — §7.5). Lenient about segment kinds: engine-built
    # paths are canonical by construction
    def resolve_segs(self, segs: list[Any]) -> Any:
        cur = self.roots_map.get(segs[0], _UNDEF)
        for s0 in segs[1:]:
            if cur is _UNDEF:
                break
            s = seg_text(s0)
            if isinstance(cur, RecInst):
                cur = self.force_slot(cur, s) if s in cur.slots else _UNDEF
            elif isinstance(cur, ArrV):
                cur = cur.items[s] if is_int(s) and 0 <= s < len(cur.items) else _UNDEF
            elif isinstance(cur, MapV):
                cur = cur.entries.get(s, _UNDEF)
            else:
                cur = _UNDEF
            if cur is ABSENT:
                cur = _UNDEF
            if isinstance(cur, Ref):
                cur = self.deref(cur)
        return cur

    # a path from a document must be canonical (§7.2, §7.5): a map key
    # bracketed, a record member dotted when the dot can spell it and
    # bracketed otherwise, an array index numeric — any other spelling does
    # not resolve
    def resolve_canonical(self, segs: list[Any]) -> Any:
        cur = self.roots_map.get(segs[0], _UNDEF)
        for s in segs[1:]:
            if cur is _UNDEF:
                break
            if isinstance(cur, RecInst):
                if is_int(s):
                    return _UNDEF
                name = seg_text(s)
                if isinstance(s, Key) == dot_spellable(name):
                    return _UNDEF
                cur = self.force_slot(cur, name) if name in cur.slots else _UNDEF
            elif isinstance(cur, ArrV):
                cur = cur.items[s] if is_int(s) and 0 <= s < len(cur.items) else _UNDEF
            elif isinstance(cur, MapV):
                cur = cur.entries.get(s.k, _UNDEF) if isinstance(s, Key) else _UNDEF
            else:
                cur = _UNDEF
            if cur is ABSENT:
                cur = _UNDEF
            if isinstance(cur, Ref):
                cur = self.deref(cur)
        return cur

    def access(self, x: Any, name: Any) -> Any:
        if isinstance(x, RecInst):
            if name in x.slots:
                return self.force_slot(x, name)  # ABSENT when the member is absent
            if name in x.extras:
                raise EvalErr(f"opaque field {name} accessed")
            # a member the record's type does not declare: the checker admitted the
            # read against a bound that declares it optional, so it is absent (§4.10)
            return ABSENT
        if isinstance(x, PreObj):
            for k_, v in self.entries_of(x):
                if k_ == name:
                    return self.ev(v.expr, v.scope) if isinstance(v, PreVal) else v
            return ABSENT
        if isinstance(x, MapV):
            # a literal that no record type claimed is a map at evaluation time
            # while the checker typed its members by name; the name reads the entry
            return x.entries.get(name, ABSENT)
        if x is None:
            raise EvalErr("member access on null")
        if x is ABSENT:
            return ABSENT
        raise EvalErr(f"member access on non-record ({name})")

    def slot_lookup(self, inst: RecInst, name: str) -> Any:
        cur = inst
        while cur is not None:
            if name in cur.slots:
                return self.force_slot(cur, name)  # ABSENT when the member is absent
            cur = cur.parent
        return _UNDEF

    def call(self, fn: Any, args: list[Any], sc: Scope) -> Any:
        if isinstance(fn, Closure):
            locals_ = dict(fn.scope.locals)
            for p, a in zip(fn.params, args, strict=False):
                locals_[p] = a
            return self.ev(fn.body, fn.scope.with_locals(locals_))
        if isinstance(fn, NatFn):
            return fn.fn(args)
        if isinstance(fn, StdRef):
            return self.std(".".join(fn.path), args, sc)
        raise EvalErr("call of non-function")

    def std(self, name: str, a: list[Any], sc: Scope) -> Any:
        def domain(msg: str) -> None:
            raise EvalErr(f"std.{name}: {msg}", "E5008")

        if name == "array.count":
            return len(self.mat_arr(a[0]))
        if name == "array.all":
            return all(self.truthy(self.call(a[1], [x], sc)) for x in self.mat_arr(a[0]))
        if name == "array.any":
            return any(self.truthy(self.call(a[1], [x], sc)) for x in self.mat_arr(a[0]))
        if name == "array.filter":
            return ArrV(
                [x for x in self.mat_arr(a[0]) if self.truthy(self.call(a[1], [x], sc))], []
            )
        if name == "array.all_distinct":
            items = self.mat_arr(a[0])
            for i in range(len(items)):
                for j in range(i + 1, len(items)):
                    if value_eq(items[i], items[j]):
                        return False
            return True
        if name == "array.sum":
            items = self.mat_arr(a[0])
            if not items:
                return 0
            acc: Any = 0.0 if is_float(items[0]) else 0
            for x in items:
                acc = acc + x
            return acc
        if name == "array.fold":
            acc = a[1]
            for x in self.mat_arr(a[0]):
                acc = self.call(a[2], [acc, x], sc)
            return acc
        if name == "array.sort":
            items = [self.mat_val(x) for x in self.mat_arr(a[0])]
            if any(not _is_orderable(x) for x in items):
                raise EvalErr(f"std.{name}: elements must be int, float, or string", "E5008")
            return ArrV(_sort_keyed([(x, x) for x in items]), [])
        if name == "array.sort_by":
            keyed = []
            for x in self.mat_arr(a[0]):
                k = self.mat_val(self.call(a[1], [x], sc))
                if not _is_orderable(k):
                    raise EvalErr(f"std.{name}: keys must be int, float, or string", "E5008")
                keyed.append((k, x))
            return ArrV(_sort_keyed(keyed), [])
        if name == "array.unique":
            out: list[Any] = []
            for x in self.mat_arr(a[0]):
                if not any(value_eq(y, x) for y in out):
                    out.append(x)
            return ArrV(out, [])
        if name == "array.reverse":
            return ArrV(list(reversed(self.mat_arr(a[0]))), [])
        if name == "map.keys":
            return ArrV(list(self.mat_map(a[0]).entries.keys()), [])
        if name == "map.values":
            return ArrV(list(self.mat_map(a[0]).entries.values()), [])
        if name == "map.entries":
            m = self.mat_map(a[0])
            return ArrV([PreObj([("key", k_), ("value", v)]) for k_, v in m.entries.items()], [])
        if name == "string.length":
            return len(a[0])
        if name == "string.of":
            return self.to_str(a[0])
        if name == "string.join":
            return a[1].join(self.mat_arr(a[0]))
        if name == "string.starts_with":
            return a[0].startswith(a[1])
        if name == "string.ends_with":
            return a[0].endswith(a[1])
        if name == "string.contains":
            return a[1] in a[0]
        if name == "string.split":
            if a[1] == "":
                domain("separator must be non-empty")
            return ArrV(a[0].split(a[1]), [])
        if name == "ref.path":
            if not isinstance(a[0], Ref):
                raise EvalErr("ref.path on non-reference")
            return path_str(a[0].segs)
        if name == "math.abs":
            return abs(a[0])
        if name == "math.min":
            return a[0] if a[0] < a[1] else a[1]
        if name == "math.max":
            return a[0] if a[0] > a[1] else a[1]
        if name == "math.clog2":
            n = a[0]
            if not is_int(n) or n < 1:
                domain(f"n >= 1 required, got {n}")
            k = 0
            while (1 << k) < n:
                k += 1
            return k
        if name == "math.floor":
            return math.floor(a[0])
        if name == "math.ceil":
            return math.ceil(a[0])
        if name == "math.round":
            x = a[0]
            f = math.floor(x)
            frac = x - f
            r = f + 1 if frac > 0.5 else f if frac < 0.5 else (f if f % 2 == 0 else f + 1)
            return int(r)
        if name == "int.of":
            x = a[0]
            if not is_float(x) or not x.is_integer():
                domain(f"no fractional part allowed, got {x}")
            return int(x)
        if name == "int.at_least":
            n = a[0]
            return NatFn(lambda args: args[0] >= n)
        if name == "int.at_most":
            n = a[0]
            return NatFn(lambda args: args[0] <= n)
        if name == "float.of":
            try:
                v = float(a[0])
            except OverflowError:
                v = math.inf
            if not math.isfinite(v):
                domain("magnitude outside binary64 range")
            return v
        if name == "object.merge":
            return self.deep_merge(a[0], a[1])
        raise EvalErr(f"std.{name} does not exist")

    def deep_merge(self, base0: Any, patch0: Any) -> Any:
        base, patch = self.mat_rec(base0), self.mat_rec(patch0)

        def val(r: RecInst, n: str) -> Any:
            if n in r.extras:
                return r.extras[n]
            s = r.slots.get(n)
            if s is None or s.kind == "der":
                return _UNDEF
            v = self.force_slot(r, n)
            return _UNDEF if s.state == "absent" else v

        names: list[Any] = []
        for n in base.entry_order:
            if n not in names:
                names.append(n)
        for m in base.rt["members"]:
            if m["kind"] == "dflt" and m["name"] not in names:
                names.append(m["name"])
        for n in patch.entry_order:
            if n not in names:
                names.append(n)
        for m in patch.rt["members"]:
            if m["kind"] == "dflt" and m["name"] not in names:
                names.append(m["name"])
        entries: list[Any] = []
        for n in names:
            bs, ps = base.slots.get(n), patch.slots.get(n)
            if (bs is not None and bs.kind == "der") or (ps is not None and ps.kind == "der"):
                continue
            bv, pv = val(base, n), val(patch, n)
            if (
                bv is not _UNDEF
                and pv is not _UNDEF
                and isinstance(self.deref(bv), RecInst)
                and isinstance(self.deref(pv), RecInst)
            ):
                entries.append((n, self.deep_merge(bv, pv)))
            elif pv is not _UNDEF:
                entries.append((n, pv))
            elif bv is not _UNDEF:
                entries.append((n, bv))
        return PreObj(entries)

    def mat_rec(self, v: Any) -> RecInst:
        d = self.deref(v)
        if isinstance(d, (PreObj, PreArr)):
            d = self.mat_pre(d)  # materialize once, cached (F21)
        elif isinstance(d, JObj):
            d = self.materialize(d, [], None, None)
        if isinstance(d, RecInst):
            return d
        raise EvalErr("std.object.merge: expected records", "E5008")

    def mat_arr(self, v: Any) -> list[Any]:
        d = self.deref(v)
        if isinstance(d, (PreArr, PreObj)):
            d = self.mat_pre(d)
        if isinstance(d, ArrV):
            return d.items
        raise EvalErr("expected array")

    def mat_map(self, v: Any) -> MapV:
        d = self.deref(v)
        if isinstance(d, (PreArr, PreObj)):
            d = self.mat_pre(d)
        if isinstance(d, MapV):
            return d
        if isinstance(d, RecInst):
            # a record reads as the map of its value entries (§3.17)
            return MapV(dict(self.spread_entries(d)), d.path)
        raise EvalErr("expected map")

    # ---------- referrers ----------
    def referrers(self, type_name: str, member: str, sc: Scope) -> Any:
        if self.phase < 2:
            raise DeferSig()
        self.record(f"referrers:{type_name}")
        self_path = sc.inst.path
        edge = self.snap_edge(type_name, member)
        key = f"{type_name}|{member}"
        inv = self.ref_index.get(key)
        if inv is None:
            # one pass over the edge inverts it: each referrer is filed under
            # every distinct place its member refers to
            inv = {}
            for e in edge.values():
                seen: set[str] = set()
                for r in e["refs"]:
                    tk = path_str(r)
                    if tk in seen:
                        continue
                    seen.add(tk)
                    inv.setdefault(tk, []).append(e["path"])
            self.ref_index[key] = inv
        out = list(inv.get(path_str(self_path), []))
        out.sort(key=_segs_key)
        return ArrV([Ref(p, self.prev) for p in out], [])

    def snap_edge(self, type_name: str, member: str) -> dict[str, Any]:
        """the edge `T|m` of the snapshot: computed from its instances on first demand"""
        if self.snap is None:
            self.take_snapshot({})  # a lazy session: what is materialized
        snap = self.snap
        assert snap is not None
        key = f"{type_name}|{member}"
        self.queried.add(key)
        edge = snap["edges"].get(key)
        if edge is None:
            if key in self.computing_edges:
                raise DeferSig()
            self.computing_edges.add(key)
            self.edge_bases.append(len(self.computing))
            try:
                edge = self.edge_of(snap["insts"].get(type_name, []), member)
            finally:
                self.edge_bases.pop()
                self.computing_edges.discard(key)
            snap["edges"][key] = edge
        return edge

    def take_snapshot(self, edges: dict[str, Any]) -> None:
        """the snapshot's instances: the previous round's universe, else what is materialized now"""
        self.ref_index.clear()
        insts = self.prev.frozen_registry if self.prev is not None else self.env.registry
        self.snap = {"insts": _group_by_type(insts or []), "edges": edges}

    def edge_of(self, insts: list[Any], member: str) -> dict[str, Any]:
        """every candidate's path with the places its member refers to
        (an invalid member: excluded silently, §6.6)"""
        out: dict[str, Any] = {}
        for cand in insts:
            slot = cand.slots.get(member)
            if slot is None or slot.state == "forcing":
                continue  # in computation: needs this very edge
            try:
                v = self.force_slot(cand, member)
            except Exception:
                continue
            refs: list[Any] = []
            _refs_in(v, refs)
            out[path_str(cand.path)] = {"path": cand.path, "refs": refs}
        return out

    def live_edges(self) -> dict[str, Any]:
        """the queried edges over this round's universe, answered from its snapshot"""
        out: dict[str, Any] = {}
        while True:
            todo = [k for k in sorted(self.queried) if k not in out]
            if not todo:
                return out
            insts = _group_by_type(self.env.registry)
            for key in todo:
                t, m = key.split("|")
                out[key] = self.edge_of(insts.get(t, []), m)
            # forcing may have materialized more, and queried more

    def settle(self, edges: dict[str, Any] | None = None) -> dict[str, Any]:
        """phase 2, one round: the deferred slots and roots, then everything;
        stable when the queried edges did not change"""
        self.phase = 2
        self.take_snapshot(edges if edges is not None else {})
        deferred, self.deferred_slots = self.deferred_slots, []
        for inst, name in deferred:
            self.force_slot_safe(inst, name)
        self.bind_deferred_roots()
        self.force_all_roots(True)
        live = self.live_edges()
        snap_edges = self.snap["edges"] if self.snap is not None else {}
        changed = sorted(k for k, e in live.items() if not _edge_eq(e, snap_edges.get(k, {})))
        return {"stable": not changed, "edges": live, "changed": changed}

    def freeze(self) -> None:
        """this round's universe, kept as the next round's snapshot"""
        self.frozen_roots = dict(self.env.roots)
        self.frozen_registry = list(self.env.registry)

    def muted(self, f: Callable[[], Any]) -> Any:
        """run inside a frozen round: what it reports is not this evaluation's"""
        self.env.muted += 1
        try:
            return f()
        finally:
            self.env.muted -= 1

    @staticmethod
    def evaluate(env: Env, bind: Callable[[Engine], None], track: bool = False) -> Engine:
        """Evaluate a universe (§7.6, §9.3): `bind` binds every root on a fresh
        engine; the rounds repeat, each answering `$referrers` from the previous
        round, until the queried edges are stable — the settled engine is the
        result. A universe still changing after `ROUNDS` rounds is E5009."""
        prev: Engine | None = None
        edges: dict[str, Any] = {}
        # the queried edges each round have been seen before (rounds are
        # deterministic, so a repeat is a cycle that never settles — report at
        # once, not after the full round budget; §7.6)
        seen: set[str] = set()
        round_ = 0
        while True:
            round_ += 1
            eng = Engine(env)
            eng.track = track
            eng.prev = prev
            mark = len(env.diagnostics)
            bind(eng)
            eng.force_all_roots(False)
            r = eng.settle(edges)
            ekey = _edges_key(r["edges"])
            cycled = not r["stable"] and ekey in seen
            if r["stable"] or cycled or round_ == Engine.ROUNDS:
                if not r["stable"]:
                    snap_edges = eng.snap["edges"] if eng.snap is not None else {}
                    for key in r["changed"]:
                        t, m = key.split("|")
                        env.report(
                            {
                                "severity": "error",
                                "message": f'$referrers({t}, "{m}") does not stabilize '
                                f"after {Engine.ROUNDS} rounds",
                                "path": _edge_diff(r["edges"][key], snap_edges.get(key, {})),
                                "code": "E5009",
                            }
                        )
                eng.settled = True
                eng.prev = None
                return eng
            # the round is the next one's snapshot; the universe starts over
            eng.freeze()
            env.roots.clear()
            env.registry.clear()
            del env.diagnostics[mark:]
            for e in eng.const_envs:
                for c in e.consts.values():
                    c["state"] = "unforced"
                    c.pop("value", None)
            seen.add(ekey)
            prev = eng
            edges = r["edges"]

    # ---------- binding / checking ----------
    def bind(
        self, raw: Any, rt: dict[str, Any], path: list[Any], parent: RecInst | None, sc: Scope
    ) -> Any:
        if isinstance(raw, PreVal):
            sc2 = raw.scope.with_inst(parent if parent is not None else raw.scope.inst)
            if rt["t"] == "ref":
                place = self.eval_place(raw.expr, sc2)
                if place is None:
                    raise EvalErr("not a place in ref position")
                # reference integrity (§7.5): the place must hold a value
                if self.resolve_segs(place) is _UNDEF:
                    self.env.report(
                        {
                            "severity": "error",
                            "message": f"dangling reference {path_str(place)}",
                            "path": path_str(path),
                            "code": "E6002",
                        }
                    )
                    raise Taint()
                return Ref(place)
            return self.bind(self.ev(raw.expr, sc2), rt, path, parent, sc)

        def fail(msg: str, code: str | None = None) -> None:
            tail = rt.get("tail")
            if tail is not None and tail["t"] == "inline":
                self.env.report(
                    {
                        "severity": "error",
                        "id": rt.get("name"),
                        "message": "".join(p for p in tail["template"] if is_str(p)),
                        "path": path_str(path),
                        "code": "E4001",
                    }
                )
            else:
                self.env.report(
                    {
                        "severity": "error",
                        "message": msg,
                        "path": path_str(path),
                        "code": code or "E4001",
                    }
                )
            raise Taint()

        t = rt["t"]
        if t == "prim":
            n = rt["name"]
            if n == "int" and is_int(raw):
                return raw
            if n == "float" and is_float(raw):
                return raw
            if n == "float" and is_int(raw) and _exact_float(raw):
                return float(raw)
            if n == "bool" and is_bool(raw):
                return raw
            if n == "string" and is_str(raw):
                return raw
            if n == "null" and raw is None:
                return raw
            fail(f"expected {n}")
        if t == "lit":
            if value_eq(raw, rt["v"]):
                return raw
            fail(f"expected {json_str(str(rt['v']))}")
        if t == "range":
            if rt["base"] == "float" and is_int(raw) and _exact_float(raw):
                raw = float(raw)
            ok = is_int(raw) if rt["base"] == "int" else is_float(raw)
            if not ok:
                fail(f"expected {rt['base']} in range")
            hi_ok = raw < rt["hi"] if rt["excl"] else raw <= rt["hi"]
            if raw >= rt["lo"] and hi_ok:
                return raw
            fail(f"out of range {_num_s(rt['lo'])}..{'<' if rt['excl'] else ''}{_num_s(rt['hi'])}")
        if t == "pattern":
            if is_str(raw) and rt["re"].fullmatch(raw) is not None:
                return raw
            fail(f"does not match /{rt['src']}/")
        if t == "quantity":
            if isinstance(raw, Quantity) and raw.dim == rt["dim"]:
                return raw
            if isinstance(raw, JObj):
                es = dict(raw.entries)
                if len(es) == 2 and "value" in es and "unit" in es:
                    try:
                        u = self.env.unit_info(es["unit"])
                    except Exception:
                        fail(f"unknown unit {es['unit']}", "E4073")
                    if u["key"] != rt["dim"]:
                        fail("unit of wrong dimension", "E4073")
                    return Quantity(rt["dim"], float(es["value"]) * u["to_base"])
            fail("expected quantity")
        if t == "ref":
            if isinstance(raw, Ref):
                return raw
            if is_str(raw):
                segs = parse_path(raw, sc.root_name)
                if self.resolve_canonical(segs) is _UNDEF:
                    fail(f"dangling reference {raw}", "E6002")
                return Ref(segs)
            if isinstance(raw, (RecInst, ArrV, MapV)):
                return Ref(raw.path)
            fail("expected reference path")
        if t == "arr":
            if isinstance(raw, PreArr):
                items = []
                for spread, v in raw.items:
                    if spread:
                        s = self.deref(self.ev(v.expr, v.scope))
                        items.extend(self.mat_arr(s))
                    else:
                        items.append(v)
            elif isinstance(raw, list):
                items = raw
            elif isinstance(raw, ArrV):
                items = raw.items
            else:
                fail("expected array")
            if rt.get("lo") is not None and (len(items) < rt["lo"] or len(items) > rt["hi"]):
                fail(f"array size {len(items)} outside {rt['lo']}..{rt['hi']}")
            arr = ArrV([], path)
            for i, it in enumerate(items):
                try:
                    arr.items.append(self.bind(it, rt["elem"], [*path, i], parent, sc))
                except Taint:
                    arr.items.append(ABSENT)
            return arr
        if t == "map":
            if isinstance(raw, JObj):
                pairs = raw.entries
            elif isinstance(raw, PreObj):
                pairs = self.entries_of(raw)
            elif isinstance(raw, MapV):
                pairs = list(raw.entries.items())
            elif isinstance(raw, RecInst):
                # a record binds to a map as its value entries (§3.18)
                pairs = self.spread_entries(raw)
            else:
                fail("expected map")
            m = MapV({}, path)
            for k_, v in pairs:
                try:
                    self.bind(k_, rt["key"], path, parent, sc)
                except Taint:
                    continue
                with contextlib.suppress(Taint):
                    m.entries[k_] = self.bind(v, rt["val"], [*path, Key(k_)], parent, sc)
            return m
        if t == "union":
            if isinstance(raw, PreVal):
                raw = self.ev(raw.expr, raw.scope)  # a literal's entry: its value decides
            leaves = leaf_recs(rt)
            if isinstance(raw, (JObj, PreObj, RecInst, MapV)) and leaves:
                # record arms discriminate hierarchically (§3.12): the union is
                # flattened to its leaf records (a union arm contributes its own
                # leaves — unions are associative), and the value is routed to a
                # unique leaf, one discriminant member at a time
                def _set_of(leaf: dict[str, Any], dn: str) -> list[Any] | None:
                    m = next((x for x in leaf["members"] if x["name"] == dn), None)
                    ty = m.get("type") if m else None
                    if ty is None:
                        return None
                    if ty["t"] == "lit":
                        return [ty["v"]]
                    if ty["t"] == "union" and all(a["t"] == "lit" for a in ty["arms"]):
                        return [a["v"] for a in ty["arms"]]
                    return None

                # route: a discriminant the value supplies narrows the candidates;
                # a leaf whose set holds the value stays, and a member the value
                # omits (it has a default) is skipped
                cands = list(leaves)
                while len(cands) > 1:
                    names = [
                        m["name"]
                        for m in cands[0]["members"]
                        if all(_set_of(leaf, m["name"]) is not None for leaf in cands)
                    ]
                    narrowed = False
                    for n in names:
                        mv = self.raw_entry(raw, n)
                        if mv is _UNDEF:
                            continue
                        lv = self.raw_lit(mv)
                        kept = [
                            leaf
                            for leaf in cands
                            if any(value_eq(lv, v) for v in (_set_of(leaf, n) or []))
                        ]
                        if kept and len(kept) < len(cands):
                            cands = kept
                            narrowed = True
                            break
                    if not narrowed:
                        break
                if len(cands) == 1:
                    return self.bind(raw, cands[0], path, parent, sc)
                fail("no union arm matches discriminant")
            for arm in rt["arms"]:
                if self.kind_matches(raw, arm):
                    return self.bind(raw, arm, path, parent, sc)
            fail("no union arm matches")
        if t == "rec":
            return self.bind_record(raw, rt, path, parent, sc)
        if t == "pred":
            v = self.bind(raw, rt["base"], path, parent, sc)
            for p in rt["preds"]:
                fn = self.ev(p, Scope(None, {}, sc.root_name, sc.menv))
                try:
                    ok = self.call(fn, [v], sc)
                except Exception:
                    ok = False
                if ok is not True:
                    fail(f"predicate {json_str(_expr_name(p))} not satisfied")
            return v
        if t == "isectN":
            v = raw
            for arm in rt["arms"]:
                v = self.bind(raw, arm, path, parent, sc)
            return v
        if t == "any":
            return raw
        raise RuntimeError(f"bind: unhandled {t}")

    def raw_entry(self, raw: Any, name: str) -> Any:
        if isinstance(raw, (JObj, PreObj)):
            for k_, v in raw.entries if isinstance(raw, JObj) else self.entries_of(raw):
                if k_ == name:
                    return v
            return _UNDEF
        if isinstance(raw, MapV):
            return raw.entries.get(name, _UNDEF)
        if isinstance(raw, RecInst):
            return self.force_slot(raw, name) if name in raw.slots else _UNDEF
        return _UNDEF

    def raw_lit(self, v: Any) -> Any:
        return self.ev(v.expr, v.scope) if isinstance(v, PreVal) else v

    def kind_matches(self, raw: Any, rt: dict[str, Any]) -> bool:
        t = rt["t"]
        if t == "prim":
            n = rt["name"]
            return (
                (n == "int" and is_int(raw))
                or (n == "float" and is_float(raw))
                or (n == "bool" and is_bool(raw))
                or (n == "string" and is_str(raw))
                or (n == "null" and raw is None)
            )
        if t == "lit":
            return value_eq(self.raw_lit(raw), rt["v"])
        if t == "range":
            return is_int(raw) if rt["base"] == "int" else is_float(raw)
        if t == "pattern":
            return is_str(raw)
        if t == "arr":
            return isinstance(raw, (list, PreArr, ArrV))
        return True

    def eval_place(self, e: dict[str, Any], sc: Scope) -> list[Any] | None:
        v = self.ev_nav(e, sc)
        if isinstance(v, Segs):
            return v.segs
        if isinstance(v, Ref):
            return v.segs  # $this / $parent / $root, or a reference read through
        if isinstance(v, (RecInst, ArrV, MapV)):
            return v.path
        return None

    def ev_nav(self, e: dict[str, Any], sc: Scope) -> Any:
        # a conditional in a ref position chooses between places (§7.4):
        # only the taken branch is navigated
        if e["e"] == "if":
            return (
                self.ev_nav(e["t"], sc)
                if self.truthy(self.ev(e["c"], sc))
                else self.ev_nav(e["f"], sc)
            )
        if e["e"] == "paren":
            return self.ev_nav(e["x"], sc)
        # a step past a missing place (an absent optional member, a key or
        # index that is not there) is still a place: the chain keeps naming
        # the location, and reference integrity (§7.5) judges it when the
        # reference is bound. `?.` is an ordinary step here — a navigation in
        # a ref position denotes the place regardless of maybe-absent steps
        if e["e"] == "member":
            x0 = self.ev_nav(e["x"], sc)
            if isinstance(x0, Segs):
                return Segs([*x0.segs, e["name"]])
            x = self.deref(x0)
            v = self.access(x, e["name"])
            if v is ABSENT and isinstance(x, RecInst):
                return Segs([*x.path, e["name"]])
            return v
        if e["e"] == "index":
            x0 = self.ev_nav(e["x"], sc)
            i = self.ev(e["i"], sc)
            if isinstance(x0, Segs):
                # past a missing place a string index can only be a map key (bracket
                # access to a dot-spellable member is a compile error, §4.3)
                return Segs([*x0.segs, int(i) if is_int(i) else Key(i)])
            x = self.deref(x0)
            if isinstance(x, ArrV):
                n = int(i)
                return x.items[n] if 0 <= n < len(x.items) else Segs([*x.path, n])
            if isinstance(x, MapV):
                return x.entries[i] if i in x.entries else Segs([*x.path, Key(i)])
            if isinstance(x, RecInst):
                v = self.access(x, i)
                return Segs([*x.path, i]) if v is ABSENT else v
        return self.ev(e, sc)

    def bind_record(
        self, raw: Any, rt: dict[str, Any], path: list[Any], parent: RecInst | None, sc: Scope
    ) -> RecInst:
        if isinstance(raw, JObj):
            entries = list(raw.entries)
        elif isinstance(raw, PreObj):
            entries = self.entries_of(raw)
        elif isinstance(raw, MapV):
            # a literal no record type claimed became a map; its entries bind like a document's
            entries = list(raw.entries.items())
        elif isinstance(raw, RecInst):
            entries = [
                (n, raw.extras[n] if n in raw.extras else self.force_slot(raw, n))
                for n in raw.entry_order
                if n not in raw.slots or raw.slots[n].kind != "der"
            ]
        else:
            self.env.report(
                {
                    "severity": "error",
                    "message": "expected record",
                    "path": path_str(path),
                    "code": "E4001",
                }
            )
            raise Taint()
        inst = RecInst(rt.get("name"), rt, path, parent)
        inst.entry_order = [k_ for k_, _ in entries]
        if self.no_reg == 0:
            self.env.registry.append(inst)
        inst.menv = sc.menv
        inst.eng = self
        isc0 = Scope(inst, {}, sc.root_name, sc.menv)
        supplied = dict(entries)
        for m in rt["members"]:
            name = m["name"]
            has = name in supplied
            types = m.get("conj") or [m["type"]]
            isc = isc0.with_menv(m["menv"]) if m.get("menv") is not None else isc0
            if m["kind"] == "der":
                # a hidden member (D34) is never part of the value: a document or
                # literal that supplies it is in error — there is nothing to restate
                if has and m.get("hidden"):
                    self.env.report(
                        {
                            "severity": "error",
                            "message": f"hidden member {name} supplied",
                            "path": path_str([*path, name]),
                            "code": "E4006",
                        }
                    )
                    inst.slots[name] = Slot("der", "invalid", False, hidden=True)
                    continue
                inst.slots[name] = Slot(
                    "der",
                    "unforced",
                    mentions_referrers(m["expr"]),
                    self._mk_derived(m, inst, path, isc, has, supplied.get(name)),
                    hidden=bool(m.get("hidden")),
                )
                if inst.slots[name].deferred:
                    self.deferred_slots.append((inst, name))
                continue
            if has:
                inst.slots[name] = Slot(
                    m["kind"],
                    "unforced",
                    False,
                    self._mk_check(supplied[name], types, m, inst, path, isc),
                )
            elif m["kind"] == "dflt":
                inst.slots[name] = Slot(
                    "dflt",
                    "unforced",
                    mentions_referrers(m["dflt"]),
                    self._mk_default(m, types, inst, path, isc),
                )
            elif m["kind"] == "opt":
                inst.slots[name] = Slot("opt", "absent")
            else:
                inst.slots[name] = Slot("req", "invalid")
                self.env.report(
                    {
                        "severity": "error",
                        "message": f"required member {name} missing",
                        "path": path_str([*path, name]),
                        "code": "E4002",
                    }
                )
        for k_, v in entries:
            if any(m["name"] == k_ for m in rt["members"]):
                continue
            if rt.get("open"):
                inst.extras[k_] = v
            else:
                nm = f" {rt['name']}" if rt.get("name") else ""
                self.env.report(
                    {
                        "severity": "error",
                        "message": f"undeclared member {k_} on closed record{nm}",
                        "path": path_str([*path, k_]),
                        "code": "E4003",
                    }
                )
        return inst

    def _mk_check(self, raw_v: Any, types: Any, m: Any, inst: Any, path: Any, isc: Any) -> Any:
        def compute() -> Any:
            v = None
            for ty in types:
                v = self.bind(raw_v, ty, [*path, m["name"]], inst, isc)
            return v

        return compute

    def _mk_default(self, m: Any, types: Any, inst: Any, path: Any, isc: Any) -> Any:
        def compute() -> Any:
            if m.get("type") is not None and m["type"]["t"] == "ref" and not m.get("conj"):
                return self.bind(PreVal(m["dflt"], isc), m["type"], [*path, m["name"]], inst, isc)
            v = self.ev(m["dflt"], isc)
            out = None
            for ty in types:
                out = self.bind(v, ty, [*path, m["name"]], inst, isc)
            return out

        return compute

    def _mk_derived(self, m: Any, inst: Any, path: Any, isc: Any, has: Any, supplied_v: Any) -> Any:
        def compute() -> Any:
            # a member declared `ref<T>` holds a navigation (§7.4) -> Any: the
            # expression names a place, and is bound as one
            if m.get("type") is not None and m["type"]["t"] == "ref":
                v = self.bind(PreVal(m["expr"], isc), m["type"], [*path, m["name"]], inst, isc)
            else:
                v = self.ev(m["expr"], isc)
                if m.get("type") is not None:
                    v = self.bind(v, m["type"], [*path, m["name"]], inst, isc)
                elif isinstance(v, (PreObj, PreArr, JObj)):
                    v = self.materialize(v, [*path, m["name"]], inst, isc)
            if has:
                self.no_reg += 1
                try:
                    restated = self.bind(
                        supplied_v,
                        m["type"] if m.get("type") is not None else _structural_of(v),
                        [*path, m["name"]],
                        inst,
                        isc,
                    )
                finally:
                    self.no_reg -= 1
                if not value_eq(v, restated):
                    self.env.report(
                        {
                            "severity": "error",
                            "message": f"derived member {m['name']} restated with a differing "
                            "value",
                            "path": path_str([*path, m["name"]]),
                            "code": "E4005",
                        }
                    )
                    raise Taint()
            return v

        return compute

    def materialize(self, v: Any, path: list[Any], parent: RecInst | None, sc: Scope | None) -> Any:
        if isinstance(v, PreArr):
            # collect the flat items iteratively: a spread of an unmaterialized
            # array is walked with an explicit stack, not host recursion, so a
            # fold-with-spread chain of any depth flattens without exhausting
            # the stack (§4.2, §9.4)
            raw: list[Any] = []
            stack: list[list[Any]] = [[v.items, 0]]
            while stack:
                fitems, idx = stack[-1]
                if idx >= len(fitems):
                    stack.pop()
                    continue
                spread, it = fitems[idx]
                stack[-1][1] += 1
                x = self.ev(it.expr, it.scope) if isinstance(it, PreVal) else it
                if spread:
                    d = self.deref(x)
                    if isinstance(d, PreArr):
                        stack.append([d.items, 0])
                    else:
                        raw.extend(self.mat_arr(d))
                else:
                    raw.append(x)
            arr = ArrV([], path)
            for i, y in enumerate(raw):
                arr.items.append(self.materialize(y, [*path, i], parent, sc))
            return arr
        if isinstance(v, PreObj):
            m = MapV({}, path)
            for k_, pv in self.entries_of(v):
                m.entries[k_] = self.materialize(
                    self.ev(pv.expr, pv.scope) if isinstance(pv, PreVal) else pv,
                    [*path, Key(k_)],
                    parent,
                    sc,
                )
            return m
        return v

    def force_state(self, inst: RecInst, name: str) -> str:
        self.force_slot_safe(inst, name)
        return inst.slots[name].state

    def force_slot_safe(self, inst: RecInst, name: str) -> None:
        with contextlib.suppress(Taint, DeferSig):
            self.force_slot(inst, name)

    def _cycle_path(self, key: str) -> str:
        # the cycle as the dependency graph records it: the slots forced between
        # the re-entered one and now, each shown by its member, closing the loop
        frm = self.computing.index(key) if key in self.computing else 0
        chain = [_member_of_key(k) for k in self.computing[frm:]]
        chain.append(_member_of_key(key))
        return " -> ".join(chain)

    def force_slot(self, inst: RecInst, name: str) -> Any:
        owner = inst.eng
        if owner is not None and owner is not self:  # a frozen round's
            return owner.muted(lambda: owner.force_slot(inst, name))
        s = inst.slots.get(name)
        if s is None:
            raise EvalErr(f"no member {name}")
        key = Engine.slot_key(inst, name)
        self.record(key)
        if s.state == "ok":
            return s.value
        if s.state == "absent":
            return ABSENT
        if s.state == "invalid":
            raise Taint()
        if s.state == "deferred":
            if self.phase < 2:
                raise DeferSig()  # known to wait for phase 2: not attempted again
            s.state = "unforced"
        if s.state == "forcing":
            # re-entered from inside an edge computation it was waiting on: the
            # candidate reading it is unanswerable in this round, not a cycle
            if self.edge_bases:
                at = self.computing.index(key) if key in self.computing else -1
                if at < self.edge_bases[-1]:
                    raise DeferSig()
            self.env.report(
                {
                    "severity": "error",
                    "message": f"dependency cycle: {self._cycle_path(key)}",
                    "path": path_str([*inst.path, name]),
                    "code": "E5007",
                }
            )
            s.state = "invalid"
            raise Taint()
        s.state = "forcing"
        self.slots_by_key[key] = (inst, name)
        try:
            v = self.step(key, s.compute)
            s.state, s.value = "ok", v
            return v
        except DeferSig:
            s.state = "deferred"
            self.deferred_slots.append((inst, name))
            raise
        except EvalErr as e:
            if s.state == "forcing":
                s.state = "invalid"
            self.env.report(
                {
                    "severity": "error",
                    "message": e.msg,
                    "path": path_str([*inst.path, name]),
                    "code": e.code,
                }
            )
            raise Taint() from None
        except Exception:
            if s.state == "forcing":
                s.state = "invalid"
            raise

    def force_const(self, name: str, root_name: str) -> Any:
        return self.force_const_in(self.env, name, root_name)

    def force_const_in(self, env: Env, name: str, root_name: str) -> Any:
        if env not in self.const_envs:
            self.const_envs.append(env)
        c = env.consts[name]
        if c["state"] == "ok":
            return c["value"]
        c["state"] = "ok"
        sc = Scope(None, {}, root_name, env)
        v = self.ev(c["expr"], sc)
        if isinstance(v, (PreObj, PreArr, JObj)):
            if c.get("type") is not None:
                v = self.bind(v, env.resolve(c["type"]), [name], None, sc)
            else:
                v = self.materialize(v, [name], None, sc)
        c["value"] = v
        return v

    # ---------- driving ----------
    # every root, in insertion order, the roots added while walking (an
    # input demanded through its fallback) included — the reference walks
    # its live Map, and a snapshot would skip them in this pass
    def force_all_roots(self, deferred_too: bool) -> None:
        seen = 0
        while True:
            vals = list(self.env.roots.values())
            if seen >= len(vals):
                break
            self.force_all(vals[seen], deferred_too)
            seen += 1

    def force_all(self, v: Any, deferred_too: bool) -> None:
        if isinstance(v, RecInst):
            for n, s in list(v.slots.items()):
                self.force_slot_safe(v, n)
                if s.state == "ok":
                    self.force_all(s.value, deferred_too)
        elif isinstance(v, ArrV):
            for x in v.items:
                self.force_all(x, deferred_too)
        elif isinstance(v, MapV):
            for x in list(v.entries.values()):
                self.force_all(x, deferred_too)

    def validate_all(self, root_name: str) -> None:
        for inst in list(self.env.registry):
            self.validate_inst(inst, root_name)

    def validate_inst(self, inst: RecInst, root_name: str) -> None:
        self.step(
            f"assert:{path_str(inst.path)}",
            lambda: self.run_asserts(inst, inst.rt["asserts"], root_name),
        )

    def reset_slot(self, key: str) -> bool:
        """reset a computed slot so that it is computed again (dependency tracking)"""
        s = self.slots_by_key.get(key)
        slot = s[0].slots.get(s[1]) if s is not None else None
        if slot is None or slot.compute is None:
            return False
        if slot.state in ("ok", "invalid"):
            slot.state = "unforced"
            slot.value = None
        return True

    def run_asserts(self, inst: RecInst, asserts: list[Any], root_name: str) -> None:
        sc0 = Scope(inst, {}, root_name, inst.menv)
        for a in asserts:
            sc = sc0.with_menv(a["menv"]) if a.get("menv") is not None else sc0
            if a["kind"] == "when":
                try:
                    cond = self.ev(a["cond"], sc)
                except (Taint, EvalErr):
                    continue
                if cond is True:
                    inner = [
                        {
                            "kind": "assert",
                            "name": b["name"],
                            "cond": b["cond"],
                            "tail": b.get("tail"),
                            "origin": a.get("origin"),
                            "menv": a.get("menv"),
                        }
                        if b["m"] == "assert"
                        else {
                            "kind": "when",
                            "cond": b["cond"],
                            "body": b["body"],
                            "origin": a.get("origin"),
                            "menv": a.get("menv"),
                        }
                        for b in a["body"]
                        if b["m"] in ("assert", "when")
                    ]
                    self.run_asserts(inst, inner, root_name)
                continue
            try:
                ok = self.ev(a["cond"], sc)
            except Taint:
                continue
            except EvalErr as e:
                self.env.report(
                    {
                        "severity": "error",
                        "message": f"{a['name']}: {e.msg}",
                        "path": path_str(inst.path),
                        "code": e.code,
                    }
                )
                continue
            if ok is True:
                continue
            id_ = f"{a.get('origin') or inst.type_name}.{a['name']}"
            tail = a.get("tail")
            if tail is None:
                self.env.report(
                    {
                        "severity": "error",
                        "id": id_,
                        "message": f"assert {a['name']} failed",
                        "path": path_str(inst.path),
                        "code": "E6001",
                    }
                )
                continue
            if tail["t"] == "inline":
                msg = "".join(
                    p if is_str(p) else self.to_str(self.ev(p, sc)) for p in tail["template"]
                )
                sev = tail["severity"]
                self.env.report(
                    {
                        "severity": sev,
                        "id": id_,
                        "message": msg,
                        "path": path_str(inst.path),
                        "code": "E6001"
                        if sev == "error"
                        else "W6001"
                        if sev == "warn"
                        else "I6001",
                    }
                )
            else:
                d = (
                    inst.menv.diags.get(tail["name"]) if inst.menv is not None else None
                ) or self.env.diags[tail["name"]]
                args = [self.ev(x, sc) for x in tail["args"]]
                psc = Scope(
                    None,
                    {p["name"]: args[i] for i, p in enumerate(d["params"])},
                    root_name,
                    inst.menv,
                )
                msg = "".join(
                    p if is_str(p) else self.to_str(self.ev(p, psc)) for p in d["template"]
                )
                self.env.report(
                    {
                        "severity": d["severity"],
                        "id": id_,
                        "message": msg,
                        "path": path_str(inst.path),
                        "code": "E6001" if d["severity"] == "error" else "W6001",
                    }
                )

    # ---------- serialization ----------
    # canonical JSON of a value (§10.4, D29): derived members included,
    # hidden never; `settable_only` is the tool option D29 provides — the
    # settable projection (required, optional, defaulted members) that a
    # document for the same type would carry
    def serialize(self, v: Any, root_name: str, settable_only: bool = False) -> str:
        def fmt_f(n: float) -> str:
            s = js_num_str(n)
            return s if ("." in s or "e" in s or "E" in s) else s + ".0"

        def go(x: Any) -> str | None:
            if x is ABSENT or isinstance(x, (Closure, NatFn, StdRef)):
                return None
            if x is None:
                return "null"
            if is_bool(x):
                return "true" if x else "false"
            if is_int(x):
                return str(x)
            if is_float(x):
                return fmt_f(x)
            if is_str(x):
                return json_str(x)
            if isinstance(x, Quantity):
                unit = json_str(self.env.base_unit_of.get(x.dim, x.dim))
                return f'{{"value":{fmt_f(x.value)},"unit":{unit}}}'
            if isinstance(x, Ref):
                return json_str(path_str(x.segs, root_name))
            if isinstance(x, ArrV):
                return "[" + ",".join(s for s in (go(i) for i in x.items) if s is not None) + "]"
            if isinstance(x, MapV):
                parts = []
                for k_, val in x.entries.items():
                    g = go(val)
                    if g is not None:
                        parts.append(f"{json_str(k_)}:{g}")
                return "{" + ",".join(parts) + "}"
            if isinstance(x, RecInst):
                parts = []
                done: set[Any] = set()
                for n in x.entry_order:
                    done.add(n)
                    if n in x.extras:
                        parts.append(f"{json_str(n)}:{_raw_json(x.extras[n])}")
                        continue
                    s = x.slots.get(n)
                    if s is None or s.state in ("invalid", "absent") or s.kind == "der":
                        continue
                    g = go(s.value)
                    if g is not None:
                        parts.append(f"{json_str(n)}:{g}")
                for m in x.rt["members"]:
                    if m["name"] in done and m["kind"] != "der":
                        continue
                    if settable_only and m["kind"] == "der":
                        continue
                    s = x.slots.get(m["name"])
                    if (
                        s is None
                        or s.hidden
                        or s.state in ("invalid", "absent", "unforced", "deferred")
                    ):
                        continue
                    g = go(s.value)
                    if g is not None:
                        parts.append(f"{json_str(m['name'])}:{g}")
                return "{" + ",".join(parts) + "}"
            raise RuntimeError("serialize: unexpected value")

        return go(v) or ""


class _Undef:
    __slots__ = ()

    def __repr__(self) -> str:
        return "UNDEF"


_UNDEF = _Undef()


def _path_key(inst: RecInst) -> Any:
    return [(0, s) if is_int(s) else (1, str(s)) for s in inst.path]


def _exact_float(v: int) -> bool:
    try:
        f = float(v)
    except OverflowError:
        return False
    return math.isfinite(f) and int(f) == v


def _num_s(v: Any) -> str:
    return js_num_str(v) if is_float(v) else str(v)


def _raw_json(v: Any) -> str:
    if v is None:
        return "null"
    if is_bool(v):
        return "true" if v else "false"
    if is_int(v):
        return str(v)
    if is_float(v):
        s = js_num_str(v)
        return s if ("." in s or "e" in s or "E" in s) else s + ".0"
    if is_str(v):
        return json_str(v)
    if isinstance(v, list):
        return "[" + ",".join(_raw_json(x) for x in v) + "]"
    if isinstance(v, JObj):
        return "{" + ",".join(f"{json_str(k)}:{_raw_json(x)}" for k, x in v.entries) + "}"
    raise RuntimeError("raw_json")


def _expr_name(e: Any) -> str:
    if isinstance(e, dict) and e.get("e") == "name":
        return e["name"]
    if isinstance(e, dict) and e.get("e") == "call":
        return _expr_name(e["fn"])
    return "<predicate>"


def _structural_of(v: Any) -> dict[str, Any]:
    if is_bool(v):
        return {"t": "prim", "name": "bool"}
    if is_int(v):
        return {"t": "prim", "name": "int"}
    if is_float(v):
        return {"t": "prim", "name": "float"}
    if is_str(v):
        return {"t": "prim", "name": "string"}
    if v is None:
        return {"t": "prim", "name": "null"}
    if isinstance(v, Ref):
        return {"t": "ref", "target": {"t": "any"}}
    if isinstance(v, ArrV):
        return {"t": "arr", "elem": _structural_of(v.items[0]) if v.items else {"t": "any"}}
    if isinstance(v, Quantity):
        return {"t": "quantity", "dim": v.dim}
    return {"t": "any"}
