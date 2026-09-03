"""End-to-end parity for the native Python runtime — the reference
implementation's decl-typescript/test/e2e.ts, packages.ts, fmt.ts, and lsp.ts
scenarios: benchmark round-trips byte-identical under renamed roots,
root-cause diagnostics on a corrupted document, layered configs, the
guide module, cross-package imports and the lock file, the canonical
formatter, and the language server over stdio."""
from __future__ import annotations

import re
import sys
from pathlib import Path

from decl.runtime.engine import Engine
from decl.runtime.module import sort_diags
from decl.runtime.parse import parse_source
from decl.runtime.semantics import ABSENT, ArrV, Env, JObj, Quantity, Scope, read_json

ROOT = Path(__file__).resolve().parents[2]
passed = failed = 0


def check(name: str, cond: bool, detail: str = "") -> None:
    global passed, failed
    if cond:
        passed += 1
        print(f"  ok   {name}")
    else:
        failed += 1
        print(f"  FAIL {name} {detail}")


def pipeline(src: str, binds: list | None = None):
    parsed = parse_source(src)
    assert not parsed["errors"], "parse errors"
    env = Env()
    env.load(parsed["decls"])
    eng = Engine(env)
    for name, raw in (binds or []):   # bound documents first (§9.2), then outputs
        eng.bind_root(name, raw, env.resolve(env.inputs[name]["type"]), Scope(None, {}, name), False)
    for o in env.outputs:
        eng.bind_root(o["name"], o["expr"], env.resolve(o["type"]), Scope(None, {}, o["name"]), True)
    for v in list(env.roots.values()):
        eng.force_all(v, False)
    eng.phase = 2
    i = 0
    while i < len(eng.deferred_slots):
        inst, n = eng.deferred_slots[i]
        eng.force_slot_safe(inst, n)
        i += 1
    for v in list(env.roots.values()):
        eng.force_all(v, True)
    eng.validate_all("")
    env.diagnostics[:] = sort_diags(env.diagnostics)
    return env, eng


def get(eng: Engine, segs: list):
    return eng.resolve_segs(segs)


print("== case 3: fixtures ==")
env, eng = pipeline((ROOT / "docs/examples/03_fixtures.decl").read_text())
check("no diagnostics", not env.diagnostics, str(env.diagnostics[:2]))
check("32 cases", isinstance(env.roots.get("sweep"), ArrV) and len(env.roots["sweep"].items) == 32)
check("16 packets each", get(eng, ["sweep", 0, "packet_count"]) == 16)
check("total_bytes", get(eng, ["sweep", 0, "total_bytes"]) == 1024)
check("derived label", get(eng, ["sweep", 0, "packets", 3, "label"]) == "pkt-3-p0")
check("repeated serialization byte-identical", eng.serialize(env.roots["sweep"], "sweep") == eng.serialize(env.roots["sweep"], "sweep"))

print("== case 2: config ==")
cfg_src = (ROOT / "docs/examples/02_config.decl").read_text()
env, eng = pipeline(cfg_src)
check("outputs clean", not env.diagnostics, str(env.diagnostics[:2]))
check("prod layering", get(eng, ["prod", "host"]) == "api.internal" and get(eng, ["prod", "workers"]) == 32)
check("derived recomputed after with", get(eng, ["prod", "insecure"]) is False)
env2, eng2 = pipeline(cfg_src, [("deployed", read_json('{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}'))])
errs = [d for d in env2.diagnostics if d["severity"] == "error"]
warns = [d for d in env2.diagnostics if d["severity"] == "warn"]
check("two errors, one warning", len(errs) == 2 and len(warns) == 1, str(env2.diagnostics))
check("cert assert id", any(d.get("id") == "TlsConfig.cert_present" for d in errs))
check("warned value preserved", get(eng2, ["deployed", "workers"]) == 100)

print("== case 1: interconnect ==")
ic_src = (ROOT / "docs/examples/01_interconnect.decl").read_text()
env, eng = pipeline(ic_src)
check("xbar clean", not env.diagnostics, str(env.diagnostics[:2]))
check("propagated master si", get(eng, ["xbar", "nodes", "dom0", "nodes", "mst0", "ports", "si", "width"]) == 64)
check("arbiter max", get(eng, ["xbar", "nodes", "dom0", "nodes", "arb0", "ports", "mi", "width"]) == 64)
ser = eng.serialize(env.roots["xbar"], "xbar")
check("relative ref paths", '"$.nodes[\\"dom0\\"].ports[\\"si0\\"]"' in ser)
env2, eng2 = pipeline(ic_src, [("doc", read_json(ser))])
check("round-trip validates", not env2.diagnostics, str(env2.diagnostics[:2]))
check("round-trip byte-identical", eng2.serialize(env2.roots["doc"], "doc") == ser)
doc = read_json(ser)
ports = next(v for k, v in doc.entries if k == "ports")
mi1 = next(v for k, v in ports.entries if k == "mi1")
mi1.entries = [(k, 32 if k == "width" else v) for k, v in mi1.entries]
env3, _ = pipeline(ic_src, [("doc", doc)])
errs = [d for d in env3.diagnostics if d["severity"] == "error"]
check("one width_match at root cause", len(errs) == 1 and errs[0].get("id") == "Edge.width_match", str(errs))

print("== guide: end to end ==")
md = (ROOT / "docs/guide/01_overview_by_example.md").read_text()
guide_src = "\n".join(m.group(1) for m in re.finditer(r"```decl\n([\s\S]*?)```", md))
env, eng = pipeline(guide_src)
check("guide evaluates clean", not env.diagnostics, str(env.diagnostics[:3]))
check("guide endpoint derived", get(eng, ["demo", "services", 0, "endpoint"]) == "svc-0:9000")
check("guide defaults filled", get(eng, ["demo", "services", 0, "replicas"]) == 1)
check("guide quantity default", isinstance(get(eng, ["demo", "services", 0, "timeout"]), Quantity)
      and get(eng, ["demo", "services", 0, "timeout"]).value == 0.25)
check("guide service_count", get(eng, ["demo", "service_count"]) == 3)
inbound = get(eng, ["demo", "services", 1, "inbound"])
check("guide inbound via referrers", isinstance(inbound, ArrV) and len(inbound.items) == 2)
check("guide serialized refs relative", '"$.links[0]"' in eng.serialize(env.roots["demo"], "demo"))

import json
import os
import subprocess
import tempfile
from decl.runtime.checker import check_module
from decl.runtime.fmt import format_source
from decl.runtime.module import load_modules, run_universe
from decl.runtime.package import lock_text, open_package_universe, verify_lock, write_lock

print("== cross-package imports under exact pins ==")
entry = str(ROOT / "tests/packages/app/main.decl")
u = open_package_universe(entry)
check("universe opens clean", not u["diags"], json.dumps(u["diags"]))
check("closed set resolved", len(u["packages"]) == 1 and u["packages"]["corelib"]["version"] == "1.0.0")
r = load_modules(entry, None, u["resolver"])
check("modules load across packages", len(r["modules"]) == 2 and not r["diags"], json.dumps(r["diags"]))
checks = [d for m in r["modules"] for d in check_module(m.decls, m.env)]
check("modules check clean", not checks, json.dumps(checks[:3]))
uni = run_universe(r["modules"], r["entry"])
check("evaluates clean", not [d for d in uni["diags"] if d["severity"] == "error"], json.dumps(uni["diags"][:2]))
check("imported const and defaults", r["entry"].env.roots.get("w") == 16 and uni["eng"].resolve_segs(["box", "width"]) == 8)

print("== lock file: reproducibility, fail-closed drift ==")
lock_path = ROOT / "tests/packages/app/decl.lock"
mod_path = ROOT / "tests/packages/app/decl_modules/corelib/types/base.decl"
try:
    u1 = open_package_universe(entry)
    write_lock(u1)
    check("fresh lock verifies clean", not verify_lock(u1))
    u2 = open_package_universe(entry)
    check("lock text is reproducible", lock_text(u1) == lock_text(u2) and "corelib 1.0.0 " in lock_text(u1))
    original = mod_path.read_text()
    mod_path.write_text(original + "// drift\n")
    try:
        u3 = open_package_universe(entry)
        check("content drift is E3017", any(d["code"] == "E3017" for d in verify_lock(u3)), json.dumps(verify_lock(u3)))
    finally:
        mod_path.write_text(original)
    lock_path.write_text(lock_text(u1).replace("1.0.0", "1.0.1"))
    check("version drift is E3016", any(d["code"] == "E3016" for d in verify_lock(open_package_universe(entry))))
    lock_path.write_text("")
    check("missing entry is E3015", any(d["code"] == "E3015" for d in verify_lock(open_package_universe(entry))))
finally:
    if lock_path.exists():
        lock_path.unlink()

print("== manifest and resolution errors ==")
bad = open_package_universe(str(ROOT / "tests/packages/bad_manifest/main.decl"))
check("unknown field is E3011", any(d["code"] == "E3011" for d in bad["diags"]), json.dumps(bad["diags"]))
check("range pin is E3012", any(d["code"] == "E3012" for d in bad["diags"]))
und = open_package_universe(str(ROOT / "tests/packages/undeclared/main.decl"))
diags = load_modules(str(ROOT / "tests/packages/undeclared/main.decl"), None, und["resolver"])["diags"]
check("undeclared dependency is E3010", any(d["code"] == "E3010" for d in diags), json.dumps(diags))
con = open_package_universe(str(ROOT / "tests/packages/conflict/main.decl"))
check("conflicting versions is E3014", any(d["code"] == "E3014" for d in con["diags"]), json.dumps(con["diags"]))

print("== canonical-form spot checks ==")
for name, inp, want in [
    ("spacing", "const x=1+2*3\n", "const x = 1 + 2 * 3\n"),
    ("range stays tight", "type P=1..65535\n", "type P = 1..65535\n"),
    ("generic angles attach", "type V = Vec<int ,4>\n", "type V = Vec<int, 4>\n"),
    ("call parens attach", "const n = std.array.count(xs )\n", "const n = std.array.count(xs)\n"),
    ("record braces breathe", "type T = {a: int,b?: string}\n", "type T = { a: int, b?: string }\n"),
    ("indent rederived", "type T = {\n        a: int\n  b: string\n}\n", "type T = {\n    a: int\n    b: string\n}\n"),
    ("unary minus attaches", "const y = -x + 1\n", "const y = -x + 1\n"),
    ("blank lines collapse", "const a = 1\n\n\n\nconst b = 2\n", "const a = 1\n\nconst b = 2\n"),
    ("continuation hangs", "type T = {\n    assert a: x > 0\nelse warn `bad`\n}\n", "type T = {\n    assert a: x > 0\n        else warn `bad`\n}\n"),
    ("lambda spacing", "const f = std.array.all(xs,(x)=>x>0)\n", "const f = std.array.all(xs, (x) => x > 0)\n"),
    ("array suffix after a record attaches", "input s: {a: int, ...}[]\n", "input s: { a: int, ... }[]\n"),
    ("func body hangs after =", "func f(n: int): int =\nn + 1\n", "func f(n: int): int =\n    n + 1\n"),
    ("lambda body hangs after =>", "const xs = std.array.filter(ys, (y) =>\ny > 0)\n", "const xs = std.array.filter(ys, (y) =>\n        y > 0)\n"),
    ("operator at line end continues", "const s = a +\nb\n", "const s = a +\n    b\n"),
    ("a closing type angle does not continue", "type P = {\n    $parent: ref<{ a: int, ... }>\n    b: int\n}\n", "type P = {\n    $parent: ref<{ a: int, ... }>\n    b: int\n}\n"),
]:
    try:
        got = format_source(inp)
    except ValueError as e:
        got = f"THROW {e}"
    check(name, got == want, json.dumps({"got": got, "want": want}))

print("== idempotency + safety over the corpus ==")
files = []
for d in ("tests/validation", "tests/modules", "tests/packages", "docs/examples"):
    files += sorted((ROOT / d).rglob("*.decl"))
idem = token_safe = skipped = idem_fail = token_fail = 0
tokens = lambda src: json.dumps(parse_source(src)["decls"], default=str)
for f in files:
    src = f.read_text()
    if parse_source(src)["errors"]:
        skipped += 1
        continue
    try:
        once = format_source(src)
    except ValueError:
        skipped += 1
        continue
    try:
        twice = format_source(once)
    except ValueError as e:
        idem_fail += 1
        print(f"  SECOND PASS FAILS {f.relative_to(ROOT)}: {e}")
        continue
    if once == twice:
        idem += 1
    else:
        idem_fail += 1
        print(f"  NOT IDEMPOTENT {f.relative_to(ROOT)}")
    if not parse_source(once)["errors"] and tokens(once) == tokens(src):
        token_safe += 1
    else:
        token_fail += 1
        print(f"  AST CHANGED {f.relative_to(ROOT)}")
check(f"fmt(fmt(x)) == fmt(x) on {idem + idem_fail} parseable files", idem_fail == 0, f"{idem_fail} failures")
check("formatting preserves the AST on all files", token_fail == 0, f"{token_fail} failures")
print(f"  ({skipped} unparseable fixtures skipped by design)")

print("== command line: evaluate --input binds a document, --root names any root ==")
cli_tmp = tempfile.mkdtemp(prefix="decl-cli-")
fixture = str(ROOT / "tests/validation/declarations/valid/output_from_input_fallback.decl")
doc_path = os.path.join(cli_tmp, "base.json")
open(doc_path, "w").write('{"host": "h", "port": 8}')


def run_cli(*args: str) -> tuple:
    r = subprocess.run([sys.executable, "-m", "decl.runtime", "evaluate", *args],
                       capture_output=True, text=True, cwd=str(ROOT / "decl-python"))
    return r.returncode, r.stdout.strip(), r.stderr.strip()


code, out, err = run_cli(fixture, "--input", f"base={doc_path}", "--root", "base")
check("bound input emitted as the named root", code == 0 and out == '{"host":"h","port":8}', f"{code} {out} {err}")
code, out, err = run_cli(fixture, "--input", f"base={doc_path}", "--root", "copy")
check("output reads the bound document", code == 0 and out == '{"host":"h","port":8}', f"{code} {out} {err}")
code, out, err = run_cli(fixture, "--root", "base")
check("fallback-demanded input is a root", code == 0 and out == '{"host":"example","port":80}', f"{code} {out} {err}")
code, out, err = run_cli(fixture, "--root", "nope")
check("--root naming no root exits 1", code == 1 and err == "no root named nope", f"{code} {out} {err}")
code, out, err = run_cli(fixture, "--input", "nope=x.json")
check("--input naming no input is a usage error", code == 2 and err == "no input named nope", f"{code} {out} {err}")

print("== language server over stdio ==")
server = subprocess.Popen([sys.executable, "-m", "decl.runtime.lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr, cwd=str(ROOT / "decl-python"))
_next_id = [0]
_notifications: list = []


def _send(msg: dict) -> None:
    body = json.dumps(msg).encode("utf-8")
    server.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    server.stdin.flush()


def _recv() -> dict:
    header = b""
    while not header.endswith(b"\r\n\r\n"):
        ch = server.stdout.read(1)
        if not ch:
            raise RuntimeError("server closed")
        header += ch
    length = int(re.search(rb"Content-Length: (\d+)", header).group(1))
    return json.loads(server.stdout.read(length).decode("utf-8"))


def request(method: str, params: dict):
    _next_id[0] += 1
    _send({"jsonrpc": "2.0", "id": _next_id[0], "method": method, "params": params})
    while True:
        m = _recv()
        if m.get("id") == _next_id[0]:
            return m.get("result")
        _notifications.append(m)


def notify_server(method: str, params: dict) -> None:
    _send({"jsonrpc": "2.0", "method": method, "params": params})


def next_diagnostics(uri: str) -> dict:
    while True:
        m = _recv()
        if m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == uri:
            return m["params"]
        _notifications.append(m)


tmpd = tempfile.mkdtemp(prefix="decl-lsp-")
lib_path = os.path.join(tmpd, "lib.decl")
open(lib_path, "w").write("export type Service = { name: string, port: 1..65535 = 8080 }\nexport const MAX = 16\n")
main_path = os.path.join(tmpd, "main.decl")
main_uri = Path(main_path).as_uri()
open(main_path, "w").write("")
init = request("initialize", {"processId": None, "rootUri": None, "capabilities": {}})
check("initialize advertises capabilities", init["capabilities"]["hoverProvider"] is True and init["capabilities"]["definitionProvider"] is True)
notify_server("initialized", {})
notify_server("textDocument/didOpen", {"textDocument": {"uri": main_uri, "languageId": "decl", "version": 1, "text": "const x = \n"}})
d = next_diagnostics(main_uri)
check("syntax error published", len(d["diagnostics"]) > 0 and d["diagnostics"][0]["message"] == "syntax error", json.dumps(d))
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 2}, "contentChanges": [{"text": "type Bad = 10..3\n"}]})
d = next_diagnostics(main_uri)
check("checker diagnostic published with code", any(x.get("code") == "E4011" for x in d["diagnostics"]), json.dumps(d))
check("diagnostic anchored to the name", d["diagnostics"][0]["range"]["start"]["line"] == 0 and d["diagnostics"][0]["range"]["start"]["character"] > 0, json.dumps(d["diagnostics"][0]["range"]))
main_src = 'import { Service, MAX as LIMIT } from "./lib.decl"\nconst top = LIMIT\nexport output s: Service = { name: "a" }\n'
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 3}, "contentChanges": [{"text": main_src}]})
d = next_diagnostics(main_uri)
check("clean module publishes no diagnostics", not d["diagnostics"], json.dumps(d))
h = request("textDocument/hover", {"textDocument": {"uri": main_uri}, "position": {"line": 1, "character": 7}})
check("hover shows the declaration", bool(h) and "const top = LIMIT" in h["contents"]["value"], json.dumps(h))
h2 = request("textDocument/hover", {"textDocument": {"uri": main_uri}, "position": {"line": 1, "character": 13}})
check("hover follows a renamed import", bool(h2) and "MAX = 16" in h2["contents"]["value"], json.dumps(h2))
col = main_src.split("\n")[2].index("Service") + 2
dfn = request("textDocument/definition", {"textDocument": {"uri": main_uri}, "position": {"line": 2, "character": col}})
check("definition jumps across the import", bool(dfn) and dfn["uri"].endswith("lib.decl") and dfn["range"]["start"]["line"] == 0, json.dumps(dfn))
request("shutdown", {})
notify_server("exit", {})
server.stdin.close()
server.wait(timeout=10)

print(f"\nTOTAL {passed} ok, {failed} failed")
sys.exit(1 if failed else 0)
