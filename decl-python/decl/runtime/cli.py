"""The `decl` command line: check / evaluate / validate / fmt.
Output formats match the reference `decl` CLI byte for byte so the
three implementations can be diffed (tests/parity/differential.py)."""
from __future__ import annotations

import json
import os
import sys

from .checker import check_module
from .conformance import judge_corpus
from .fmt import format_source
from .module import load_modules, run_universe
from .package import open_package_universe, verify_lock
from .parse import parse_source
from .pipeline import run_pipeline
from .semantics import read_json


def _print_diag(file: str, d: dict, collected, json_mode: bool) -> None:
    if json_mode:
        collected.append({"file": file, **d})
        return
    code = f" [{d['code']}]" if d.get("code") else ""
    id_ = f" {d['id']}" if d.get("id") else ""
    at = f" at {d['path']}" if d.get("path") else ""
    print(f"{file}: {d['severity']}{code}{id_}{at}: {d['message']}", file=sys.stderr)


def open_universe(file: str) -> dict:
    """The module graph of an entry file inside its package universe
    (manifest and lock diagnostics first), as the reference CLI opens it."""
    abs_ = os.path.abspath(file)
    pkg = open_package_universe(abs_)
    pre = (pkg["diags"] + verify_lock(pkg)) if pkg else []
    r = load_modules(abs_, None, pkg["resolver"] if pkg else None)
    return {"modules": r["modules"], "entry": r["entry"], "diags": pre + r["diags"]}


class CliError(Exception):
    """A fatal command-line error: the message goes to stderr as-is and the
    process exits with `status` (2 for a usage error, 1 for a missing root);
    `diagnostics` are the ones already produced before it."""

    def __init__(self, message: str, status: int, diagnostics: list | None = None):
        super().__init__(message)
        self.status = status
        self.diagnostics: list = list(diagnostics or [])


def input_binds(modules: list, specs: list[str]) -> list:
    """The documents named by --input, each bound to the module that declares
    its input (§10): `name=doc.json`."""
    binds: list = []
    for spec in specs:
        eq = spec.find("=")
        if eq < 0:
            raise CliError(f"--input expects name=doc.json, got {spec}", 2)
        name, file = spec[:eq], spec[eq + 1:]
        module = next((m for m in modules if name in m.env.inputs), None)
        if module is None:
            raise CliError(f"no input named {name}", 2)
        with open(file, encoding="utf-8") as f:
            binds.append({"module": module, "input": name, "raw": read_json(f.read())})
    return binds


def evaluate_file(path: str, root: str | None, input_specs: list[str] | None = None):
    """Returns (exit_code, value_text, diagnostics); raises CliError for a bad
    --input spec (status 2) or a --root that names no root (status 1)."""
    r = open_universe(path)
    if r["diags"] or r["entry"] is None:
        return 1, None, r["diags"]
    entry = r["entry"]
    checks = [dict(d, file=m.path) for m in r["modules"] for d in check_module(m.decls, m.env)]
    if checks:
        return 1, None, checks
    u = run_universe(r["modules"], entry, input_binds(r["modules"], input_specs or []))
    diags = list(u["diags"])
    errs = [d for d in diags if d["severity"] == "error"]
    if errs:
        return 1, None, diags
    eng = u["eng"]
    # every output, or the one root --root names (an output, or an input
    # bound by --input / demanded through its fallback)
    names = [root] if root is not None else [o["name"] for m in r["modules"] for o in m.env.outputs]
    missing = [n for n in names if n not in entry.env.roots]
    if missing:
        raise CliError("\n".join(f"no root named {n}" for n in missing), 1, diags)
    pieces = [f"{json.dumps(n, ensure_ascii=False)}:{eng.serialize(entry.env.roots[n], n)}" for n in names]
    if root is not None and len(names) == 1:
        return 0, eng.serialize(entry.env.roots[root], root), diags
    return 0, "{" + ",".join(pieces) + "}", diags


def validate_file(path: str, input_specs: list[str] | None = None):
    """Static checks, then evaluation (binding the --input documents, if any);
    returns (parse_error_count, diagnostics); raises CliError for a bad --input."""
    with open(path, encoding="utf-8") as f:
        src = f.read()
    parsed = parse_source(src)
    if parsed["errors"]:
        return len(parsed["errors"]), []
    checks = check_module(parsed["decls"])
    diags = list(checks)
    if not checks:
        if input_specs:
            r = open_universe(path)
            if r["entry"] is not None:
                diags += run_universe(r["modules"], r["entry"], input_binds(r["modules"], input_specs))["diags"]
        else:
            diags += run_pipeline(parsed["decls"])["diags"]
    return 0, diags


def check_files(paths: list[str]) -> list:
    """`decl check`: load each entry (following imports), report load
    diagnostics and every module's static findings, tagged with their file."""
    out: list = []
    for f in paths:
        r = open_universe(f)
        out += [dict(d, file=f) for d in r["diags"]]
        for m in r["modules"]:
            out += [dict(d, file=m.path) for d in check_module(m.decls, m.env)]
    return out


USAGE = """usage:
  decl check <files...>
  decl evaluate <file> [--input name=doc.json]... [--root <name>]
  decl validate <dir>
  decl validate <file> [--input name=doc.json]... [--expect-errors E1,E2]
  decl fmt <files...> [--check]
  (check / validate accept --json: diagnostics as a JSON array on stdout)"""


def usage() -> int:
    print(USAGE, file=sys.stderr)
    return 2


def main(argv: list[str]) -> int:
    if not argv:
        return usage()
    cmd, args = argv[0], argv[1:]
    flags: dict = {}
    input_flags: list = []   # --input name=doc.json, repeatable
    pos: list = []
    i = 0
    while i < len(args):
        a = args[i]
        if a.startswith("--"):
            name = a[2:]
            if name in ("root", "input", "expect-errors") and i + 1 < len(args) and not args[i + 1].startswith("--"):
                if name == "input":
                    input_flags.append(args[i + 1])
                else:
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
            return usage()
        diags = check_files(pos)
        for d in diags:
            _print_diag(d["file"], {k: v for k, v in d.items() if k != "file"}, collected, json_mode)
        if not diags:
            print(f"ok: {len(pos)} entry file(s) check clean", file=sys.stderr)
        if json_mode:
            print(json.dumps(collected, ensure_ascii=False, default=str))
        return 1 if diags else 0
    if cmd == "fmt":
        if not pos:
            return usage()
        changed = bad = 0
        for f in pos:
            with open(f, encoding="utf-8") as fh:
                src = fh.read()
            try:
                out = format_source(src)
            except ValueError as e:
                print(f"{f}: {e}", file=sys.stderr)
                bad += 1
                continue
            if out != src:
                changed += 1
                if flags.get("check"):
                    print(f"would reformat {f}", file=sys.stderr)
                else:
                    with open(f, "w", encoding="utf-8") as fh:
                        fh.write(out)
                    print(f"reformatted {f}", file=sys.stderr)
        return 1 if (bad or (flags.get("check") and changed)) else 0
    if cmd == "evaluate":
        if not pos:
            return usage()
        note = None   # a bare stderr line (a --root that names no root)
        try:
            code, text, diags = evaluate_file(pos[0], flags.get("root"), input_flags)
        except CliError as e:
            if e.status == 2:
                print(e, file=sys.stderr)
                return 2   # a usage error: no report
            code, text, diags, note = e.status, None, e.diagnostics, str(e)
        for d in diags:
            _print_diag(pos[0], d, collected, json_mode)
        if note is not None:
            print(note, file=sys.stderr)
        if json_mode:
            print(f'{{"ok":{"true" if code == 0 else "false"},"value":{text if text is not None else "null"},"diagnostics":{json.dumps(collected, ensure_ascii=False, default=str)}}}')
        elif text is not None:
            print(text)
        return code
    if cmd == "validate":
        if not pos:
            return usage()
        target = pos[0]
        if os.path.isdir(target):
            ok = fail = 0
            for v in judge_corpus(os.path.abspath(target)):
                if v["ok"]:
                    ok += 1
                else:
                    fail += 1
                    print(f"FAIL {v['file']} {v['detail']}", file=sys.stderr)
            print(f"{ok} ok, {fail} failed", file=sys.stderr)
            return 1 if fail else 0
        try:
            parse_errors, diags = validate_file(target, input_flags)
        except CliError as e:
            print(e, file=sys.stderr)
            return e.status
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
    return usage()
