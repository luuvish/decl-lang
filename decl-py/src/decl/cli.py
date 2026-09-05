"""The `decl` command line: check / evaluate / validate / fmt.
Output formats match the reference `decl` CLI byte for byte so the
three implementations can be diffed (tests/parity/differential.py)."""

from __future__ import annotations

import json
import os
import sys
from typing import Any

from .checker import check_module
from .conformance import judge_corpus
from .fmt import format_source
from .module import load_modules, run_universe
from .package import open_package_universe, verify_lock
from .semantics import read_json


def _diag_json(file: str, d: dict[str, Any]) -> dict[str, Any]:
    """one diagnostic in the report's field order (§12.2): file, code, id,
    severity, message, path — absent fields omitted, so every implementation
    emits the same bytes"""
    o: dict[str, Any] = {"file": file}
    if d.get("code"):
        o["code"] = d["code"]
    if d.get("id"):
        o["id"] = d["id"]
    o["severity"] = d["severity"]
    o["message"] = d["message"]
    o["path"] = d.get("path", "")
    return o


def _dumps(v: Any) -> str:
    return json.dumps(v, ensure_ascii=False, separators=(",", ":"), default=str)


def _print_diag(file: str, d: dict[str, Any], collected: Any, json_mode: bool) -> None:
    if json_mode:
        collected.append(_diag_json(file, d))
        return
    code = f" [{d['code']}]" if d.get("code") else ""
    id_ = f" {d['id']}" if d.get("id") else ""
    at = f" at {d['path']}" if d.get("path") else ""
    print(f"{file}: {d['severity']}{code}{id_}{at}: {d['message']}", file=sys.stderr)


def open_universe(file: str) -> dict[str, Any]:
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

    def __init__(self, message: str, status: int, diagnostics: list[Any] | None = None):
        super().__init__(message)
        self.status = status
        self.diagnostics: list[Any] = list(diagnostics or [])


def input_binds(modules: list[Any], specs: list[str]) -> list[Any]:
    """The documents named by --input, each bound to the module that declares
    its input (§10): `name=doc.json`. A usage error (bad spec, unknown input)
    is a CliError with status 2; a document that cannot be read or is not
    well-formed JSON is a CliError carrying one E6004 diagnostic (status 1)."""

    def doc_error(name: str, message: str) -> CliError:
        return CliError(
            "", 1, [{"severity": "error", "code": "E6004", "message": message, "path": name}]
        )

    binds: list[Any] = []
    for spec in specs:
        eq = spec.find("=")
        if eq < 0:
            raise CliError(f"--input expects name=doc.json, got {spec}", 2)
        name, file = spec[:eq], spec[eq + 1 :]
        module = next((m for m in modules if name in m.env.inputs), None)
        if module is None:
            raise CliError(f"no input named {name}", 2)
        try:
            with open(file, encoding="utf-8") as f:
                text = f.read()
        except OSError:
            raise doc_error(name, f"bound document cannot be read: {file}") from None
        try:
            raw = read_json(text)
        except Exception:
            raise doc_error(name, f"bound document is not well-formed JSON: {file}") from None
        binds.append({"module": module, "input": name, "raw": raw})
    return binds


def evaluate_file(
    path: str, outputs: list[str] | None, input_specs: list[str] | None = None
) -> Any:
    """Returns (exit_code, document_for_stdout, diagnostics, notes) — notes are
    bare stderr lines printed after the diagnostics; raises CliError for a bad
    --input or --output spec (status 2). What to emit, and where (§5.5): each
    `--output name[=file]` names a root — an output, or an input bound by
    --input or demanded through its fallback — and the file its document goes
    to (stdout without one); with no --output, the entry module's exported
    outputs, as one object keyed by name, on stdout."""
    targets: list[tuple[str, str | None]] = []
    for spec in outputs or []:
        name, _, dest = spec.partition("=")
        file = dest if "=" in spec else None
        if not name or file == "":
            raise CliError(f"--output expects name or name=file, got {spec}", 2)
        targets.append((name, file))
    if sum(1 for _, f in targets if f is None) > 1:
        raise CliError("--output: at most one document can go to stdout", 2)
    r = open_universe(path)
    if r["diags"] or r["entry"] is None:
        return 1, None, r["diags"], []
    entry = r["entry"]
    checks = [
        dict(d, file=_file_tag(path, entry, m.path))
        for m in r["modules"]
        for d in check_module(m.decls, m.env)
    ]
    if checks:
        return 1, None, checks, []
    u = run_universe(r["modules"], entry, input_binds(r["modules"], input_specs or []))
    diags = list(u["diags"])
    errs = [d for d in diags if d["severity"] == "error"]
    if errs:
        return 1, None, diags, []
    eng = u["eng"]
    names = (
        [n for n, _ in targets]
        if targets
        else [o["name"] for o in entry.env.outputs if o.get("exported")]
    )
    notes = [f"no root named {n}" for n in names if n not in entry.env.roots]
    if notes:
        return 1, None, diags, notes

    def doc(n: str) -> str:
        return eng.serialize(entry.env.roots[n], n)

    text = None
    if not targets:
        if not names:
            notes.append(f"{path}: exports no output; --output <name> selects a root")
        text = "{" + ",".join(f"{json.dumps(n, ensure_ascii=False)}:{doc(n)}" for n in names) + "}"
    else:
        for n, file in targets:
            if file is None:
                text = doc(n)
                continue
            try:
                with open(file, "w", encoding="utf-8") as fh:
                    fh.write(doc(n) + "\n")
            except OSError:
                notes.append(f"cannot write {file}")
                return 1, None, diags, notes
    return 0, text, diags, notes


def validate_file(path: str, input_specs: list[str] | None = None) -> Any:
    """Single-file validation, module-aware like `check` and `evaluate`: load
    the universe, check every module, then evaluate with the --input documents
    bound (none bound is fine: fallbacks apply). Returns diagnostics, each
    carrying the file it is reported against; raises CliError for a bad --input."""
    r = open_universe(path)
    diags = [dict(d, file=path) for d in r["diags"]]
    entry = r["entry"]
    if diags or entry is None:
        return diags
    checks = [
        dict(d, file=_file_tag(path, entry, m.path))
        for m in r["modules"]
        for d in check_module(m.decls, m.env)
    ]
    if checks:
        return checks
    u = run_universe(r["modules"], entry, input_binds(r["modules"], input_specs or []))
    return [dict(d, file=path) for d in u["diags"]]


def _file_tag(given: str, entry: Any, module_path: str) -> str:
    """the file a diagnostic is reported against: the entry module by the
    path given on the command line, any other module by its absolute path"""
    return given if entry is not None and module_path == entry.path else module_path


def check_files(paths: list[str]) -> list[Any]:
    """`decl check`: load each entry (following imports), report load
    diagnostics and every module's static findings, tagged with their file."""
    out: list[Any] = []
    for f in paths:
        r = open_universe(f)
        out += [dict(d, file=f) for d in r["diags"]]
        for m in r["modules"]:
            out += [
                dict(d, file=_file_tag(f, r["entry"], m.path)) for d in check_module(m.decls, m.env)
            ]
    return out


USAGE = """usage:
  decl --version
  decl check <files...>
  decl evaluate <file> [--input name=doc.json]... [--output name[=file]]...
  decl validate <dir>
  decl validate <file> [--input name=doc.json]... [--expect-errors E1,E2]
  decl fmt <files...> [--check]
  decl repl [file.decl] [--input name=doc.json]... [--script session.txt | --script -] [--compact]
  (check / validate accept --json: diagnostics as a JSON array on stdout)"""


def usage() -> int:
    print(USAGE, file=sys.stderr)
    return 2


def main(argv: list[str] | None = None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    if not argv:
        return usage()
    cmd, args = argv[0], argv[1:]
    # `decl --version`: the package's version, the same string on every registry
    if cmd == "--version":
        from .api import __version__

        print(f"decl {__version__}")
        return 0
    # `decl repl`: its own argument syntax (docs/tooling/02_repl.md)
    if cmd == "repl":
        from .repl import run_repl

        return run_repl(args)
    flags: dict[str, Any] = {}
    input_flags: list[Any] = []  # --input name=doc.json, repeatable
    output_flags: list[Any] = []  # --output name[=file], repeatable
    pos: list[Any] = []
    i = 0
    while i < len(args):
        a = args[i]
        if a.startswith("--"):
            name = a[2:]
            if (
                name in ("output", "input", "expect-errors")
                and i + 1 < len(args)
                and not args[i + 1].startswith("--")
            ):
                if name == "input":
                    input_flags.append(args[i + 1])
                elif name == "output":
                    output_flags.append(args[i + 1])
                else:
                    flags[name] = args[i + 1]
                i += 2
                continue
            flags[name] = True
        else:
            pos.append(a)
        i += 1
    json_mode = bool(flags.get("json"))
    collected: list[Any] = []
    if cmd == "check":
        if not pos:
            return usage()
        diags = check_files(pos)
        for d in diags:
            _print_diag(
                d["file"], {k: v for k, v in d.items() if k != "file"}, collected, json_mode
            )
        if not diags:
            print(f"ok: {len(pos)} entry file(s) check clean", file=sys.stderr)
        if json_mode:
            print(_dumps(collected))
        return 1 if diags else 0
    if cmd == "fmt":
        if not pos:
            return usage()
        changed = bad = 0
        for f in pos:
            try:
                with open(f, encoding="utf-8") as fh:
                    src = fh.read()
            except OSError:
                print(f"{f}: cannot be read", file=sys.stderr)
                bad += 1
                continue
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
        try:
            code, text, diags, notes = evaluate_file(pos[0], output_flags, input_flags)
        except CliError as e:
            if e.status == 2:
                print(e, file=sys.stderr)
                return 2  # a usage error: no report
            code, text, diags, notes = e.status, None, e.diagnostics, [str(e)] if str(e) else []
        for d in diags:
            _print_diag(
                d.get("file", pos[0]),
                {k: v for k, v in d.items() if k != "file"},
                collected,
                json_mode,
            )
        for n in notes:
            print(n, file=sys.stderr)
        if json_mode:
            print(
                f'{{"ok":{"true" if code == 0 else "false"},'
                f'"value":{text if text is not None else "null"},'
                f'"diagnostics":{_dumps(collected)}}}'
            )
        elif text is not None:
            print(text)
        return code
    if cmd == "validate":
        if not pos:
            return usage()
        target = pos[0]
        if flags.get("expect-errors") is True:
            print("--expect-errors expects a list of codes: E1,E2", file=sys.stderr)
            return 2
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
            diags = validate_file(target, input_flags)
        except CliError as e:
            if e.status == 2:
                print(e, file=sys.stderr)
                return 2  # a usage error: no report
            diags = [dict(d, file=target) for d in e.diagnostics]
        for d in diags:
            _print_diag(
                d.get("file", target),
                {k: v for k, v in d.items() if k != "file"},
                collected,
                json_mode,
            )
        if json_mode:
            print(_dumps(collected))
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
