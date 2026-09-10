"""Replay Session root binding over retained records and member revisions."""

from __future__ import annotations

from collections.abc import Callable
from copy import copy
from typing import TYPE_CHECKING, Any

from decl.qengine.revisions import Revisions
from decl.semantics import (
    ABSENT,
    ArrV,
    JObj,
    MapV,
    PreVal,
    Quantity,
    RangeV,
    RecInst,
    Ref,
    path_str,
    value_eq,
)

if TYPE_CHECKING:
    from decl.engine import Engine


def same_raw(a: Any, b: Any) -> bool:
    if a is b:
        return True
    if type(a) is not type(b):
        return False
    if isinstance(a, list):
        return len(a) == len(b) and all(same_raw(x, y) for x, y in zip(a, b, strict=True))
    if isinstance(a, JObj):
        return len(a.entries) == len(b.entries) and all(
            k == l and same_raw(x, y) for (k, x), (l, y) in zip(a.entries, b.entries, strict=True)
        )
    if isinstance(a, PreVal):
        x, y = a.scope, b.scope
        return (
            a.expr is b.expr
            and x.inst is y.inst
            and x.menv is y.menv
            and x.root_name == y.root_name
            and x.locals.keys() == y.locals.keys()
            and all(same_raw(v, y.locals[k]) for k, v in x.locals.items())
        )
    if a is None or type(a) in (int, float, str, bool) or isinstance(a, Quantity):
        return value_eq(a, b)
    return False


def capture(v: Any) -> Callable[[Any], bool]:
    if isinstance(v, RecInst):
        rt, owner, path, order = v.rt, v.eng, list(v.path), list(v.entry_order)
        members = [(n, s.kind, s.hidden) for n, s in v.slots.items()]
        extras = [(k, capture(x)) for k, x in v.extras.items()]
        return lambda n: (
            isinstance(n, RecInst)
            and n is v
            and n.rt is rt
            and n.eng is owner
            and n.path == path
            and n.entry_order == order
            and [(k, s.kind, s.hidden) for k, s in n.slots.items()] == members
            and list(n.extras) == [k for k, _ in extras]
            and all(matches(n.extras[k]) for k, matches in extras)
        )
    if isinstance(v, ArrV):
        path, items = list(v.path), [capture(x) for x in v.items]
        return lambda n: (
            isinstance(n, ArrV)
            and n.path == path
            and len(n.items) == len(items)
            and all(f(x) for f, x in zip(items, n.items, strict=True))
        )
    if isinstance(v, MapV):
        path, entries = list(v.path), [(k, capture(x)) for k, x in v.entries.items()]
        return lambda n: (
            isinstance(n, MapV)
            and n.path == path
            and list(n.entries) == [k for k, _ in entries]
            and all(f(n.entries[k]) for k, f in entries)
        )
    if isinstance(v, Ref):
        if v.snap or v.inverse or v.round_ref:
            return lambda _: False
        return lambda n: (
            isinstance(n, Ref) and not (n.snap or n.inverse or n.round_ref) and value_eq(v, n)
        )
    if (
        v is None
        or v is ABSENT
        or type(v) in (int, float, str, bool)
        or isinstance(v, (Quantity, RangeV))
    ):
        return lambda n: value_eq(v, n)
    return lambda _: False


class Edits:
    def __init__(self) -> None:
        self.active = False
        self.revisions = Revisions()
        self.slot_computes = 0
        self.retained_records = 0
        self.prepared_queries = 0
        self.inputs: dict[RecInst, dict[str, Any]] = {}
        self.roots: dict[str, tuple[Any, bool]] = {}
        self.pool: dict[str, RecInst] = {}
        self.live: set[RecInst] = set()
        self.invalid: set[str] = set()
        self.root_values: dict[str, Any] = {}
        self.root_reads: dict[str, set[str]] = {}
        self.binding_reuse = True
        self.revisions.resolve = self.resolve

    @staticmethod
    def resolve(eng: Engine, key: str) -> bool:
        slot = eng.slots_by_key.get(key)
        if slot:
            eng.force_slot(*slot)
            return True
        if key.startswith("const:"):
            index, name = key[6:].split("|", 1)
            envs = list(eng.const_envs)
            if int(index) < len(envs) and name in envs[int(index)].consts:
                eng.force_const_in(envs[int(index)], name, "")
                return True
        return False

    def retain(self, eng: Engine, v: Any, seen: set[int]) -> bool:
        if (
            v is None
            or v is ABSENT
            or type(v) in (int, float, str, bool)
            or isinstance(v, (Quantity, RangeV))
        ):
            return True
        if id(v) in seen:
            return True
        seen.add(id(v))
        if isinstance(v, Ref):
            return not (v.snap or v.inverse or v.round_ref)
        if isinstance(v, RecInst):
            return v.eng is eng and all(
                self.retain(eng, x, seen) for x in self.inputs.get(v, {}).values()
            )
        if isinstance(v, ArrV):
            return all(self.retain(eng, x, seen) for x in v.items)
        if isinstance(v, MapV):
            return all(self.retain(eng, x, seen) for x in v.entries.values())
        if isinstance(v, list):
            return all(self.retain(eng, x, seen) for x in v)
        if isinstance(v, JObj):
            return all(self.retain(eng, x, seen) for _, x in v.entries)
        if isinstance(v, PreVal):
            return (v.scope.inst is None or v.scope.inst.eng is eng) and all(
                self.retain(eng, x, seen) for x in v.scope.locals.values()
            )
        return False

    def begin(self, eng: Engine, changed: set[str], documents: dict[str, Any]) -> None:
        values: dict[str, Any] = {}
        forced: set[str] = set()
        errors = bool(eng.env.diagnostics)
        self.binding_reuse = not errors
        self.pool = {path_str(r.path): r for r in eng.env.registry}
        self.live.clear()
        self.retained_records = 0
        self.root_values = dict(eng.env.roots)
        self.root_reads = {f"root:{n}": set(eng.reads.get(f"root:{n}", ())) for n in eng.env.roots}
        known_inputs: set[str] = set()

        def collect(v: Any, into: set[str]) -> None:
            if isinstance(v, RecInst):
                for n, slot in v.slots.items():
                    key = f"{path_str(v.path)}.{n}"
                    eng.slots_by_key[key] = (v, n)
                    into.add(key)
                    if slot.state == "ok":
                        collect(slot.value, into)
            elif isinstance(v, ArrV):
                for x in v.items:
                    collect(x, into)
            elif isinstance(v, MapV):
                for x in v.entries.values():
                    collect(x, into)

        def diff(before: Any, next_: Any, value: Any, producer: str) -> None:
            if same_raw(before, next_):
                return
            known_inputs.add(producer)
            forced.add(producer)
            if isinstance(value, RecInst) and isinstance(before, JObj) and isinstance(next_, JObj):
                a, b = dict(before.entries), dict(next_.entries)
                for name in a.keys() | b.keys():
                    if (name in a) == (name in b) and same_raw(a.get(name), b.get(name)):
                        continue
                    slot = value.slots.get(name)
                    if slot:
                        key = f"{path_str(value.path)}.{name}"
                        eng.slots_by_key[key] = (value, name)
                        diff(a.get(name), b.get(name), slot.value, key)
                        forced.add(key)
            elif isinstance(value, ArrV) and isinstance(before, list) and isinstance(next_, list):
                for i in range(max(len(before), len(next_))):
                    diff(
                        before[i] if i < len(before) else None,
                        next_[i] if i < len(next_) else None,
                        value.items[i] if i < len(value.items) else None,
                        producer,
                    )
            elif isinstance(value, MapV) and isinstance(before, JObj) and isinstance(next_, JObj):
                a, b = dict(before.entries), dict(next_.entries)
                for k in a.keys() | b.keys():
                    diff(a.get(k), b.get(k), value.entries.get(k), producer)
            else:
                collect(value, forced)

        for name in changed:
            previous, value, key = self.roots.get(name), eng.env.roots.get(name), f"root:{name}"
            forced.add(key)
            known_inputs.add(key)
            if previous is not None and not previous[1] and name in documents:
                diff(previous[0], documents[name], value, key)
            else:
                collect(value, forced)
        readers: dict[str, list[str]] = {}
        for key, deps in eng.reads.items():
            for dep in deps:
                readers.setdefault(dep, []).append(key)
                if dep.startswith(("edge:", "snapshot:", "round:", "value:")):
                    forced.add(key)
        for name, value in eng.env.roots.items():
            values[f"root:{name}"] = value
        for inst in eng.env.registry:
            for name, slot in inst.slots.items():
                key = f"{path_str(inst.path)}.{name}"
                if slot.state in ("ok", "absent"):
                    values[key] = ABSENT if slot.state == "absent" else slot.value
                if errors:
                    forced.add(key)
                    eng.slots_by_key[key] = (inst, name)
        for index, env in enumerate(eng.const_envs):
            for name, con in env.consts.items():
                key = f"const:{index}|{name}"
                if con.get("state") == "ok":
                    values[key] = con.get("value")
                if errors:
                    forced.add(key)
        if eng.queried:
            for key, value in values.items():
                if not self.retain(eng, value, set()):
                    forced.add(key)
        if errors:
            forced.update(eng.reads)
        invalid: set[str] = set()
        queue = list(forced)
        if any(d.startswith("value:") for deps in eng.reads.values() for d in deps):
            queue.extend(values)
        while queue:
            key = queue.pop()
            if key in invalid:
                continue
            invalid.add(key)
            if key not in known_inputs and key in values:
                descendants: set[str] = set()
                collect(values[key], descendants)
                queue.extend(descendants - invalid)
            queue.extend(readers.get(key, ()))
        self.invalid = invalid
        self.prepared_queries = len(invalid)
        self.revisions.begin_queries(eng, invalid, values, forced, capture)
        copied: set[RecInst] = set()
        for key in invalid:
            found = eng.slots_by_key.get(key)
            if found is None:
                continue
            inst, name = found
            slot = inst.slots.get(name)
            if slot is None or slot.compute is None:
                continue
            if inst not in copied:
                inst.slots = dict(inst.slots)
                copied.add(inst)
            next_slot = copy(slot)
            next_slot.state, next_slot.value = "unforced", None
            inst.slots[name] = next_slot
        for index, env in enumerate(eng.const_envs):
            for name, con in env.consts.items():
                if f"const:{index}|{name}" in invalid:
                    con["state"] = "unforced"
                    con.pop("value", None)
        eng.env.roots.clear()
        eng.env.registry.clear()
        eng.env.diagnostics.clear()
        eng.failed_inputs.clear()
        eng.deferred_slots = []
        eng.deferred_roots = []
        eng.phase, eng.settled, eng.prev, eng.snap = 1, False, None, None
        eng.queried.clear()
        eng.ref_index.clear()
        eng.computing_edges.clear()
        eng.edge_bases = []
        eng.round_roots, eng.snapshot_value = None, None
        self.active = True

    def root(
        self, eng: Engine, name: str, raw: Any, expression: bool, run: Callable[[], Any]
    ) -> Any:
        previous = self.roots.get(name)
        self.roots[name] = (raw, expression)
        if not self.active:
            return run()
        key = f"root:{name}"
        if (
            previous is None
            or previous[1] != expression
            or not (previous[0] is raw if expression else same_raw(previous[0], raw))
        ):
            self.revisions.force(key)
        if key not in self.invalid and name in self.root_values:
            eng.reads[key] = self.root_reads.get(key, set())
            value = self.root_values[name]
            self.activate(eng, value)
            return value
        return self.compute(eng, key, run)

    def unchanged(self, inst: RecInst, entries: Any) -> bool:
        before = self.inputs.get(inst)
        return (
            self.binding_reuse
            and before is not None
            and len(before) == len(entries)
            and inst.entry_order == [k for k, _ in entries]
            and all(k in before and same_raw(before[k], v) for k, v in entries)
        )

    def record(self, rt: Any, path: Any, parent: RecInst | None) -> RecInst | None:
        if not self.active:
            return None
        old = self.pool.get(path_str(path))
        return old if old is not None and old.rt is rt and old.parent is parent else None

    def bound(self, eng: Engine, inst: RecInst, entries: Any, old_slots: Any = None) -> None:
        before, next_ = self.inputs.get(inst), dict(entries)
        if self.active:
            for name, slot in inst.slots.items():
                key = f"{path_str(inst.path)}.{name}"
                old = old_slots.get(name) if old_slots is not None else None
                if (
                    before is None
                    or (name in before) != (name in next_)
                    or not same_raw(before.get(name), next_.get(name))
                    or old is None
                    or old.kind != slot.kind
                ):
                    self.revisions.force(key)
                elif old is not None:
                    inst.slots[name] = old
                eng.slots_by_key[key] = (inst, name)
        self.inputs[inst] = next_
        if self.active:
            for slot in inst.slots.values():
                if slot.state == "ok":
                    self.activate(eng, slot.value)

    def register(self, eng: Engine, inst: RecInst) -> None:
        if inst in self.live:
            return
        self.live.add(inst)
        eng.env.registry.append(inst)
        if self.pool.get(path_str(inst.path)) is inst:
            self.retained_records += 1

    def activate(self, eng: Engine, value: Any) -> None:
        if not self.active or eng.no_reg > 0:
            return
        if isinstance(value, RecInst):
            if value.eng is not eng or value in self.live:
                return
            self.register(eng, value)
            for slot in value.slots.values():
                if slot.state == "ok":
                    self.activate(eng, slot.value)
        elif isinstance(value, ArrV):
            for v in value.items:
                self.activate(eng, v)
        elif isinstance(value, MapV):
            for v in value.entries.values():
                self.activate(eng, v)

    def compute(self, eng: Engine, key: str, run: Callable[[], Any]) -> Any:
        value = self.revisions.compute(eng, key, run)
        self.activate(eng, value)
        return value

    def absent(self, key: str) -> None:
        if self.active:
            self.revisions.accept(key, ABSENT)

    def prune(self, eng: Engine) -> None:
        # Captured scopes may refer back to their record. Explicitly prune the
        # metadata: a weak-key table's strong values would retain that cycle.
        live = set(eng.env.registry)
        self.inputs = {inst: raw for inst, raw in self.inputs.items() if inst in live}
        self.roots = {n: v for n, v in self.roots.items() if n in eng.env.roots}

    def finish(self, eng: Engine) -> None:
        self.prune(eng)
        if not self.active:
            return
        for key, (inst, _) in list(eng.slots_by_key.items()):
            if inst not in self.live:
                del eng.slots_by_key[key]
                eng.reads.pop(key, None)
        # Assertions run again after settling; removed roots and assertions
        # have no cached result whose old reads need to survive the edit.
        for key in list(eng.reads):
            if key.startswith("assert:") or (
                key.startswith("root:") and key[5:] not in eng.env.roots
            ):
                del eng.reads[key]
        self.revisions.finish()
        self.revisions.prune(eng)
        self.pool.clear()
        self.live.clear()
        self.invalid.clear()
        self.root_values.clear()
        self.root_reads.clear()
        self.active = False
