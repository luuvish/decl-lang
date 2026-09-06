"""The session object (docs/tooling/02_repl.md §1) — a port of the
reference implementation's session.ts: a universe (the modules loaded
from an entry file, their texts taken as a snapshot) plus an operation
log (bindings, document edits, session declarations, reloads). The state
is the universe with the log applied, recomputed deterministically from
the snapshot, which is what makes `:undo` exact and a scripted session
reproducible. The REPL (repl.py) drives it; nothing here prints, and
every answer is the same checker, inference, and engine the command line
runs."""

from __future__ import annotations

import json
import os
import re
import time
from collections.abc import Callable
from typing import Any

from .checker import check_module
from .engine import _UNDEF, Engine
from .fmt import format_source
from .infer import STD, infer, make_ctx, type_text
from .module import Module, load_modules
from .package import open_package_universe, verify_lock
from .parse import parse_expr_text, parse_source
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
    RecInst,
    Scope,
    StdRef,
    Taint,
    is_bool,
    is_float,
    is_int,
    is_str,
    js_num_str,
    json_str,
    parse_path,
    path_str,
    read_json,
    seg_text,
    sort_diags,
)
from .yaml import YamlError, is_yaml_path, read_yaml


class SessionError(Exception):
    pass


def _now() -> float:
    return time.perf_counter() * 1000.0


def _is_root_diag(d: dict[str, Any], root: str) -> bool:
    p = d.get("path", "")
    return p == root or p.startswith(root + ".") or p.startswith(root + "[")


def parse_expr(text: str) -> dict[str, Any]:
    """parse one expression: the text is wrapped in a constant declaration"""
    e = parse_expr_text(text)
    if e is None:
        raise SessionError(f"cannot parse expression: {text.strip()}")
    return e


def parse_decl(text: str) -> dict[str, Any]:
    """parse one module-level declaration; returns {"decl", "name"}"""
    r = parse_source(text.strip() + "\n")
    decls, errors = r["decls"], r["errors"]
    if errors or len(decls) != 1:
        raise SessionError(f"cannot parse declaration: {text.strip().splitlines()[0]}")
    d = decls[0]
    if isinstance(d.get("name"), str):
        name = d["name"]
    elif d["d"] == "import":
        name = f"import {d['from']}"
    else:
        name = f"{d['d']} {d['from']}"
    return {"decl": d, "name": name}


def _parse_doc(text: str, what: str, file: str | None = None) -> Any:
    """a document's text is JSON, or YAML when its file says so (docs/tooling/05_render.md §2)"""
    if file is not None and is_yaml_path(file):
        try:
            return read_yaml(text)
        except YamlError as e:
            raise SessionError(f"{what} is not well-formed YAML: {e}") from None
    try:
        return read_json(text)
    except Exception:
        raise SessionError(f"{what} is not well-formed JSON") from None


# ---------------- JSON documents (read_json's shape) ----------------
def doc_json(v: Any) -> str:
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
        return "[" + ",".join(doc_json(x) for x in v) + "]"
    if isinstance(v, JObj):
        return "{" + ",".join(f"{json_str(k)}:{doc_json(x)}" for k, x in v.entries) + "}"
    raise RuntimeError("doc_json")


def _doc_clone(v: Any) -> Any:
    return read_json(doc_json(v))


def _doc_step(v: Any, seg: Any) -> Any:
    k = seg_text(seg)
    if isinstance(v, JObj):
        for kk, x in v.entries:
            if kk == k:
                return x
        return _UNDEF
    if isinstance(v, list) and is_int(k):
        return v[k] if 0 <= k < len(v) else _UNDEF
    return _UNDEF


# ---------------- pretty printing ----------------
def pretty_json(compact: str) -> str:
    """canonical JSON, re-indented (numbers and strings untouched)"""
    out: list[Any] = []
    depth = 0
    i = 0
    n = len(compact)

    def pad() -> str:
        return "  " * depth

    while i < n:
        c = compact[i]
        if c == '"':
            j = i + 1
            while compact[j] != '"':
                if compact[j] == "\\":
                    j += 1
                j += 1
            out.append(compact[i : j + 1])
            i = j + 1
            continue
        if c in "{[":
            close = "}" if c == "{" else "]"
            if i + 1 < n and compact[i + 1] == close:
                out.append(c + close)
                i += 2
                continue
            depth += 1
            out.append(c + "\n" + pad())
            i += 1
            continue
        if c in "}]":
            depth -= 1
            out.append("\n" + pad() + c)
            i += 1
            continue
        if c == ",":
            out.append(",\n" + pad())
            i += 1
            continue
        if c == ":":
            out.append(": ")
            i += 1
            continue
        out.append(c)
        i += 1
    return "".join(out)


# ---------------- state ----------------
class Document:
    """the document a root is built from, as the session holds it"""

    __slots__ = ("base", "doc", "edited", "file", "origin")

    def __init__(
        self, origin: str, doc: Any, base: Any, file: str | None = None, edited: bool = False
    ):
        self.origin, self.doc, self.base, self.file, self.edited = origin, doc, base, file, edited


class State:
    __slots__ = ("decls", "documents", "outputs", "snapshot")

    def __init__(self, snapshot: dict[str, Any]):
        self.snapshot: dict[str, Any] = snapshot
        self.decls: dict[str, Any] = {}  # session declarations, in order
        self.outputs: dict[str, Any] = {}  # session outputs `x = e`: name -> {"type", "expr"}
        self.documents: dict[str, Any] = {}  # root -> Document


class Run:
    __slots__ = (
        "checks",
        "diags",
        "eng",
        "entry",
        "load_diags",
        "modules",
        "session_checks",
        "session_roots",
        "timing",
    )

    def __init__(
        self,
        modules: list[Any],
        entry: Module | None,
        load_diags: list[Any],
        timing: dict[str, Any],
    ):
        self.modules, self.entry, self.load_diags, self.timing = modules, entry, load_diags, timing
        self.checks: list[Any] = []  # [{"file", "diag"}]
        # session outputs whose expressions do not check (path: the output)
        self.session_checks: list[Any] = []
        self.session_roots: list[Any] = []  # the session outputs bound, as bound: (name, expr, rt)
        self.eng: Engine | None = None
        self.diags: list[Any] = []


class RootInfo:
    __slots__ = ("binding", "detail", "edited", "exported", "kind", "module", "name", "session")

    def __init__(
        self,
        kind: str,
        name: str,
        module: str,
        exported: bool,
        session: bool,
        binding: str,
        detail: str,
        edited: bool,
    ):
        self.kind, self.name, self.module, self.exported = kind, name, module, exported
        self.session, self.binding, self.detail, self.edited = session, binding, detail, edited


# ---------------- the session ----------------
class Session:
    SCRATCH = "<session>"
    # full recomputation on every question (the harness's cross-check)
    full_recompute = bool(os.environ.get("DECL_FULL_RECOMPUTE"))

    def __init__(self, entry: str | None = None, overlay: dict[str, Any] | None = None):
        self.entry_path: str | None = os.path.abspath(entry) if entry else None
        # texts that override the disk (the language server's open buffers), by absolute path
        self.overlay: dict[str, Any] = overlay if overlay is not None else {}
        self.log: list[Any] = []
        self.cursor = 0
        self.last_timing: dict[str, Any] | None = None
        # the last full run, kept for the incremental step (§6): reused as long
        # as the universe's texts and declarations are the same, its engine
        # rebinding the documents that changed and recomputing what read them
        self._last: dict[str, Any] | None = None  # {"key", "docs", "run"}
        self._snapshot0 = self._snapshot_from_disk()
        self.state = self._initial_state()

    @property
    def entry_abs(self) -> str:
        return self.entry_path or os.path.abspath(Session.SCRATCH)

    @property
    def entry_name(self) -> str:
        return os.path.basename(self.entry_path) if self.entry_path else Session.SCRATCH

    # the universe's texts as they are on disk now: the entry and every
    # module reachable from it (a module that cannot be read is absent and
    # reported on use, as the command line reports it)
    def _snapshot_from_disk(self) -> dict[str, Any]:
        snap: dict[str, Any] = {}
        if not self.entry_path:
            return snap
        pkg = open_package_universe(self.entry_path)
        r = load_modules(self.entry_path, self.overlay, pkg["resolver"] if pkg else None)
        paths = [self.entry_path] + [m.path for m in r["modules"]]
        for p in dict.fromkeys(paths):
            if p in self.overlay:
                snap[p] = self.overlay[p]
                continue
            try:
                with open(p, encoding="utf-8") as f:
                    snap[p] = f.read()
            except OSError:
                pass  # absent
        return snap

    def _initial_state(self) -> State:
        return State(self._snapshot0)

    # ---- the log ----
    def apply(self, op: dict[str, Any]) -> None:
        del self.log[self.cursor :]  # a new operation after :undo discards what was undone
        self._apply_to(self.state, op)  # a refused operation raises and is not logged
        self.log.append(op)
        self.cursor += 1

    def undo(self, n: int = 1) -> int:
        to = max(0, self.cursor - n)
        stepped = self.cursor - to
        self.cursor = to
        self._replay()
        return stepped

    def redo(self, n: int = 1) -> int:
        to = min(len(self.log), self.cursor + n)
        stepped = to - self.cursor
        self.cursor = to
        self._replay()
        return stepped

    def _replay(self) -> None:
        self.state = self._initial_state()
        for op in self.log[: self.cursor]:
            self._apply_to(self.state, op)

    def reload_op(self) -> dict[str, Any]:
        return {"op": "reload", "snapshot": self._snapshot_from_disk()}

    def _apply_to(self, st: State, op: dict[str, Any]) -> None:
        kind = op["op"]
        if kind == "bind":
            name, src = op["name"], op["src"]
            modules = self._build(st)["modules"]
            if not any(name in m.env.inputs for m in modules):
                raise SessionError(f"no input named {name}")
            if src["kind"] == "expr":
                doc = self._eval_to_doc(st, src["text"])
            else:
                doc = _parse_doc(
                    src["text"],
                    src["file"] if src["kind"] == "file" else "the document",
                    src["file"] if src["kind"] == "file" else None,
                )
            st.documents[name] = Document(
                src["kind"], doc, _doc_clone(doc), src["file"] if src["kind"] == "file" else None
            )
            return
        if kind == "unbind":
            if op["name"] not in st.documents:
                raise SessionError(f"{op['name']} is not bound")
            del st.documents[op["name"]]
            return
        if kind == "edit":
            self._edit(st, op)
            return
        if kind == "declare":
            st.decls.pop(op["name"], None)
            st.outputs.pop(op["name"], None)
            st.decls[op["name"]] = op["text"]
            return
        if kind == "output":
            st.decls.pop(op["name"], None)
            st.outputs.pop(op["name"], None)
            st.outputs[op["name"]] = {"type": op.get("type"), "expr": op["expr"]}
            return
        if kind == "drop":
            a = st.decls.pop(op["name"], None) is not None
            b = st.outputs.pop(op["name"], None) is not None
            if not a and not b:
                raise SessionError(f"no session declaration named {op['name']}")
            return
        if kind == "reload":
            st.snapshot = op["snapshot"]
            return
        if kind == "reset":
            st.decls.clear()
            st.outputs.clear()
            st.documents.clear()
            return

    # ---- documents and edits (§3) ----
    def _eval_to_doc(self, st: State, expr_text: str) -> Any:
        expr = parse_expr(expr_text)
        r = self._engine_for(st)
        if r.eng is None or r.entry is None:
            raise SessionError(self._load_failure(r))
        sc = Scope(None, {}, "", r.entry.env)

        def go(eng: Engine, env: Env) -> Any:
            try:
                v = eng.ev(expr, sc)
                v = eng.materialize(v, ["_"], None, sc)
                eng.force_all(v, True)
                text = eng.serialize(v, "")
                if not text:
                    raise SessionError("the value is not data")
                return read_json(text)
            except SessionError:
                raise
            except EvalErr as e:
                raise SessionError(e.msg) from e
            except (Taint, DeferSig):
                raise SessionError("the value is invalid") from None

        return self._scratch(r, go)

    def _edit(self, st: State, op: dict[str, Any]) -> None:
        try:
            segs = parse_path(op["path"], "")
        except Exception:
            raise SessionError(f"bad path {op['path']}") from None
        if not is_str(segs[0]) or segs[0] == "":
            raise SessionError(f"bad path {op['path']}")
        root = segs[0]
        if len(segs) < 2:
            raise SessionError(f"a path below a root is required, got {op['path']}")
        value = None if op["kind"] == "remove" else self._eval_to_doc(st, op["expr"])
        doc = self._document_of(st, root)
        parent = doc.doc
        for idx_s, s in enumerate(segs[1:-1], start=1):
            parent = _doc_step(parent, s)
            if parent is _UNDEF:
                raise SessionError(f"nothing at {path_str(segs[: idx_s + 1])}")
        last = segs[-1]
        k = seg_text(last)
        if isinstance(parent, JObj):
            idx = next((i for i, (kk, _) in enumerate(parent.entries) if kk == k), -1)
            if op["kind"] == "create":
                if idx >= 0:
                    raise SessionError(f"{op['path']} already holds a value")
                parent.entries.append((str(k), value))
            elif idx < 0:
                raise SessionError(f"nothing at {op['path']}")
            elif op["kind"] == "update":
                parent.entries[idx] = (parent.entries[idx][0], value)
            else:
                del parent.entries[idx]
        elif isinstance(parent, list) and is_int(k):
            if op["kind"] == "create":
                if k < len(parent):
                    raise SessionError(f"{op['path']} already holds a value")
                if k > len(parent):
                    raise SessionError(f"{op['path']} is past the end of the array")
                parent.append(value)
            elif k >= len(parent):
                raise SessionError(f"nothing at {op['path']}")
            elif op["kind"] == "update":
                parent[k] = value
            else:
                del parent[k]
        else:
            raise SessionError(f"{path_str(segs[:-1])} is not a record, map, or array")
        doc.edited = True

    # the document of a root, made if the root has none yet: an unbound
    # input's fallback, or an output detached into its settable projection
    def _document_of(self, st: State, root: str) -> Document:
        have = st.documents.get(root)
        if have is not None:
            return have
        b = self._build(st)
        input_mod = next((m for m in b["modules"] if root in m.env.inputs), None)
        output_mod = next(
            (m for m in b["modules"] if any(o["name"] == root for o in m.env.outputs)), None
        )
        if input_mod is None and output_mod is None:
            raise SessionError(
                f"{root} is a session output; edit the roots it reads"
                if root in st.outputs
                else f"no root named {root}"
            )
        r = self.run(st, "full")
        if r.eng is None or r.entry is None:
            raise SessionError(self._load_failure(r))
        v = r.entry.env.roots.get(root, _UNDEF)
        if v is _UNDEF or any(d["severity"] == "error" and _is_root_diag(d, root) for d in r.diags):
            raise SessionError(f"{root} is invalid; fix it before editing")
        text = r.eng.serialize(v, root, True)
        doc = read_json(text)
        d = Document("fallback" if input_mod is not None else "detached", doc, _doc_clone(doc))
        st.documents[root] = d
        return d

    # ---- building the universe ----
    def _build(self, st: State) -> dict[str, Any]:
        entry_abs = self.entry_abs
        overlay = dict(st.snapshot)
        text = st.snapshot.get(entry_abs)
        if text is None and not self.entry_path:
            text = ""
        if text is not None:
            text = detach_outputs(
                text, [n for n, d in st.documents.items() if d.origin == "detached"]
            )
            extra = list(st.decls.values())
            if extra:
                text = re.sub(r"\n?$", "\n", text, count=1) + "\n".join(extra) + "\n"
            overlay[entry_abs] = text
        pkg = open_package_universe(entry_abs) if self.entry_path else None
        pre = (list(pkg["diags"]) + verify_lock(pkg)) if pkg else []
        r = load_modules(entry_abs, overlay, pkg["resolver"] if pkg else None)
        return {"modules": r["modules"], "entry": r["entry"], "diags": pre + r["diags"]}

    def _load_failure(self, r: Run) -> str:
        d = r.load_diags[0] if r.load_diags else None
        if d is None:
            return "the universe did not load"
        return f"{'[' + d['code'] + '] ' if d.get('code') else ''}{d['message']}"

    def run(self, st: State | None = None, mode: str = "full") -> Run:
        """load, check, and (unless `mode` says otherwise) evaluate the state"""
        if st is None:
            st = self.state
        if mode == "full" and not Session.full_recompute:
            stepped = self._step_from(st)
            if stepped is not None:
                return stepped
        r = self._run_fresh(st, mode)
        if mode == "full":
            self._last = (
                {"key": self._universe_key(st), "docs": self._doc_keys(st), "run": r}
                if r.eng is not None
                else None
            )
        return r

    def _universe_key(self, st: State) -> str:
        detached = sorted(n for n, d in st.documents.items() if d.origin == "detached")
        return json.dumps(
            [
                self.entry_abs,
                list(st.snapshot.items()),
                list(st.decls.items()),
                list(st.outputs.items()),
                detached,
            ]
        )

    def _doc_keys(self, st: State) -> dict[str, Any]:
        return {n: doc_json(d.doc) for n, d in st.documents.items()}

    # the incremental step: the same universe, some documents changed
    def _step_from(self, st: State) -> Run | None:
        last = self._last
        if (
            last is None
            or last["run"].eng is None
            or last["run"].entry is None
            or last["key"] != self._universe_key(st)
        ):
            return None
        docs = self._doc_keys(st)
        changed: dict[str, Any] = {}
        for n, k in docs.items():
            if last["docs"].get(n) != k:
                changed[n] = True
        for n in last["docs"]:
            if n not in docs:
                changed[n] = True
        if not changed:
            self.last_timing = last["run"].timing
            return last["run"]
        t0 = _now()
        r: Run = last["run"]
        eng, entry = r.eng, r.entry
        assert eng is not None and entry is not None  # a run that evaluated has both
        env = entry.env

        # 1. what the change touches: the roots themselves, every slot under
        #    them, and the `$referrers` queries over types instantiated under them
        def under(path: str, root: str) -> bool:
            return path == root or path.startswith(root + ".") or path.startswith(root + "[")

        seeds: dict[str, Any] = {}
        for root in changed:
            seeds[f"root:{root}"] = True
            for k in list(eng.reads.keys()):
                if not k.startswith("root:") and under(re.sub(r"^assert:", "", k), root):
                    seeds[k] = True
            for inst in env.registry:
                if under(path_str(inst.path), root) and inst.type_name:
                    seeds[f"referrers:{inst.type_name}"] = True
        # 2. everything that read them, transitively
        readers: dict[str, Any] = {}
        for reader, rs in eng.reads.items():
            for k in rs:
                readers.setdefault(k, {})[reader] = True
        invalid: dict[str, Any] = {}
        queue = list(seeds)
        while queue:
            k = queue.pop()
            if k in invalid:
                continue
            invalid[k] = True
            for rd in readers.get(k, {}):
                if rd not in invalid:
                    queue.append(rd)
        # the roots to rebind: the changed ones, and every root that read them at binding
        rebind = [k[5:] for k in invalid if k.startswith("root:")]
        for root in rebind:
            for inst in env.registry:
                if under(path_str(inst.path), root) and inst.type_name:
                    rk = f"referrers:{inst.type_name}"
                    if rk not in invalid:
                        invalid[rk] = True
                        for rd in readers.get(rk, {}):
                            if rd not in invalid:
                                invalid[rd] = True
                                queue.append(rd)
        while queue:
            k = queue.pop()
            for rd in readers.get(k, {}):
                if rd not in invalid:
                    invalid[rd] = True
                    queue.append(rd)

        # 3. forget: the diagnostics of the invalidated steps and of the rebound roots, the slots,
        # the instances
        def gone(d: dict[str, Any]) -> bool:
            by = d.get("by")
            return (by is not None and by in invalid) or any(
                under(d.get("path") or "", root) for root in rebind
            )

        env.diagnostics[:] = [d for d in env.diagnostics if not gone(d)]
        recomputed = 0
        for k in invalid:
            if k.startswith(("root:", "assert:", "referrers:")):
                continue
            if any(under(k, root) for root in rebind):
                eng.slots_by_key.pop(k, None)
                eng.reads.pop(k, None)
                continue
            if eng.reset_slot(k):
                recomputed += 1
            eng.reads.pop(k, None)
        dropped: set[Any] = set()
        kept: list[Any] = []
        for inst in env.registry:
            if any(under(path_str(inst.path), root) for root in rebind):
                dropped.add(id(inst))
            else:
                kept.append(inst)
        env.registry[:] = kept
        for root in rebind:
            env.roots.pop(root, None)
            eng.failed_inputs.discard(root)
            eng.reads.pop(f"root:{root}", None)
        eng.deferred_slots = [d for d in eng.deferred_slots if id(d[0]) not in dropped]
        for k in list(eng.reads.keys()):
            if k.startswith("assert:") and any(under(k[7:], root) for root in rebind):
                eng.reads.pop(k, None)
        # 4. rebind the roots in the fresh run's order — the documents in the
        #    state's order, the modules' outputs in declaration order, the
        #    session's outputs — then force everything: what is `ok` stays
        #    (an unbound input is demanded through its fallback on first read)
        rebinding = set(rebind)
        eng.phase = 1
        for name, d in st.documents.items():
            if name not in rebinding:
                continue
            m = next((x for x in r.modules if name in x.env.inputs), entry)
            eng.bind_root(
                name,
                d.doc,
                m.env.resolve(m.env.inputs[name]["type"]),
                Scope(None, {}, name, m.env),
                False,
            )
        for om in r.modules:
            for o in om.env.outputs:
                if o["name"] not in rebinding:
                    continue
                eng.bind_root(
                    o["name"],
                    o["expr"],
                    om.env.resolve(o["type"]),
                    Scope(None, {}, o["name"], om.env),
                    True,
                )
        for name, expr, rt in r.session_roots:
            if name not in rebinding:
                continue
            eng.bind_root(name, expr, rt, Scope(None, {}, name, entry.env), True)
        eng.force_all_roots(False)
        eng.phase = 2
        i = 0
        while i < len(eng.deferred_slots):
            inst, name = eng.deferred_slots[i]
            eng.force_slot_safe(inst, name)
            i += 1
        eng.bind_deferred_roots()
        eng.force_all_roots(True)
        # 5. the asserts of the instances that are new or whose asserts read what changed
        for inst in list(env.registry):
            key = f"assert:{path_str(inst.path)}"
            if key not in eng.reads or key in invalid:
                eng.validate_inst(inst, "")
        env.diagnostics[:] = sort_diags(env.diagnostics)
        timing = {
            "load": 0.0,
            "check": 0.0,
            "bind": 0.0,
            "evaluate": _now() - t0,
            "total": _now() - t0,
            "recomputed": recomputed,
            "slots": len(eng.slots_by_key),
        }
        run = Run(r.modules, r.entry, r.load_diags, timing)
        run.checks, run.session_checks, run.session_roots, run.eng, run.diags = (
            r.checks,
            r.session_checks,
            r.session_roots,
            eng,
            env.diagnostics,
        )
        self._last = {"key": last["key"], "docs": docs, "run": run}
        self.last_timing = timing
        return run

    def _run_fresh(self, st: State, mode: str) -> Run:
        t0 = _now()
        b = self._build(st)
        t1 = _now()
        out = Run(
            b["modules"],
            b["entry"],
            b["diags"],
            {"load": t1 - t0, "check": 0.0, "bind": 0.0, "evaluate": 0.0, "total": 0.0},
        )

        def finish() -> Run:
            out.timing["total"] = _now() - t0
            self.last_timing = out.timing
            return out

        if b["diags"] or b["entry"] is None:
            return finish()
        entry = b["entry"]
        for m in b["modules"]:
            for d in check_module(m.decls, m.env):
                out.checks.append({"file": m.path, "diag": d})
        # session outputs: their expressions are inferred where a declared
        # output's would be checked; the inferred type is the root's type
        session_roots: list[Any] = []
        for name, o in st.outputs.items():
            taken = any(
                name in m.env.inputs or any(x["name"] == name for x in m.env.outputs)
                for m in b["modules"]
            )
            if taken:
                out.session_checks.append(
                    {
                        "severity": "error",
                        "code": "E3018",
                        "message": f"root {name} is already declared by the universe",
                        "path": name,
                    }
                )
                continue
            try:
                expr = parse_expr(o["expr"])
                before = len(out.session_checks)

                def report(code: str, msg: str, name: Any = name) -> None:
                    out.session_checks.append(
                        {"severity": "error", "code": code, "message": msg, "path": name}
                    )

                cx = self._session_ctx(st, entry.env, report, name)
                ty = infer(cx, expr)
                if len(out.session_checks) > before:
                    continue
                if o.get("type"):
                    t = parse_decl(f"output {name}: {o['type']} = 0")["decl"]
                    rt = entry.env.resolve(t["type"])
                else:
                    rt = ty["rt"] or {"t": "any"}
            except Exception as e:
                out.session_checks.append({"severity": "error", "message": str(e), "path": name})
                continue
            session_roots.append((name, expr, rt))
        out.timing["check"] = _now() - t1
        # a static error in a module stops full evaluation as it stops `decl
        # evaluate`; a session output that does not check is left out, and a
        # bare expression (lazy) evaluates over what loaded regardless
        if mode == "check" or (
            mode == "full" and any(c["diag"]["severity"] == "error" for c in out.checks)
        ):
            return finish()

        t2 = _now()
        eng = Engine(entry.env)
        for m in b["modules"]:
            menv = m.env
            menv.const_eval = (lambda e_: lambda n: eng.force_const_in(e_, n, ""))(menv)
            menv.expr_eval = (lambda e_: lambda x: eng.ev(x, Scope(None, {}, "", e_)))(menv)
        # documents first (an output may read an input, §5.5), then the
        # modules' outputs, then the session's
        for name, d in st.documents.items():
            m = next((x for x in b["modules"] if name in x.env.inputs), entry)
            rt = m.env.resolve(m.env.inputs[name]["type"])
            eng.bind_root(name, d.doc, rt, Scope(None, {}, name, m.env), False)
        for m in b["modules"]:
            for o in m.env.outputs:
                eng.bind_root(
                    o["name"],
                    o["expr"],
                    m.env.resolve(o["type"]),
                    Scope(None, {}, o["name"], m.env),
                    True,
                )
        for name, expr, rt in session_roots:
            eng.bind_root(name, expr, rt, Scope(None, {}, name, entry.env), True)
        out.session_roots = session_roots
        out.eng = eng
        out.timing["bind"] = _now() - t2
        if mode == "lazy":
            eng.phase = 2
            out.diags = entry.env.diagnostics
            return finish()
        t3 = _now()
        eng.force_all_roots(False)
        eng.phase = 2
        i = 0
        while i < len(eng.deferred_slots):
            inst, name = eng.deferred_slots[i]
            eng.force_slot_safe(inst, name)
            i += 1
        eng.bind_deferred_roots()
        eng.force_all_roots(True)
        eng.validate_all("")
        entry.env.diagnostics[:] = sort_diags(entry.env.diagnostics)  # §6.7
        out.diags = entry.env.diagnostics
        out.timing["evaluate"] = _now() - t3
        return finish()

    # an inference context over the entry's scope in which the session's
    # outputs are variables of their inferred types, in declaration order
    def _session_ctx(
        self, st: State, env: Env, report: Callable[[str, str], None], up_to: str | None = None
    ) -> Any:
        cx = make_ctx(env, report)
        for name, o in st.outputs.items():
            if name == up_to:
                break
            try:
                expr = parse_expr(o["expr"])
                quiet = make_ctx(env, lambda c, m: None)
                quiet.vars = dict(cx.vars)
                rt = infer(quiet, expr)["rt"]
                if o.get("type"):
                    t = parse_decl(f"output {name}: {o['type']} = 0")["decl"]
                    rt = env.resolve(t["type"])
                cx.vars[name] = {"rt": rt, "abs": False}
            except Exception:
                pass  # a session output that does not parse is not in scope
        return cx

    # ---- questions ----
    # the engine an expression evaluates over: the last full run's when the
    # universe evaluates (complete, so `$referrers` answers over every
    # instance, and nothing is rebuilt), else a lazy run's (bound, unforced)
    def _engine_for(self, st: State) -> Run:
        full = self.run(st, "full")
        return full if full.eng is not None else self.run(st, "lazy")

    # evaluate `f` over the run's engine and leave the run as it was: the
    # diagnostics the expression added and the instances it materialized
    # under `_` are removed, forced slots keep the values a full run gives
    def _scratch(self, r: Run, f: Callable[[Engine, Env], Any]) -> Any:
        assert r.entry is not None and r.eng is not None  # a run that evaluated has both
        env, eng = r.entry.env, r.eng
        n = len(env.diagnostics)
        reg = len(env.registry)
        roots = set(env.roots.keys())
        try:
            return f(eng, env)
        finally:
            # an input demanded through its fallback by the expression alone is not a root of the
            # run
            demanded = [k for k in env.roots if k not in roots]

            def under(p: str) -> bool:
                return any(
                    p == k or p.startswith(k + ".") or p.startswith(k + "[") for k in demanded
                )

            for k in demanded:
                env.roots.pop(k, None)
                eng.failed_inputs.discard(k)
                eng.reads.pop(f"root:{k}", None)
            for k in list(eng.reads.keys()):
                if under(re.sub(r"^assert:", "", k)):
                    eng.reads.pop(k, None)
            for k in list(eng.slots_by_key.keys()):
                if under(k):
                    eng.slots_by_key.pop(k, None)
            del env.diagnostics[n:]
            env.registry[:] = [
                inst
                for i, inst in enumerate(env.registry)
                if (i < reg and not under(path_str(inst.path)))
                or (i >= reg and inst.path[0] != "_" and not under(path_str(inst.path)))
            ]
            eng.computing.clear()

    def evaluate_expr(self, text: str) -> dict[str, Any]:
        """partial evaluation of one expression (§2.1): {"value", "diags", "error"}"""
        expr = parse_expr(text)
        r = self._engine_for(self.state)
        if r.eng is None or r.entry is None:
            return {"value": None, "diags": r.load_diags, "error": {"code": None, "message": ""}}
        sc = Scope(None, {}, "", r.entry.env)

        def go(eng: Engine, env: Env) -> dict[str, Any]:
            # the run may already have reported (a root whose binding failed); the
            # expression's own diagnostics are the ones that arise from here on,
            # plus the diagnostics of the roots it names
            all_ = env.diagnostics
            frm = len(all_)
            named = set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", text))

            def arising() -> list[Any]:
                return sort_diags(
                    [d for d in all_[:frm] if d.get("path") in named] + list(all_[frm:])
                )

            try:
                v = eng.ev(expr, sc)
                v = eng.materialize(v, ["_"], None, sc)
                eng.force_all(v, True)
                return {"value": self._value_text(eng, v), "diags": arising(), "error": None}
            except EvalErr as e:
                return {
                    "value": None,
                    "diags": arising(),
                    "error": {"code": e.code, "message": e.msg},
                }
            except (Taint, DeferSig):
                return {"value": None, "diags": arising(), "error": {"code": None, "message": ""}}

        return self._scratch(r, go)

    def _value_text(self, eng: Engine, v: Any) -> str:
        if v is ABSENT or v is _UNDEF:
            return "absent"
        if isinstance(v, (Closure, NatFn, StdRef)):
            return "<function>"
        if isinstance(v, NsRef):
            return "<namespace>"
        if isinstance(v, Pattern):
            return f"/{v.re}/"
        return eng.serialize(v, "")

    def roots(self) -> list[Any]:
        """the roots of the universe and of the session (`:roots`)"""
        b = self._build(self.state)
        out: list[Any] = []

        def rel(p: str) -> str:
            return (
                self.entry_name
                if p == self.entry_abs
                else os.path.relpath(p, os.path.dirname(self.entry_abs))
            )

        for m in b["modules"]:
            # the module's roots in declaration order, from its text as loaded
            # (a detached output is blanked from the universe but still a root)
            decls = (
                parse_source(self.state.snapshot.get(m.path, ""))["decls"]
                if m.path == self.entry_abs
                else m.decls
            )
            for decl in decls:
                if decl["d"] == "output":
                    d = self.state.documents.get(decl["name"])
                    out.append(
                        RootInfo(
                            "output",
                            decl["name"],
                            rel(m.path),
                            bool(decl.get("exported")),
                            False,
                            "detached" if d is not None and d.origin == "detached" else "",
                            "",
                            bool(d and d.edited),
                        )
                    )
                elif decl["d"] == "input":
                    d = self.state.documents.get(decl["name"])
                    if d is not None:
                        binding = "fallback" if d.origin == "fallback" else "bound"
                        detail = (
                            d.file
                            if d.origin == "file"
                            else "(inline)"
                            if d.origin == "inline"
                            else "(expression)"
                            if d.origin == "expr"
                            else ""
                        )
                    else:
                        binding = "fallback" if decl.get("fallback") is not None else "unbound"
                        detail = ""
                    out.append(
                        RootInfo(
                            "input",
                            decl["name"],
                            rel(m.path),
                            False,
                            False,
                            binding,
                            detail or "",
                            bool(d and d.edited),
                        )
                    )
        for name in self.state.outputs:
            out.append(RootInfo("output", name, "", False, True, "", "", False))
        return out

    def all_root_names(self) -> list[Any]:
        return [r.name for r in self.roots()]

    def has_root(self, name: str) -> bool:
        return name in self.all_root_names()

    def check(self) -> list[Any]:
        """static diagnostics of every module, with the file each is reported against"""
        r = self.run(self.state, "check")
        return (
            [{"file": self.entry_abs, "diag": d} for d in r.load_diags]
            + r.checks
            + [{"file": self.entry_abs, "diag": d} for d in r.session_checks]
        )

    def evaluate(self, names: list[Any]) -> dict[str, Any]:
        """full evaluation of the named roots (`:evaluate`), or of the exported outputs"""
        r = self.run(self.state, "full")
        docs: list[Any] = []
        if r.entry is None:
            return {"run": r, "docs": docs, "exported": not names}
        want = (
            names
            if names
            else [d["name"] for d in r.entry.decls if d["d"] == "output" and d.get("exported")]
        )
        for n in names:
            if not self.has_root(n):
                raise SessionError(f"no root named {n}")
        if r.eng is None:
            return {
                "run": r,
                "docs": [{"name": n, "json": None} for n in want],
                "exported": not names,
            }
        for name in want:
            v = r.entry.env.roots.get(name, _UNDEF)
            bad = v is _UNDEF or any(
                d["severity"] == "error" and _is_root_diag(d, name) for d in r.diags
            )
            docs.append({"name": name, "json": None if bad else r.eng.serialize(v, name)})
        return {"run": r, "docs": docs, "exported": not names}

    def validate(self, names: list[Any]) -> dict[str, Any]:
        """whole-document validation of the named roots (`:validate`), or of every root"""
        for n in names:
            if not self.has_root(n):
                raise SessionError(f"no root named {n}")
        r = self.run(self.state, "full")
        want = names if names else (list(r.entry.env.roots.keys()) if r.entry is not None else [])
        diags = [
            d
            for d in r.diags
            if any(_is_root_diag(d, n) for n in want) or (not d.get("path") and not names)
        ]
        verdicts = []
        for name in want:
            errors = sum(1 for d in r.diags if d["severity"] == "error" and _is_root_diag(d, name))
            if r.entry is None or name not in r.entry.env.roots:
                errors += 1 if r.eng is not None else 0
            warnings = sum(1 for d in r.diags if d["severity"] == "warn" and _is_root_diag(d, name))
            verdicts.append({"name": name, "errors": errors, "warnings": warnings})
        return {"run": r, "verdicts": verdicts, "diags": diags}

    def type_of(self, text: str) -> dict[str, Any]:
        """the static type of an expression (`:type`)"""
        expr = parse_expr(text)
        b = self._build(self.state)
        if b["entry"] is None:
            raise SessionError(
                b["diags"][0]["message"] if b["diags"] else "the universe did not load"
            )
        diags: list[Any] = []
        cx = self._session_ctx(
            self.state,
            b["entry"].env,
            lambda code, message: diags.append(
                {"severity": "error", "code": code, "message": message, "path": ""}
            ),
        )
        ty = infer(cx, expr)
        return {"type": type_text(ty["rt"]), "maybe_absent": bool(ty["abs"]), "diags": diags}

    def path_of(self, text: str) -> str:
        """the canonical path of the place a navigation names (`:path`)"""
        expr = parse_expr(text)
        r = self._engine_for(self.state)
        if r.eng is None or r.entry is None:
            raise SessionError(self._load_failure(r))
        sc = Scope(None, {}, "", r.entry.env)

        def go(eng: Engine, env: Env) -> str:
            try:
                segs = eng.eval_place(
                    expr, sc
                )  # (the last run, phase 2: a `$referrers` on the way answers)
                # a scalar member or element is a place too: its container's place, one step down
                if segs is None and expr["e"] in ("member", "index"):
                    base = eng.eval_place(expr["x"], sc)
                    if base is not None:
                        if expr["e"] == "member":
                            step: Any = expr["name"]
                        else:
                            i = eng.ev(expr["i"], sc)
                            step = int(i) if is_int(i) else Key(i)
                        segs = [*base, step]
                if segs is None and expr["e"] == "name" and expr["name"] in env.roots:
                    segs = [expr["name"]]
                if segs is None:
                    raise SessionError("the expression does not name a place")
                return path_str(segs)
            except EvalErr as e:
                raise SessionError(e.msg) from e
            except (Taint, DeferSig):
                raise SessionError("the place is invalid") from None

        return self._scratch(r, go)

    def doc_of(self, name: str) -> list[Any]:
        """the declaration a name resolves to, with its documentation (`:doc`)"""
        parts = name.split(".")
        head = parts[0]
        member = parts[1] if len(parts) > 1 else None
        # a session declaration first
        if not member and head in self.state.decls:
            return self.state.decls[head].split("\n")
        if not member and head in self.state.outputs:
            o = self.state.outputs[head]
            return [f"{head}{': ' + o['type'] if o.get('type') else ''} = {o['expr']}"]
        b = self._build(self.state)
        entry = b["entry"]
        if entry is None:
            raise SessionError(
                b["diags"][0]["message"] if b["diags"] else "the universe did not load"
            )
        mod: Module | None = entry
        target = head
        if not any(d.get("name") == head for d in entry.decls):
            im = entry.env.imports.get(head)
            if im is not None:
                mod = next((m for m in b["modules"] if m.env is im["env"]), None)
                target = im["name"]
            else:
                mod = None
        decl = (
            next((d for d in mod.decls if d.get("name") == target and d.get("loc")), None)
            if mod is not None
            else None
        )
        if mod is None or decl is None:
            raise SessionError(f"no declaration named {head}")
        text = self.state.snapshot.get(mod.path, "")
        lines = text.split("\n")
        loc = decl["loc"]
        frm = loc["sl"]
        doc_lines: list[Any] = []
        while frm > 0 and re.match(r"^\s*///", lines[frm - 1]):
            frm -= 1
            doc_lines.insert(0, lines[frm])
        body = lines[loc["sl"] : loc["el"] + 1]
        if member:
            picked: list[Any] = []
            head_re = re.compile(rf"^\s*{member}\$?\??\s*[:=]")
            for i, l in enumerate(body):
                if head_re.match(l):
                    j = i
                    ds: list[Any] = []
                    while j > 0 and re.match(r"^\s*///", body[j - 1]):
                        j -= 1
                        ds.insert(0, body[j].strip())
                    picked += [*ds, l.strip()]
            if not picked:
                raise SessionError(f"{head} has no member {member}")
            return picked
        return doc_lines + body

    def trace(self, path_text: str) -> list[Any]:
        """the derivation of a valid place, or the root cause of an invalid one (`:trace`)"""
        try:
            segs = parse_path(path_text, "")
        except Exception:
            raise SessionError(f"bad path {path_text}") from None
        root = segs[0]
        if not self.has_root(root):
            raise SessionError(f"no root named {root}")
        r = self.run(self.state, "full")
        if r.eng is None or r.entry is None:
            raise SessionError(self._load_failure(r))
        eng, entry = r.eng, r.entry
        lines: list[Any] = []
        seen: set[Any] = set()

        def short(v: Any) -> str:
            t = self._value_text(eng, v)
            return t[:57] + "..." if len(t) > 60 else t

        def walk(segs: list[Any], depth: int) -> None:
            path = path_str(segs)
            ind = "  " * depth
            if path in seen:
                lines.append(f"{ind}{path}  (above)")
                return
            seen.add(path)
            own = [d for d in r.diags if d.get("path") == path]
            parent = self._value_at(eng, entry, segs[:-1]) if len(segs) > 1 else None
            last = segs[-1]
            slot = parent.slots.get(last) if isinstance(parent, RecInst) and is_str(last) else None
            if slot is not None:
                assert isinstance(parent, RecInst)  # a slot belongs to a record instance
                kind = (
                    "derived"
                    if slot.kind == "der"
                    else "defaulted"
                    if slot.kind == "dflt"
                    else "optional"
                    if slot.kind == "opt"
                    else "required"
                )
                m = next((x for x in parent.rt["members"] if x["name"] == last), None)
                supplied = slot.kind in ("req", "opt") or (
                    slot.kind == "dflt" and last in parent.entry_order
                )
                if slot.state == "invalid":
                    lines.append(f"{ind}{path}  (invalid)")
                    for d in own:
                        lines.append(f"{ind}  {fmt_diag(d)}")
                    if not own and m is not None and m.get("expr") is not None:
                        for rd in reads_of(m["expr"]):
                            s = self._read_segs(eng, parent, rd, entry)
                            if s is not None:
                                walk(s, depth + 1)
                    return
                if slot.state == "absent":
                    lines.append(f"{ind}{path}  absent")
                    return
                has_expr = m is not None and m.get("expr") is not None
                detail = ""
                if m is not None and has_expr and not supplied:
                    detail = ": " + expr_text(m["expr"])
                who = "supplied" if supplied else kind
                lines.append(f"{ind}{path} = {short(slot.value)}  ({who}{detail})")
                if not supplied and m is not None and has_expr and depth < 6:
                    for rd in reads_of(m["expr"]):
                        s = self._read_segs(eng, parent, rd, entry)
                        if s is not None:
                            walk(s, depth + 1)
                        else:
                            lines.append(f"{ind}  {expr_text(rd)}  (not a place)")
                return
            v = self._value_at(eng, entry, segs)
            if v is _UNDEF:
                if any(d["severity"] == "error" and _is_root_diag(d, path) for d in r.diags):
                    lines.append(f"{ind}{path}  (invalid)")
                    for d in r.diags:
                        if _is_root_diag(d, path):
                            lines.append(f"{ind}  {fmt_diag(d)}")
                else:
                    lines.append(f"{ind}{path}  nothing there")
                return
            origin = (
                ("document" if root in self.state.documents else "root literal")
                if len(segs) == 1
                else "supplied"
            )
            lines.append(f"{ind}{path} = {short(v)}  ({origin})")
            for d in own:
                lines.append(f"{ind}  {fmt_diag(d)}")

        walk(segs, 0)
        return lines

    def _value_at(self, eng: Engine, entry: Module, segs: list[Any]) -> Any:
        try:
            v: Any = entry.env.roots.get(segs[0], _UNDEF)
            for s in segs[1:]:
                v = eng.deref(v)
                if isinstance(v, RecInst):
                    st = eng.force_state(v, s)
                    v = v.slots[s].value if st == "ok" else _UNDEF
                elif isinstance(v, ArrV):
                    v = v.items[s] if is_int(s) and 0 <= s < len(v.items) else _UNDEF
                elif isinstance(v, MapV):
                    v = v.entries.get(seg_text(s), _UNDEF)
                else:
                    return _UNDEF
                if v is _UNDEF or v is ABSENT:
                    return _UNDEF
            return v
        except Exception:
            return _UNDEF

    def _read_segs(
        self, eng: Engine, inst: Any, rd: dict[str, Any], entry: Module
    ) -> list[Any] | None:
        # a bare name read inside a record is a sibling member (§4.4's scope
        # chain), else a root; a chain is navigated to the place it names
        def sibling(n: str) -> Any:
            i = inst
            while i is not None:
                if n in i.slots:
                    return i
                i = i.parent
            return None

        if rd["e"] == "name":
            owner = sibling(rd["name"])
            if owner is not None:
                return [*owner.path, rd["name"]]
            return [rd["name"]] if rd["name"] in entry.env.roots else None
        sc = Scope(inst, {}, inst.path[0], entry.env)
        try:
            return eng.eval_place(rd, sc)
        except Exception:
            return None

    def complete(self, text: str, commands: list[Any]) -> list[Any]:
        """the candidates completion offers at the end of `text` (`:complete`)"""

        def uniq(xs: list[Any]) -> list[Any]:
            return sorted(set(xs))

        if text.startswith(":"):
            sp = text.find(" ")
            if sp < 0:
                return uniq([c for c in commands if c.startswith(text)])
            cmd, rest = text[:sp], text[sp + 1 :]
            parts = re.split(r"[\s,=]+", rest)
            last = parts[-1] if parts else ""

            def by(xs: list[Any]) -> list[Any]:
                return uniq([x for x in xs if x.startswith(last)])

            if cmd in (":evaluate", ":validate", ":unbind", ":diff", ":save", ":bind"):
                return by(self.all_root_names())
            if cmd == ":drop":
                return by(list(self.state.decls.keys()) + list(self.state.outputs.keys()))
            if cmd == ":set":
                return by(["pretty", "compact"])
            if cmd == ":help":
                return by(commands)
            if cmd in (":trace", ":path", ":create", ":update", ":remove"):
                return self._complete_path(last)
            return []
        m = re.search(
            r"([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_][A-Za-z0-9_$]*|\[[^\]]*\])*)\.([A-Za-z_]*)$",
            text,
        )
        if m:
            prefix = m.group(2)
            if m.group(1) == "std" or m.group(1).startswith("std."):
                ns = "" if m.group(1) == "std" else m.group(1)[4:] + "."
                return uniq(
                    [
                        k[len(ns) :].split(".")[0]
                        for k in STD
                        if k.startswith(ns) and k[len(ns) :].split(".")[0].startswith(prefix)
                    ]
                )
            rt = None
            try:
                b = self._build(self.state)
                if b["entry"] is not None:
                    cx = self._session_ctx(self.state, b["entry"].env, lambda c, mm: None)
                    rt = infer(cx, parse_expr(m.group(1)))["rt"]
            except Exception:
                return []

            def members(t: dict[str, Any] | None) -> list[Any] | None:
                if not t:
                    return None
                if t["t"] == "rec":
                    return t["members"]
                if t["t"] == "union":
                    sets = [members(a) for a in t["arms"]]
                    known = [s for s in sets if s is not None]
                    if len(known) != len(sets):
                        return None
                    return [
                        mm
                        for mm in known[0]
                        if all(any(x["name"] == mm["name"] for x in s) for s in known)
                    ]
                if t["t"] == "pred":
                    return members(t["base"])
                return None

            ms = members(rt) or []
            words = {"der": "derived", "dflt": "defaulted", "opt": "optional"}
            return uniq(
                [
                    f"{x['name']}{'$' if x.get('hidden') else ''}  "
                    f"{words.get(x['kind'], 'required')}"
                    f"{': ' + type_text(x['type']) if x.get('type') else ''}"
                    for x in ms
                    if x["name"].startswith(prefix)
                ]
            )
        w = re.search(r"([A-Za-z_$][A-Za-z0-9_$]*)$", text)
        prefix = w.group(1) if w else ""
        if prefix.startswith("$"):
            return uniq(
                [
                    x
                    for x in ["$this", "$parent", "$root", "$key", "$path", "$referrers"]
                    if x.startswith(prefix)
                ]
            )
        names: list[Any] = ["std"]
        b = self._build(self.state)
        if b["entry"] is not None:
            e = b["entry"].env
            names += (
                list(e.type_asts.keys())
                + list(e.consts.keys())
                + list(e.funcs.keys())
                + list(e.inputs.keys())
                + [o["name"] for o in e.outputs]
                + list(e.imports.keys())
                + list(e.namespaces.keys())
                + list(e.diags.keys())
            )
        names += list(self.state.outputs.keys())
        kw = [
            "if",
            "then",
            "else",
            "for",
            "in",
            "match",
            "with",
            "matches",
            "true",
            "false",
            "null",
        ]
        return uniq([n for n in names + kw if n.startswith(prefix)])

    def _complete_path(self, partial: str) -> list[Any]:
        m = re.match(r"^(.*?)(?:\.([A-Za-z_][A-Za-z0-9_]*)?|\[(\"?)([^\]]*)?)?$", partial)
        base = m.group(1) if m else partial
        if not m or ("." not in partial and "[" not in partial):
            return sorted(n for n in self.all_root_names() if n.startswith(partial))
        r = self.run(self.state, "full")
        if r.eng is None or r.entry is None:
            return []
        try:
            segs = parse_path(base, "")
        except Exception:
            return []
        v = r.eng.deref(self._value_at(r.eng, r.entry, segs))
        out: list[Any] = []
        if isinstance(v, RecInst):
            for n, s in v.slots.items():
                if s.hidden:
                    continue
                out.append(f"{base}.{n}")
        if isinstance(v, MapV):
            for k in v.entries:
                out.append(f"{base}[{json_str(k)}]")
        if isinstance(v, ArrV):
            for i in range(len(v.items)):
                out.append(f"{base}[{i}]")
        return sorted(x for x in out if x.startswith(partial))

    # ---- the scratch module (§4) ----
    def scratch_text(self) -> str:
        parts: list[Any] = [t.strip() for t in self.state.decls.values()]
        for n, o in self.state.outputs.items():
            parts.append(
                f"output {n}: {o['type'] or self._inferred_type_text(o['expr'])} = {o['expr']}"
            )
        return "\n".join(parts) + "\n" if parts else ""

    def _inferred_type_text(self, expr: str) -> str:
        try:
            return self.type_of(expr)["type"]
        except Exception:
            return "any"

    def module_text(self) -> str:
        """the scratch module as a file: imports of the entry's exports it uses, then the "
        "declarations"""
        body = self.scratch_text()
        b = self._build(self.state)
        used = set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", body))
        names = sorted(n for n in b["entry"].exports if n in used) if b["entry"] is not None else []
        header = (
            f'import {{ {", ".join(names)} }} from "./{os.path.basename(self.entry_path)}"\n\n'
            if names and self.entry_path
            else ""
        )
        return header + body

    def fmt(self) -> str:
        t = self.scratch_text()
        return format_source(t) if t else ""

    def write(self, file: str) -> None:
        try:
            with open(file, "w", encoding="utf-8") as f:
                f.write(self.module_text())
        except OSError:
            raise SessionError(f"cannot write {file}") from None

    # ---- documents out (§3) ----
    def document_text(self, name: str) -> str:
        d = self.state.documents.get(name)
        if d is not None:
            return doc_json(d.doc)
        if not self.has_root(name):
            raise SessionError(f"no root named {name}")
        docs = self.evaluate([name])["docs"]
        if docs[0]["json"] is None:
            raise SessionError(f"{name} is invalid")
        return docs[0]["json"]

    def save(self, name: str, file: str) -> None:
        text = self.document_text(name)
        try:
            with open(file, "w", encoding="utf-8") as f:
                f.write(text + "\n")
        except OSError:
            raise SessionError(f"cannot write {file}") from None

    def diff(self, name: str) -> list[Any]:
        d = self.state.documents.get(name)
        if d is None:
            raise SessionError(
                f"{name} holds no document" if self.has_root(name) else f"no root named {name}"
            )
        before, after = doc_json(d.base), doc_json(d.doc)
        if before == after:
            return ["(no changes)"]
        return line_diff(pretty_json(before).split("\n"), pretty_json(after).split("\n"))

    # ---- introspection ----
    def session_lines(self) -> list[Any]:
        out: list[Any] = []
        for n, t in self.state.decls.items():
            out.append(f"declaration  {n:<16} {t.strip().split(chr(10))[0]}")
        for n, o in self.state.outputs.items():
            out.append(
                f"output       {n:<16} {n}{': ' + o['type'] if o.get('type') else ''} = {o['expr']}"
            )
        for n, d in self.state.documents.items():
            out.append(
                f"document     {n:<16} "
                f"{d.origin}{' ' + d.file if d.file else ''}{' (edited)' if d.edited else ''}"
            )
        return out

    def history_lines(self) -> list[Any]:
        out = [f"{'*' if self.cursor == 0 else ' '} 0  (start)"]
        for i, op in enumerate(self.log):
            out.append(f"{'*' if self.cursor == i + 1 else ' '} {i + 1}  {op_text(op)}")
        return out

    def script_lines(self) -> list[Any]:
        return [op_text(op) for op in self.log[: self.cursor]]


# ---------------- helpers ----------------
def fmt_diag(d: dict[str, Any], in_file: str | None = None) -> str:
    code = f" [{d['code']}]" if d.get("code") else ""
    id_ = f" {d['id']}" if d.get("id") else ""
    at = f" at {d['path']}" if d.get("path") else ""
    where = f" (in {in_file})" if in_file else ""
    return f"{d['severity']}{code}{id_}{at}: {d['message']}{where}"


def op_text(op: dict[str, Any]) -> str:
    k = op["op"]
    if k == "bind":
        src = op["src"]
        if src["kind"] == "file":
            return f":bind {op['name']}={src['file']}"
        if src["kind"] == "inline":
            return f":bind {op['name']} {doc_json(read_json(src['text']))}"
        return f":bind {op['name']} = {src['text'].strip()}"
    if k == "unbind":
        return f":unbind {op['name']}"
    if k == "edit":
        expr = f" = {op['expr'].strip()}" if op.get("expr") is not None else ""
        return f":{op['kind']} {op['path']}{expr}"
    if k == "declare":
        return op["text"].strip()
    if k == "output":
        return f"{op['name']}{': ' + op['type'] if op.get('type') else ''} = {op['expr'].strip()}"
    if k == "drop":
        return f":drop {op['name']}"
    if k == "reload":
        return ":reload"
    return ":reset"


def detach_outputs(text: str, names: list[Any]) -> str:
    """a detached output (§3): its declaration becomes `input name: T` in the
    session's copy of the module — the name stays declared, the checker
    sees a root of the same type, and the session binds the projected
    document to it; line numbers are kept"""
    if not names:
        return text
    decls = parse_source(text)["decls"]
    lines = text.split("\n")
    for d in decls:
        if d["d"] != "output" or d["name"] not in names or not d.get("loc"):
            continue
        loc = d["loc"]
        src = "\n".join(lines[loc["sl"] : loc["el"] + 1])
        colon = src.find(":", src.find(d["name"]))
        # the type text: from the colon to the `=` at bracket depth 0
        depth = 0
        eq = -1
        i = colon + 1
        while i < len(src):
            c = src[i]
            if c in "{[(<":
                depth += 1
            elif c in "}])>":
                depth -= 1
            elif (
                c == "="
                and depth == 0
                and src[i + 1 : i + 2] != "="
                and src[i - 1] not in ("!", "<", ">")
            ):
                eq = i
                break
            i += 1
        type_text_ = src[colon + 1 :].strip() if eq < 0 else src[colon + 1 : eq].strip()
        one_line = re.sub(r"\s*\n\s*", " ", type_text_)
        lines[loc["sl"]] = f"input {d['name']}: {one_line}"
        for j in range(loc["sl"] + 1, loc["el"] + 1):
            lines[j] = ""
    return "\n".join(lines)


def reads_of(e: dict[str, Any]) -> list[Any]:
    """the places an expression reads, as navigation chains (a static
    approximation of the engine's read set: names, members, indexes)"""
    out: list[Any] = []

    def is_chain(x: Any) -> bool:
        if not isinstance(x, dict) or "e" not in x:
            return False
        return x["e"] in ("name", "ctx") or (x["e"] in ("member", "index") and is_chain(x.get("x")))

    def go(x: Any) -> None:
        if not isinstance(x, (dict, list)):
            return
        if isinstance(x, list):
            for v in x:
                go(v)
            return
        if is_chain(x) and x.get("e") in ("member", "index"):
            out.append(x)
            if x["e"] == "index":
                go(x["i"])
            return
        if x.get("e") == "name":
            out.append(x)
            return
        for v in x.values():
            if isinstance(v, (list, dict)):
                go(v)

    go(e)
    return [x for x in out if x["e"] != "name" or x["name"] not in ("true", "false", "null")]


def expr_text(e: Any) -> str:
    """an expression's text, for chains and simple forms (the trace view)"""
    k = e.get("e")
    if k == "lit":
        v = e["v"]
        if is_int(v):
            return str(v)
        if is_str(v):
            return json_str(v)
        if is_bool(v):
            return "true" if v else "false"
        if v is None:
            return "null"
        return js_num_str(v) if is_float(v) else str(v)
    if k == "unitlit":
        return f"{js_num_str(e['num'])}{e['unit']}"
    if k in ("name", "ctx"):
        return e["name"]
    if k == "member":
        return f"{expr_text(e['x'])}{'?.' if e.get('safe') else '.'}{e['name']}"
    if k == "index":
        return f"{expr_text(e['x'])}[{expr_text(e['i'])}]"
    if k == "paren":
        return f"({expr_text(e['x'])})"
    if k == "bin":
        return f"{expr_text(e['l'])} {e['op']} {expr_text(e['r'])}"
    if k == "un":
        return f"{e['op']}{expr_text(e['x'])}"
    if k == "call":
        return f"{expr_text(e['fn'])}({', '.join(expr_text(a) for a in e['args'])})"
    if k == "if":
        return f"if {expr_text(e['c'])} then {expr_text(e['t'])} else {expr_text(e['f'])}"
    if k == "referrers":
        return f"$referrers({e['type']}, {json_str(e['member'])})"
    if k == "template":
        return (
            "`" + "".join(p if is_str(p) else "${" + expr_text(p) + "}" for p in e["parts"]) + "`"
        )
    if k == "obj":
        return (
            "{ " + ", ".join(f"{en['key']}: {expr_text(en['val'])}" for en in e["entries"]) + " }"
        )
    if k == "arr":
        return (
            "["
            + ", ".join(
                ("..." if it.get("spread") else "") + expr_text(it["expr"]) for it in e["items"]
            )
            + "]"
        )
    if k == "comp":
        return (
            "[for "
            + ", ".join(f"{c['v']} in {expr_text(c['iter'])}" for c in e["clauses"])
            + " … ]"
        )
    if k == "lambda":
        return f"({', '.join(e['params'])}) => …"
    if k == "with":
        return f"{expr_text(e['base'])} with …"
    if k == "match":
        return f"match {expr_text(e['subject'])} {{ … }}"
    return "…"


def line_diff(a: list[Any], b: list[Any]) -> list[Any]:
    """a minimal line diff (longest common subsequence)"""
    n, m = len(a), len(b)
    dp = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        for j in range(m - 1, -1, -1):
            dp[i][j] = dp[i + 1][j + 1] + 1 if a[i] == b[j] else max(dp[i + 1][j], dp[i][j + 1])
    out: list[Any] = []
    i = j = 0
    while i < n and j < m:
        if a[i] == b[j]:
            out.append(f"  {a[i]}")
            i += 1
            j += 1
        elif dp[i + 1][j] >= dp[i][j + 1]:
            out.append(f"- {a[i]}")
            i += 1
        else:
            out.append(f"+ {b[j]}")
            j += 1
    while i < n:
        out.append(f"- {a[i]}")
        i += 1
    while j < m:
        out.append(f"+ {b[j]}")
        j += 1
    return out
