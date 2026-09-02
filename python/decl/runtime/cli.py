"""Native runtime CLI: `python -m decl.runtime evaluate|validate ...`.
Output formats match the reference `decl` CLI byte for byte so the
three implementations can be diffed (tests/parity/differential.py)."""
from __future__ import annotations

import json
import os
import re
import sys

from .module import load_modules, run_universe
from .parse import parse_source
from .semantics import read_json


def _print_diag(file: str, d: dict, collected, json_mode: bool) -> None:
    if json_mode:
        collected.append({"file": file, **d})
        return
    code = f" [{d['code']}]" if d.get("code") else ""
    id_ = f" {d['id']}" if d.get("id") else ""
    at = f" at {d['path']}" if d.get("path") else ""
    print(f"{file}: {d['severity']}{code}{id_}{at}: {d['message']}", file=sys.stderr)


def evaluate_file(path: str, root: str | None):
    """Returns (exit_code, value_text, diagnostics)."""
    r = load_modules(path)
    if r["diags"] or r["entry"] is None:
        return 1, None, r["diags"]
    entry = r["entry"]
    u = run_universe(r["modules"], entry)
    diags = list(u["diags"])
    errs = [d for d in diags if d["severity"] == "error"]
    if errs:
        return 1, None, diags
    eng = u["eng"]
    names = [root] if root is not None else [o["name"] for m in r["modules"] for o in m.env.outputs]
    pieces = []
    for n in names:
        v = entry.env.roots.get(n)
        if v is None:
            diags.append({"severity": "error", "message": f"no output named {n}", "path": ""})
            return 1, None, diags
        pieces.append(f"{json.dumps(n, ensure_ascii=False)}:{eng.serialize(v, n)}")
    if root is not None and len(names) == 1:
        return 0, eng.serialize(entry.env.roots[root], root), diags
    return 0, "{" + ",".join(pieces) + "}", diags


def validate_file(path: str, input_spec: str | None):
    """Evaluate a module's outputs (optionally binding one input document)."""
    r = load_modules(path)
    if r["diags"] or r["entry"] is None:
        return r["diags"]
    binds = []
    if input_spec:
        name, _, file = input_spec.partition("=")
        with open(file, encoding="utf-8") as f:
            binds.append({"input": name, "raw": read_json(f.read())})
    return run_universe(r["modules"], r["entry"], binds)["diags"]


def run_pipeline(decls: list) -> list:
    """Single-module evaluation of a fixture (no import resolution) — the
    same judgment the reference conformance runner applies."""
    from .engine import Engine
    from .semantics import Env, Scope
    env = Env()
    env.load(decls)
    eng = Engine(env)
    for o in env.outputs:
        sc = Scope(None, {}, o["name"])
        try:
            env.roots[o["name"]] = eng.bind(eng.ev(o["expr"], sc), env.resolve(o["type"]), [o["name"]], None, sc)
        except Exception:
            pass
    for v in list(env.roots.values()):
        eng.force_all(v, False)
    eng.phase = 2
    i = 0
    while i < len(eng.deferred_slots):
        inst, name = eng.deferred_slots[i]
        eng.force_slot_safe(inst, name)
        i += 1
    for v in list(env.roots.values()):
        eng.force_all(v, True)
    eng.validate_all("")
    return env.diagnostics


def judge_fixture(file: str, is_valid: bool) -> dict:
    """Runtime-level judgment: valid fixtures evaluate clean, parsing-phase
    fixtures fail to parse, binding-phase fixtures report their code;
    checking-phase fixtures need the static checker and are skipped."""
    with open(file, encoding="utf-8") as f:
        src = f.read()
    meta = dict(m for m in re.findall(r"// @([a-z-]+):\s*(.+)", src))
    phase = meta.get("expect-phase")
    want = (meta.get("expect-error") or "").strip()
    parsed = parse_source(src)
    if is_valid:
        if parsed["errors"]:
            return {"file": file, "ok": False, "detail": f"{len(parsed['errors'])} parse errors"}
        errs = [d for d in run_pipeline(parsed["decls"]) if d["severity"] == "error"]
        return {"file": file, "ok": not errs, "detail": json.dumps(errs[:2], default=str)}
    if phase == "parsing":
        return {"file": file, "ok": bool(parsed["errors"]), "detail": "expected parse errors"}
    if phase == "binding":
        diags = run_pipeline(parsed["decls"]) if not parsed["errors"] else []
        return {"file": file, "ok": any(d.get("code") == want for d in diags), "detail": json.dumps(diags[:3], default=str)}
    return {"file": file, "ok": None, "detail": f"phase {phase} needs the static checker"}


def main(argv: list[str]) -> int:
    if not argv:
        print("usage: python -m decl.runtime evaluate <file> [--root name] [--json] | validate <dir|file> [--input n=f] [--json]", file=sys.stderr)
        return 2
    cmd, args = argv[0], argv[1:]
    flags: dict = {}
    pos: list = []
    i = 0
    while i < len(args):
        a = args[i]
        if a.startswith("--"):
            name = a[2:]
            if name in ("root", "input", "expect-errors") and i + 1 < len(args) and not args[i + 1].startswith("--"):
                flags[name] = args[i + 1]
                i += 2
                continue
            flags[name] = True
        else:
            pos.append(a)
        i += 1
    json_mode = bool(flags.get("json"))
    collected: list = []
    if cmd == "evaluate":
        code, text, diags = evaluate_file(pos[0], flags.get("root"))
        for d in diags:
            _print_diag(pos[0], d, collected, json_mode)
        if json_mode:
            print(f'{{"ok":{"true" if code == 0 else "false"},"value":{text if text is not None else "null"},"diagnostics":{json.dumps(collected, ensure_ascii=False, default=str)}}}')
        elif text is not None:
            print(text)
        return code
    if cmd == "validate":
        target = pos[0]
        if os.path.isdir(target):
            ok = fail = skipped = 0
            for dirpath, _, files in os.walk(target):
                for fn in sorted(files):
                    if not fn.endswith(".decl"):
                        continue
                    fp = os.path.join(dirpath, fn)
                    v = judge_fixture(fp, "/valid/" in fp)
                    if v["ok"] is None:
                        skipped += 1
                    elif v["ok"]:
                        ok += 1
                    else:
                        fail += 1
                        print(f"FAIL {fp} {v['detail']}", file=sys.stderr)
            print(f"{ok} ok, {fail} failed, {skipped} skipped (checking phase)", file=sys.stderr)
            return 1 if fail else 0
        diags = validate_file(target, flags.get("input"))
        for d in diags:
            _print_diag(target, d, collected, json_mode)
        if json_mode:
            print(json.dumps(collected, ensure_ascii=False, default=str))
        return 1 if any(d["severity"] == "error" for d in diags) else 0
    print(f"unknown command {cmd}", file=sys.stderr)
    return 2
