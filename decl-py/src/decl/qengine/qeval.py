"""Query entry points using shared programs and the value layer's slot graph."""

from __future__ import annotations

from typing import Any

from decl.engine import Engine
from decl.semantics import Scope, sort_diags


class Unsupported(Exception):
    """Compatibility with callers that handle a query capability fallback."""

    def __init__(self, what: str) -> None:
        super().__init__(what)
        self.what = what


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


def _eval_rounds(
    entry: Any,
    roots: list[Any],
    binds: list[Any],
    hook_mods: list[Any],
    serialize_outputs: bool = True,
) -> tuple[QReport, Engine]:
    """Bind inputs and outputs through shared programs until references settle."""

    def bind(eng: Engine) -> None:
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
        for name, ty_ast, expr, menv in roots:
            eng.bind_root(name, expr, menv.resolve(ty_ast), Scope(None, {}, name, menv), True)

    eng = Engine.evaluate(entry, bind, incremental=True)
    eng.validate_all("")
    diags = sort_diags(list(entry.diagnostics))  # §6.7
    entry.diagnostics[:] = diags
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
