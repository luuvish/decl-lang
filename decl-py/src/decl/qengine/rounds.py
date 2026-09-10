"""Reference-round reuse over the value layer; mirrors qengine/rounds.ts.

Member reads, roots, frozen member reads and edge answers are dependencies.
Context-bearing or unfinished snapshots retain fresh-round evaluation.
"""

from __future__ import annotations

from copy import copy
from typing import TYPE_CHECKING, Any

from decl.semantics import ArrV, JObj, MapV, Quantity, RangeV, RecInst, Ref, path_str, value_eq

if TYPE_CHECKING:
    from decl.engine import Engine


def slot_key(inst: RecInst, name: str) -> str:
    return f"{path_str(inst.path)}.{name}"


def same(a: Any, b: Any) -> bool:
    """Container shape equality; member values have separate read dependencies."""
    if a is b:
        return True
    if isinstance(a, RecInst) and isinstance(b, RecInst):
        return (
            a.type_name == b.type_name
            and a.rt is b.rt
            and a.path == b.path
            and [(n, s.kind, s.hidden) for n, s in a.slots.items()]
            == [(n, s.kind, s.hidden) for n, s in b.slots.items()]
            and a.entry_order == b.entry_order
            and list(a.extras) == list(b.extras)
            and all(same(v, b.extras[k]) for k, v in a.extras.items())
        )
    if isinstance(a, ArrV) and isinstance(b, ArrV):
        return (
            a.path == b.path
            and len(a.items) == len(b.items)
            and all(same(x, y) for x, y in zip(a.items, b.items, strict=True))
        )
    if isinstance(a, MapV) and isinstance(b, MapV):
        return (
            a.path == b.path
            and list(a.entries) == list(b.entries)
            and all(same(v, b.entries[k]) for k, v in a.entries.items())
        )
    if isinstance(a, (RecInst, ArrV, MapV)) or isinstance(b, (RecInst, ArrV, MapV)):
        return False
    return value_eq(a, b)


def snapshot_result(v: Any, eng: Engine, prefix: str, seen: set[int] | None = None) -> bool:
    """Forwarded inverse references and frozen record views depend on the epoch."""
    if not isinstance(v, (Ref, RecInst, ArrV, MapV)):
        return False
    seen = set() if seen is None else seen
    if id(v) in seen:
        return False
    seen.add(id(v))
    if isinstance(v, Ref):
        return bool(v.inverse or v.snap)
    if isinstance(v, ArrV):
        return any(snapshot_result(x, eng, prefix, seen) for x in v.items)
    if isinstance(v, MapV):
        return any(snapshot_result(x, eng, prefix, seen) for x in v.entries.values())
    path = path_str(v.path)
    if (v.eng is not None and v.eng is not eng) or not (
        path == prefix or path.startswith(prefix + ".") or path.startswith(prefix + "[")
    ):
        return True
    return any(snapshot_result(s.value, eng, prefix, seen) for s in v.slots.values()) or any(
        snapshot_result(x, eng, prefix, seen) for x in v.extras.values()
    )


class IneligibleSnapshot(Exception):
    """The snapshot contains a value requiring fresh-round evaluation."""


class RoundCache:
    """A retained dependency graph uses the slots as its single value cache."""

    @staticmethod
    def needed(env: Any, seen: set[Any] | None = None) -> bool:
        from decl.semantics import mentions_referrers

        seen = set() if seen is None else seen
        if env in seen:
            return False
        seen.add(env)
        asts = [
            *env.type_asts.values(),
            *env.funcs.values(),
            *env.inputs.values(),
            *env.outputs,
            *env.unit_decls.values(),
            *[[c["expr"], c.get("type")] for c in env.consts.values()],
        ]
        return (
            mentions_referrers(asts)
            or any(RoundCache.needed(im["env"], seen) for im in env.imports.values())
            or any(RoundCache.needed(ns["env"], seen) for ns in env.namespaces.values())
        )

    def __init__(self) -> None:
        self.clean_records: set[RecInst] = set()
        self.rounds = 0
        self.reused_rounds = 0
        self.retained_records = 0
        self.invalidated_slots = 0
        self.rebased: dict[int, tuple[Any, Any]] = {}

    def value(self, v: Any, previous: Engine | None) -> Any:
        if previous is None or not isinstance(v, (Ref, ArrV, MapV)):
            return v
        hit = self.rebased.get(id(v))
        if hit is not None:
            return hit[1]
        result = v
        if isinstance(v, Ref) and v.round_ref:
            result = Ref(v.segs, previous, True)
        elif isinstance(v, ArrV):
            items = [self.value(x, previous) for x in v.items]
            if any(x is not y for x, y in zip(items, v.items, strict=True)):
                result = ArrV(items, v.path)
        elif isinstance(v, MapV):
            entries = {k: self.value(x, previous) for k, x in v.entries.items()}
            if any(x is not v.entries[k] for k, x in entries.items()):
                result = MapV(entries, v.path)
        self.rebased[id(v)] = (v, result)
        return result

    def advance(self, eng: Engine, edges: dict[str, Any]) -> Engine | None:
        self.rounds += 1
        env = eng.env
        if env.diagnostics or any(
            s.state not in ("ok", "absent") for r in env.registry for s in r.slots.values()
        ):
            return None
        prior_records = (
            {path_str(r.path): r for r in (eng.prev.frozen_registry or [])} if eng.prev else {}
        )
        records = {path_str(r.path): r for r in env.registry}
        if len(records) != len(env.registry):
            return None
        reverse: dict[str, set[str]] = {}
        pending: list[str] = []
        indexes: dict[int, dict[str, list[str]]] = {}

        def answer(edge: dict[str, Any], target: str) -> list[str]:
            index = indexes.get(id(edge))
            if index is None:
                index = {}
                indexes[id(edge)] = index
                for source, row in edge.items():
                    for dest in {path_str(p) for p in row["refs"]}:
                        index.setdefault(dest, []).append(source)
                for paths in index.values():
                    paths.sort()
            return index.get(target, [])

        empty: dict[str, Any] = {}
        for reader, deps in eng.reads.items():
            for dep in deps:
                reverse.setdefault(dep, set()).add(reader)
                if dep.startswith("edge:"):
                    type_name, member, target = dep[5:].split("|", 2)
                    key = f"{type_name}|{member}"
                    prior = eng.snap["edges"].get(key, empty) if eng.snap else empty
                    if answer(prior, target) != answer(edges.get(key, empty), target):
                        pending.append(dep)
                elif dep == "round:nested":
                    pending.append(dep)
                elif dep == "round:reference":
                    slot = eng.slots_by_key.get(reader)
                    prefix = reader[5:] if reader.startswith("root:") else reader
                    value = slot[0].slots[slot[1]].value if slot else env.roots.get(prefix)
                    if reader.startswith("const:") or snapshot_result(value, eng, prefix):
                        pending.append(reader)
                elif dep.startswith("snapshot:"):
                    cur = eng.slots_by_key.get(dep[9:])
                    old = prior_records.get(path_str(cur[0].path)) if cur else None
                    a = old.slots.get(cur[1]) if old and cur else None
                    b = cur[0].slots.get(cur[1]) if cur else None
                    av = a.value if a else None
                    if eng.prev and eng.prev.snapshot_value:
                        av = eng.prev.snapshot_value(av)
                    if a is None or b is None or a.state != b.state or not same(av, b.value):
                        pending.append(dep)
        invalid: set[str] = set()
        dropped: set[RecInst] = set()
        roots: set[str] = set()
        subtrees: dict[str, list[RecInst]] = {}
        for inst in records.values():
            for i in range(1, len(inst.path) + 1):
                subtrees.setdefault(path_str(inst.path[:i]), []).append(inst)

        def add_subtree(path: list[Any]) -> None:
            for inst in subtrees.get(path_str(path), []):
                if inst in dropped:
                    continue
                dropped.add(inst)
                pending.extend(slot_key(inst, name) for name in inst.slots)

        while pending:
            key = pending.pop()
            if key in invalid:
                continue
            invalid.add(key)
            if key.startswith("const:"):
                return None
            pending.extend(reverse.get(key, ()))
            if key.startswith("root:"):
                name = key[5:]
                roots.add(name)
                add_subtree([name])
            else:
                found = eng.slots_by_key.get(key)
                if found:
                    add_subtree([*found[0].path, found[1]])
        rebound = False
        for name in env.roots:
            rebound = rebound or name in roots
            if rebound and name not in roots:
                return None
        dirty: set[RecInst] = set()
        for key in invalid:
            found = eng.slots_by_key.get(key)
            inst = found[0] if found else None
            while inst is not None:
                dirty.add(inst)
                inst = inst.parent
        try:
            snapshot = self.freeze(eng)
        except IneligibleSnapshot:
            return None
        for inst in dropped:
            for name in inst.slots:
                key = slot_key(inst, name)
                eng.reads.pop(key, None)
                eng.slots_by_key.pop(key, None)
        copied_slots: set[RecInst] = set()
        for key in invalid:
            found = eng.slots_by_key.get(key)
            if found:
                if found[0] not in copied_slots:
                    found[0].slots = dict(found[0].slots)
                    copied_slots.add(found[0])
                inst, name = found
                slot = inst.slots[name]
                if slot.compute is not None:
                    fresh = copy(slot)
                    fresh.state, fresh.value = "unforced", None
                    inst.slots[name] = fresh
                    self.invalidated_slots += 1
            eng.reads.pop(key, None)
        env.registry[:] = [r for r in env.registry if r not in dropped]
        self.clean_records = set(env.registry) - dirty
        for name in roots:
            env.roots.pop(name, None)
        eng.deferred_slots = []
        eng.deferred_roots = []
        eng.round_roots = roots
        eng.phase = 1
        eng.snap = None
        eng.queried = {
            "|".join(dep[5:].split("|", 2)[:2])
            for deps in eng.reads.values()
            for dep in deps
            if dep.startswith("edge:")
        }
        eng.ref_index.clear()
        eng.computing_edges.clear()
        eng.edge_bases = []
        for reset in eng.round_resets:
            reset()
        self.rebased = {}
        for name, value in env.roots.items():
            env.roots[name] = self.value(value, snapshot)
        self.retained_records += len(env.registry)
        self.reused_rounds += 1
        return snapshot

    def freeze(self, eng: Engine) -> Engine:
        frozen = copy(eng)
        copies: dict[int, Any] = {}
        previous = eng.prev
        normalizer = RoundCache()

        def clone(raw: Any) -> Any:
            v = normalizer.value(raw, previous)
            if type(v) in (type(None), bool, int, float, str):
                return v
            if id(v) in copies:
                return copies[id(v)]
            if isinstance(v, Ref):
                return Ref(v.segs, v.snap, inverse=True) if v.round_ref else v
            if isinstance(v, (Quantity, RangeV, JObj)):
                return v
            if isinstance(v, RecInst):
                if v.eng is not None and v.eng is not eng:
                    return v
                r = RecInst(v.type_name, v.rt, v.path, None)
                copies[id(v)] = r
                r.eng, r.menv = frozen, v.menv
                r.entry_order = v.entry_order
                r.parent = clone(v.parent) if v.parent else None
                r.slots = v.slots
                r.extras = v.extras
                return r
            if isinstance(v, ArrV):
                a = ArrV([], v.path)
                copies[id(v)] = a
                a.items = [clone(x) for x in v.items]
                if all(x is y for x, y in zip(a.items, v.items, strict=True)):
                    copies[id(v)] = v
                    return v
                return a
            if isinstance(v, MapV):
                m = MapV({}, v.path)
                copies[id(v)] = m
                m.entries = {k: clone(x) for k, x in v.entries.items()}
                if all(x is v.entries[k] for k, x in m.entries.items()):
                    copies[id(v)] = v
                    return v
                return m
            from decl.semantics import ABSENT

            if v is ABSENT:
                return v
            raise IneligibleSnapshot("context-bearing snapshot")

        frozen.frozen_registry = [clone(r) for r in eng.env.registry]
        frozen.frozen_roots = {k: clone(v) for k, v in eng.env.roots.items()}
        for inst in eng.env.registry:
            for slot in inst.slots.values():
                clone(slot.value)
            for value in inst.extras.values():
                clone(value)
        frozen.slots_by_key = {}
        frozen.snapshot_value = clone
        frozen.round_cache = None
        frozen.round_roots = None
        frozen.track = False
        frozen.computing = []
        frozen.reads = {}
        return frozen
