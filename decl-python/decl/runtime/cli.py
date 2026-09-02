"""Native runtime CLI: `python -m decl.runtime evaluate|validate ...`.
Output formats match the reference `decl` CLI byte for byte so the
three implementations can be diffed (tests/parity/differential.py)."""
from __future__ import annotations

import json
import os
import re
import sys

from .checker import check_module
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
    checks = [dict(d, file=m.path) for m in r["modules"] for d in check_module(m.decls, m.env)]
    if checks:
        return 1, None, checks
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
    """Static checks, then evaluation (optionally binding one input document);
    returns (parse_error_count, diagnostics)."""
    with open(path, encoding="utf-8") as f:
        src = f.read()
    parsed = parse_source(src)
    if parsed["errors"]:
        return len(parsed["errors"]), []
    checks = check_module(parsed["decls"])
    diags = list(checks)
    if not checks:
        if input_spec:
            name, _, file = input_spec.partition("=")
            r = load_modules(path)
            with open(file, encoding="utf-8") as f:
                raw = read_json(f.read())
            if r["entry"] is not None:
                diags += run_universe(r["modules"], r["entry"], [{"input": name, "raw": raw}])["diags"]
        else:
            diags += run_pipeline(parsed["decls"])
    return 0, diags


def check_files(paths: list[str]) -> list:
    """`decl check`: load each entry (following imports), report load
    diagnostics and every module's static findings, tagged with their file."""
    out: list = []
    for f in paths:
        r = load_modules(f)
        out += [dict(d, file=f) for d in r["diags"]]
        for m in r["modules"]:
            out += [dict(d, file=m.path) for d in check_module(m.decls, m.env)]
    return out


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
    checking-phase fixtures report their code (and message) too."""
    with open(file, encoding="utf-8") as f:
        src = f.read()
    meta = dict(m for m in re.findall(r"// @([a-z-]+):\s*(.+)", src))
    phase = meta.get("expect-phase")
    want = (meta.get("expect-error") or "").strip()
    want_msg = (meta.get("expect-message") or "").strip()
    parsed = parse_source(src)
    if is_valid:
        # a valid fixture must parse, check clean, AND evaluate its outputs
        # without error-severity diagnostics
        checks = check_module(parsed["decls"]) if not parsed["errors"] else []
        eval_errs = [d for d in run_pipeline(parsed["decls"]) if d["severity"] == "error"] \
            if (not parsed["errors"] and not checks) else []
        ok = not parsed["errors"] and not checks and not eval_errs
        detail = f"{len(parsed['errors'])} parse errors" if parsed["errors"] else json.dumps(checks + eval_errs, default=str)
        return {"file": file, "ok": ok, "detail": detail}
    if phase == "parsing":
        return {"file": file, "ok": bool(parsed["errors"]), "detail": "expected parse errors, got none"}
    if phase == "checking":
        checks = check_module(parsed["decls"]) if not parsed["errors"] else []
        ok = any(d.get("code") == want for d in checks) and (not want_msg or any(want_msg in d["message"] for d in checks))
        return {"file": file, "ok": ok, "detail": json.dumps(checks, default=str)}
    if phase == "binding":
        diags = run_pipeline(parsed["decls"]) if not parsed["errors"] else []
        ok = any(d.get("code") == want for d in diags) and (not want_msg or any(want_msg in d["message"] for d in diags))
        return {"file": file, "ok": ok, "detail": json.dumps(diags, default=str)}
    return {"file": file, "ok": False, "detail": f"unknown phase {phase}"}


def main(argv: list[str]) -> int:
    if not argv:
        print("usage: python -m decl.runtime check <file>... | evaluate <file> [--root name] [--json] | validate <dir|file> [--input n=f] [--expect-errors E1,E2] [--json]", file=sys.stderr)
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
    if cmd == "check":
        if not pos:
            print("usage: python -m decl.runtime check <file>... [--json]", file=sys.stderr)
            return 2
        diags = check_files(pos)
        for d in diags:
            _print_diag(d["file"], {k: v for k, v in d.items() if k != "file"}, collected, json_mode)
        if not diags:
            print(f"ok: {len(pos)} entry file(s) check clean", file=sys.stderr)
        if json_mode:
            print(json.dumps(collected, ensure_ascii=False, default=str))
        return 1 if diags else 0
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
            ok = fail = 0
            for dirpath, _, files in os.walk(target):
                for fn in sorted(files):
                    if not fn.endswith(".decl"):
                        continue
                    fp = os.path.join(dirpath, fn)
                    v = judge_fixture(fp, "/valid/" in fp)
                    if v["ok"]:
                        ok += 1
                    else:
                        fail += 1
                        print(f"FAIL {fp} {v['detail']}", file=sys.stderr)
            print(f"{ok} ok, {fail} failed", file=sys.stderr)
            return 1 if fail else 0
        parse_errors, diags = validate_file(target, flags.get("input"))
        if parse_errors:
            print(f"{target}: {parse_errors} parse error(s)", file=sys.stderr)
            return 1
        for d in diags:
            _print_diag(target, d, collected, json_mode)
        if json_mode:
            print(json.dumps(collected, ensure_ascii=False, default=str))
        err_codes = [d.get("code") or "" for d in diags if d["severity"] == "error"]
        expect = flags.get("expect-errors")
        if isinstance(expect, str):
            want = [w.strip() for w in expect.split(",") if w.strip()]
            missing = [w for w in want if w not in err_codes]
            extra = [c for c in err_codes if c not in want]
            if missing or extra:
                if missing:
                    print(f"expected error(s) not reported: {', '.join(missing)}", file=sys.stderr)
                if extra:
                    print(f"unexpected error(s): {', '.join(extra)}", file=sys.stderr)
                return 1
            print(f"ok: expected errors reported ({', '.join(want) or 'none'})", file=sys.stderr)
            return 0
        return 1 if err_codes else 0
    print(f"unknown command {cmd}", file=sys.stderr)
    return 2
