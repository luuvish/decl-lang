"""The incremental, memoized query core of the query engine (qengine/DESIGN.md);
the faithful port of decl-ts/src/qengine/db.ts and decl-rs/src/qengine/db.rs.

A demand-driven database of queries. Some keys are *inputs* (set from outside);
the rest are *derived* — computed by a resolver-supplied function that reads
other queries through the database, so its dependencies are recorded
automatically. Results are memoized with a global revision and two stamps per
entry, giving early cutoff on both axes. The verify cutoff: if none of an
entry's dependencies changed since it was verified, it is reused without
recomputing. The value cutoff: a recompute that yields an equal value does not
advance the entry's changed-revision, so its dependents cut off in turn. This is
Salsa/Adapton-shaped, generic over the value it stores, and independent of the
decl language; the decl layer (compiled expressions as derived queries) is built
on top.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from decl.semantics import EvalErr, value_eq

# a query key — an opaque string; the decl layer defines the grammar of keys
Key = str
# compute a derived query's value, reading other queries through the database
Compute = Callable[["Db"], Any]
# map a derived key to its compute; None for an unknown key
Resolve = Callable[[str], Compute | None]


class QCycle(Exception):
    """a query that (transitively) reads itself, with the path it walked (§7.6)"""

    def __init__(self, keys: list[Key]) -> None:
        super().__init__("dependency cycle")
        self.keys = keys


class _Entry:
    __slots__ = ("changed_rev", "deps", "value", "verified_rev")

    def __init__(self, value: Any, deps: list[Key], changed_rev: int, verified_rev: int) -> None:
        self.value = value
        self.deps = deps
        self.changed_rev = changed_rev  # last revision the value actually changed
        self.verified_rev = verified_rev  # last revision the value was confirmed current


class _Frame:
    __slots__ = ("deps", "key")

    def __init__(self, key: Key) -> None:
        self.key = key
        self.deps: list[Key] = []


class Db:
    """the incremental, memoized query database (see the module docstring)"""

    __slots__ = ("_input_rev", "_inputs", "_memo", "_resolve", "_rev", "_stack")

    def __init__(self, resolve: Resolve) -> None:
        self._rev = 0
        self._memo: dict[Key, _Entry] = {}
        self._inputs: dict[Key, Any] = {}
        self._input_rev: dict[Key, int] = {}
        self._resolve = resolve
        # the queries currently (re)computing, innermost last — dependency
        # capture and cycle detection
        self._stack: list[_Frame] = []

    def revision(self) -> int:
        """the current revision (advances when an input changes)"""
        return self._rev

    def set_input(self, key: str, value: Any) -> None:
        """Set an input's value. If it differs from the current value the
        revision advances and dependents recompute on demand; an equal value is
        a no-op."""
        if key in self._inputs and value_eq(self._inputs[key], value):
            return
        self._rev += 1
        self._inputs[key] = value
        self._input_rev[key] = self._rev

    def query(self, key: str) -> Any:
        """read a query's value, recording it as a dependency of the caller"""
        if self._stack:
            self._stack[-1].deps.append(key)
        return self._evaluate(key)

    def _evaluate(self, key: str) -> Any:
        """bring a query up to date and return its value, recording no dependency"""
        if key in self._inputs:
            return self._inputs[key]
        cur = self._rev
        m = self._memo.get(key)
        if m is not None and m.verified_rev == cur:
            return m.value
        if m is not None and self._deps_unchanged(m.deps, m.verified_rev):
            # verify cutoff: no dependency changed since this was verified
            m.verified_rev = cur
            return m.value
        return self._recompute(key, m is not None)

    def _deps_unchanged(self, deps: list[Key], verified: int) -> bool:
        for d in deps:
            self._evaluate(d)  # bring the dependency current (may recompute it)
            if self._changed_rev_of(d) > verified:
                return False
        return True

    def _recompute(self, key: str, has_prev: bool) -> Any:
        if any(f.key == key for f in self._stack):
            raise QCycle([f.key for f in self._stack] + [key])
        compute = self._resolve(key)
        if compute is None:
            raise EvalErr(f"no such query: {key}")
        frame = _Frame(key)
        self._stack.append(frame)
        try:
            value = compute(self)  # propagates on error; nothing is memoized
        finally:
            self._stack.pop()
        cur = self._rev
        # value cutoff: an equal recompute keeps the old changed-revision, so
        # dependents that only read this query need not recompute
        changed_rev = cur
        if has_prev:
            old = self._memo.get(key)
            if old is not None and value_eq(value, old.value):
                changed_rev = old.changed_rev
        self._memo[key] = _Entry(value, frame.deps, changed_rev, cur)
        return value

    def _changed_rev_of(self, key: str) -> int:
        if key in self._input_rev:
            return self._input_rev[key]
        m = self._memo.get(key)
        return m.changed_rev if m is not None else 0
