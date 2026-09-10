"""Revision verification over the Engine's existing slot cache.

Previous values are held only while a slot awaits verification/recomputation.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from decl.semantics import ABSENT, ArrV, MapV, Quantity, RangeV, RecInst, Ref, value_eq

if TYPE_CHECKING:
    from decl.engine import Engine


@dataclass
class Pending:
    value: Any
    deps: set[str]
    changed: int
    forced: bool
    matches: Callable[[Any], bool] | None = None


def comparable(v: Any) -> bool:
    if v is ABSENT or v is None or type(v) in (int, float, str, bool):
        return True
    if isinstance(v, RecInst):
        return False
    if isinstance(v, Ref):
        return not (v.snap or v.inverse or v.round_ref)
    if isinstance(v, ArrV):
        return all(comparable(x) for x in v.items)
    if isinstance(v, MapV):
        return all(comparable(x) for x in v.entries.values())
    return isinstance(v, (Quantity, RangeV))


def equal(a: Any, b: Any) -> bool:
    if isinstance(a, ArrV) and isinstance(b, ArrV):
        return (
            a.path == b.path
            and len(a.items) == len(b.items)
            and all(equal(x, y) for x, y in zip(a.items, b.items, strict=True))
        )
    if isinstance(a, MapV) and isinstance(b, MapV):
        return (
            a.path == b.path
            and list(a.entries) == list(b.entries)
            and all(equal(v, b.entries[k]) for k, v in a.entries.items())
        )
    return value_eq(a, b)


class Revisions:
    def __init__(self) -> None:
        self.revision = 0
        self.verified_cutoffs = 0
        self.value_cutoffs = 0
        self.recomputed = 0
        self.changed: dict[str, int] = {}
        self.pending: dict[str, Pending] = {}
        self.resolve: Callable[[Engine, str], bool] | None = None

    @property
    def tracked_queries(self) -> int:
        return len(self.changed)

    def prune(self, eng: Engine) -> None:
        self.changed = {
            key: stamp
            for key, stamp in self.changed.items()
            if key in eng.reads or key in eng.slots_by_key
        }

    def begin_queries(
        self,
        eng: Engine,
        invalid: set[str],
        values: dict[str, Any],
        forced: set[str],
        capture: Callable[[Any], Callable[[Any], bool]],
    ) -> None:
        self.pending.clear()
        self.revision += 1
        for key in invalid:
            if key in values:
                value = values[key]
                self.pending[key] = Pending(
                    value,
                    set(eng.reads.get(key, ())),
                    self.changed.get(key, 0),
                    key in forced,
                    capture(value),
                )
            self.changed[key] = self.revision

    def force(self, key: str) -> None:
        prior = self.pending.get(key)
        if prior is not None:
            prior.forced = True
        self.changed[key] = self.revision

    def accept(self, key: str, value: Any) -> None:
        prior = self.pending.pop(key, None)
        if prior is not None and (
            prior.matches(value)
            if prior.matches
            else comparable(value) and equal(prior.value, value)
        ):
            self.changed[key] = prior.changed
            self.value_cutoffs += 1

    def finish(self) -> None:
        self.pending.clear()

    def begin(
        self, eng: Engine, invalid: set[str], forced: set[str], dropped: set[RecInst]
    ) -> None:
        self.pending.clear()
        self.revision += 1
        for key in invalid:
            found = eng.slots_by_key.get(key)
            slot = found[0].slots.get(found[1]) if found else None
            if (
                found
                and found[0] not in dropped
                and slot
                and slot.state == "ok"
                and slot.compute
                and comparable(slot.value)
            ):
                self.pending[key] = Pending(
                    slot.value,
                    set(eng.reads.get(key, ())),
                    self.changed.get(key, 0),
                    key in forced,
                )
            self.changed[key] = self.revision

    def compute(self, eng: Engine, key: str, run: Callable[[], Any]) -> Any:
        prior = self.pending.get(key)
        if prior is None:
            return run()

        def verify() -> bool:
            # Verify owners before their former contents: deleted records
            # must not execute while verifying a reader's old dependencies.
            for dep in sorted(prior.deps, key=lambda d: (not d.startswith("root:"), d)):
                if dep in self.pending:
                    if self.resolve:
                        if not self.resolve(eng, dep):
                            return False
                    else:
                        found = eng.slots_by_key.get(dep)
                        if found is None:
                            return False
                        eng.force_slot(*found)
                if self.changed.get(dep, 0) == self.revision:
                    return False
            return True

        if not prior.forced and eng.verify_reads(verify):
            eng.reads[key] = prior.deps
            self.changed[key] = prior.changed
            del self.pending[key]
            self.verified_cutoffs += 1
            return prior.value
        value = run()
        self.recomputed += 1
        self.accept(key, value)
        return value
