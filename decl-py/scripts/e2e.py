"""End-to-end parity for the native Python runtime — the reference
implementation's decl-ts/test/e2e.ts, packages.ts, fmt.ts, and lsp.ts
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
# the AST without its source ranges: formatting moves columns, never nodes
def _without_loc(x):
    if isinstance(x, dict):
        return {k: _without_loc(v) for k, v in x.items() if k != "loc"}
    if isinstance(x, list):
        return [_without_loc(v) for v in x]
    return x


tokens = lambda src: json.dumps(_without_loc(parse_source(src)["decls"]), default=str)
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

print("== command line: evaluate --input binds a document, --output names any root ==")
cli_tmp = tempfile.mkdtemp(prefix="decl-cli-")
fixture = str(ROOT / "tests/validation/declarations/valid/output_from_input_fallback.decl")
doc_path = os.path.join(cli_tmp, "base.json")
open(doc_path, "w").write('{"host": "h", "port": 8}')


def run_cli(*args: str) -> tuple:
    r = subprocess.run([sys.executable, "-m", "decl.runtime", "evaluate", *args],
                       capture_output=True, text=True, cwd=str(ROOT / "decl-py"))
    return r.returncode, r.stdout.strip(), r.stderr.strip()


code, out, err = run_cli(fixture, "--input", f"base={doc_path}", "--output", "base")
check("bound input emitted as the named root", code == 0 and out == '{"host":"h","port":8}', f"{code} {out} {err}")
code, out, err = run_cli(fixture, "--input", f"base={doc_path}", "--output", "copy")
check("output reads the bound document", code == 0 and out == '{"host":"h","port":8}', f"{code} {out} {err}")
code, out, err = run_cli(fixture, "--output", "base")
check("fallback-demanded input is a root", code == 0 and out == '{"host":"example","port":80}', f"{code} {out} {err}")
code, out, err = run_cli(fixture, "--output", "nope")
check("--output naming no root exits 1", code == 1 and err == "no root named nope", f"{code} {out} {err}")
code, out, err = run_cli(fixture)
check("a module exporting no output says so", code == 0 and out == "{}" and err.endswith("exports no output; --output <name> selects a root"), f"{code} {out} {err}")
code, out, err = run_cli(fixture, "--input", "nope=x.json")
check("--input naming no input is a usage error", code == 2 and err == "no input named nope", f"{code} {out} {err}")

print("== language server over stdio ==")
server = subprocess.Popen([sys.executable, "-m", "decl.runtime.lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr, cwd=str(ROOT / "decl-py"))
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
open(lib_path, "w").write("export type Service = { name: string, port?: 1..65535 = 8080 }\nexport const MAX = 16\nexport func cap(n: int): int = std.math.min(n, MAX)\nexport type Public = Service { public: bool }\n")
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
main_src = 'import { Service, MAX as LIMIT, cap } from "./lib.decl"\nconst top = LIMIT\nexport output s: Service = { name: "a" }\nexport output t: Service = {\n    name: "b"\n}\nconst first = s.name\nconst c = cap(top)\nconst d = 250ms\ntype Local = Service { extra = name }\n'
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
td = request("textDocument/typeDefinition", {"textDocument": {"uri": main_uri}, "position": {"line": 6, "character": 14}})
check("type definition of a value of type Service", bool(td) and td["uri"].endswith("lib.decl") and td["range"]["start"]["line"] == 0, json.dumps(td))
refs = request("textDocument/references", {"textDocument": {"uri": main_uri}, "position": {"line": 2, "character": 19}, "context": {"includeDeclaration": True}})
check("references of Service: declaration, import item, annotations, extensions", len(refs) == 6 and sum(1 for r in refs if r["uri"].endswith("lib.decl")) == 2, json.dumps(refs))
hl = request("textDocument/documentHighlight", {"textDocument": {"uri": main_uri}, "position": {"line": 6, "character": 14}})
check("highlight of s: its declaration and its use", len(hl) == 2 and hl[0]["range"]["start"]["line"] == 2 and hl[1]["range"]["start"]["line"] == 6, json.dumps(hl))
c1 = request("textDocument/completion", {"textDocument": {"uri": main_uri}, "position": {"line": 1, "character": 15}})
check("completion of a name prefix", any(i["label"] == "LIMIT" for i in c1["items"]), json.dumps(c1))
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 20}, "contentChanges": [{"text": main_src + "const e = s.\n"}]})
next_diagnostics(main_uri)
c2 = request("textDocument/completion", {"textDocument": {"uri": main_uri}, "position": {"line": 10, "character": 12}})
check("member completion while the text does not parse", ",".join(i["label"] for i in c2["items"]) == "name,port", json.dumps(c2))
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 21}, "contentChanges": [{"text": main_src}]})
next_diagnostics(main_uri)
syms = request("textDocument/documentSymbol", {"textDocument": {"uri": main_uri}})
check("document symbols", ",".join(x["name"] for x in syms) == "top,s,t,first,c,d,Local", json.dumps(syms))
folds = request("textDocument/foldingRange", {"textDocument": {"uri": main_uri}})
check("folding of the multi-line output", len(folds) == 1 and folds[0]["startLine"] == 3 and folds[0]["endLine"] == 5, json.dumps(folds))
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 22}, "contentChanges": [{"text": "const x=1\n"}]})
next_diagnostics(main_uri)
fmt = request("textDocument/formatting", {"textDocument": {"uri": main_uri}, "options": {"tabSize": 4, "insertSpaces": True}})
check("formatting replaces the document with its canonical form", len(fmt) == 1 and fmt[0]["newText"] == "const x = 1\n", json.dumps(fmt))
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 23}, "contentChanges": [{"text": main_src}]})
next_diagnostics(main_uri)
pr = request("textDocument/prepareRename", {"textDocument": {"uri": main_uri}, "position": {"line": 1, "character": 7}})
check("prepare rename gives the name range", bool(pr) and pr["placeholder"] == "top" and pr["range"]["start"]["character"] == 6, json.dumps(pr))
rn = request("textDocument/rename", {"textDocument": {"uri": main_uri}, "position": {"line": 2, "character": 19}, "newName": "Svc"})
check("rename edits every module", bool(rn) and len(rn["changes"]) == 2 and len(rn["changes"][main_uri]) == 4, json.dumps(rn))
lenses = request("textDocument/codeLens", {"textDocument": {"uri": main_uri}})
check("lenses on the outputs", len(lenses) == 2 and lenses[0]["command"]["command"] == "decl.evaluate", json.dumps(lenses))
ev = request("workspace/executeCommand", {"command": "decl.evaluate", "arguments": [main_uri, "s"]})
check("decl.evaluate returns the document", bool(ev) and ev["document"] == '{"name":"a","port":8080}' and not ev["diagnostics"], json.dumps(ev))
va = request("workspace/executeCommand", {"command": "decl.validate", "arguments": [main_uri, "s"]})
check("decl.validate returns the verdict", bool(va) and len(va["verdicts"]) == 1 and va["verdicts"][0]["errors"] == 0, json.dumps(va))
sh = request("textDocument/signatureHelp", {"textDocument": {"uri": main_uri}, "position": {"line": 7, "character": 15}})
check("signature help of a function call", bool(sh) and sh["signatures"][0]["label"] == "cap(n: int): int" and sh["activeParameter"] == 0, json.dumps(sh))
ws = request("workspace/symbol", {"query": "ca"})
check("workspace symbols across the universe", any(x["name"] == "cap" and x["location"]["uri"].endswith("lib.decl") for x in ws), json.dumps(ws))
sr = request("textDocument/selectionRange", {"textDocument": {"uri": main_uri}, "positions": [{"line": 6, "character": 16}]})
check("selection ranges grow outward", sr[0]["range"]["start"]["character"] == 14 and sr[0]["parent"]["range"]["start"]["character"] == 0, json.dumps(sr))
st = request("textDocument/semanticTokens/full", {"textDocument": {"uri": main_uri}})
check("semantic tokens are encoded in fives", len(st["data"]) > 0 and len(st["data"]) % 5 == 0, json.dumps(st))
ih = request("textDocument/inlayHint", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 20, "character": 0}}})
check("inlay hints: parameter name, unit base value, derived type", any(h["label"] == "n:" for h in ih) and any(h["label"] == "= 0.25 s" for h in ih) and any(h["label"] == ": string" for h in ih), json.dumps(ih))
ch = request("textDocument/prepareCallHierarchy", {"textDocument": {"uri": main_uri}, "position": {"line": 7, "character": 11}})
inc = request("callHierarchy/incomingCalls", {"item": ch[0]})
check("call hierarchy: cap is called from c", ch[0]["name"] == "cap" and len(inc) == 1 and inc[0]["from"]["name"] == "c", json.dumps(inc))
th = request("textDocument/prepareTypeHierarchy", {"textDocument": {"uri": main_uri}, "position": {"line": 2, "character": 19}})
sub = request("typeHierarchy/subtypes", {"item": th[0]})
check("type hierarchy: Service has two subtypes", ",".join(sorted(x["name"] for x in sub)) == "Local,Public", json.dumps(sub))
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 40}, "contentChanges": [{"text": "const z = cap(1)\n"}]})
dz = next_diagnostics(main_uri)
ca = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 0, "character": 10}, "end": {"line": 0, "character": 13}}, "context": {"diagnostics": dz["diagnostics"]}})
check("code action: import the unknown name from the module beside", any(x["title"] == 'import cap from "./lib.decl"' for x in ca), json.dumps(ca))
# linked editing and rename of a local variable; the member-kind conversions; flipping a comparison
actions_src = "type Pair = {\n    a: int,\n    b?: int,\n    c = a + 1\n}\nconst xs = [x * 2 for x in [1, 2] if x > 0]\nconst cmp = 3 < 4\n"
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 50}, "contentChanges": [{"text": actions_src}]})
next_diagnostics(main_uri)
le = request("textDocument/linkedEditingRange", {"textDocument": {"uri": main_uri}, "position": {"line": 5, "character": 12}})
check("linked editing of a comprehension variable", bool(le) and len(le["ranges"]) == 3, json.dumps(le))
lr = request("textDocument/rename", {"textDocument": {"uri": main_uri}, "position": {"line": 5, "character": 12}, "newName": "y"})
check("rename of a local variable", bool(lr) and len(lr["changes"][main_uri]) == 3, json.dumps(lr))
conv = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 4}}, "context": {"diagnostics": []}})
check("assists on a derived member: annotate, hide, export", all(any(x["title"] == t for x in conv) for t in ["annotate: int", "make hidden (x$)", "export Pair"]), json.dumps([x["title"] for x in conv]))
flip = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 6, "character": 14}, "end": {"line": 6, "character": 14}}, "context": {"diagnostics": []}})
check("assist: flip the comparison", any(x["title"] == "flip the comparison" and x["edit"]["changes"][main_uri][0]["newText"] == "4 > 3" for x in flip), json.dumps(flip))
# the remaining quick fixes and assists, and the context-variable hints
actions2 = ("type Item = { label = $parent.name }\ntype Owner = { name: string, items: Item[] }\n"
            "const K = 2\nconst twice = K + K\nconst dur = 250ms\n"
            "type R = {\n    d = 1,\n    a: int\n}\n"
            "type Ctx = { $parent: ref<Owner2>, tag = $parent.name }\ntype Owner2 = { name: string, c: Ctx }\n")
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 60}, "contentChanges": [{"text": actions2}]})
d2 = next_diagnostics(main_uri)["diagnostics"]
fix = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 0, "character": 22}, "end": {"line": 0, "character": 22}}, "context": {"diagnostics": d2}})
check("quick fix: declare the context variable", any(x["title"] == "declare $parent: ref<{ ... }> on Item" for x in fix), json.dumps([x["title"] for x in fix]))
inl = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 0}}, "context": {"diagnostics": []}})
check("assist: inline the constant", any(x["title"] == "inline K" and len(x["edit"]["changes"][main_uri]) == 3 for x in inl), json.dumps([x["title"] for x in inl]))
unit = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 4, "character": 12}, "end": {"line": 4, "character": 12}}, "context": {"diagnostics": []}})
check("assist: convert the unit literal", any(x["title"] == "convert to 0.25s" for x in unit), json.dumps([x["title"] for x in unit]))
reorder = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 5, "character": 5}, "end": {"line": 5, "character": 5}}, "context": {"diagnostics": []}})
check("assist: reorder the members", any(x["title"] == "reorder the members canonically" and x["edit"]["changes"][main_uri][0]["newText"] == "{\n    a: int\n    d = 1\n}" for x in reorder), json.dumps([x for x in reorder if x["title"].startswith("reorder")]))
notify_server("workspace/didChangeConfiguration", {"settings": {"decl": {"inlayHints": {"contextVariables": True}}}})
next_diagnostics(main_uri)
ch2 = request("textDocument/inlayHint", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 20, "character": 0}}})
check("inlay hints: the context variable's declared bound", any(h["label"] == ": ref<Owner2>" for h in ch2), json.dumps(ch2))
notify_server("workspace/didChangeConfiguration", {"settings": {"decl": {"inlayHints": {"contextVariables": False}}}})
next_diagnostics(main_uri)
# the conversions, inlining a derived member, on-type formatting
conv_src = ('type Circle = { kind: "circle", r: int }\ntype Rect = { kind: "rect", w: int, h: int }\ntype Shape = Circle | Rect\ninput shape: Shape\n'
            'const area = if shape.kind == "circle" then shape.r * 2 else if shape.kind == "rect" then shape.w * shape.h else 0\n'
            'type Box = {\n    w: int,\n    h: int,\n    area = w * h,\n    big = area > 10,\n    assert fits: w <= 100 else error `too wide: ${w}`\n}\n')
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 70}, "contentChanges": [{"text": conv_src}]})
next_diagnostics(main_uri)
conv = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 4, "character": 13}, "end": {"line": 4, "character": 13}}, "context": {"diagnostics": []}})
check("assist: convert the if chain to match", any(x["title"] == "convert to match" and x["edit"]["changes"][main_uri][0]["newText"].startswith("match shape {\n    (s: Circle) => s.r * 2") for x in conv), json.dumps([x["title"] for x in conv]))
inl2 = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 8, "character": 4}, "end": {"line": 8, "character": 4}}, "context": {"diagnostics": []}})
check("assist: inline the derived member", any(x["title"] == "inline area" and len(x["edit"]["changes"][main_uri]) == 2 and x["edit"]["changes"][main_uri][0]["newText"] == "(w * h)" for x in inl2), json.dumps([x["title"] for x in inl2]))
dg = request("textDocument/codeAction", {"textDocument": {"uri": main_uri}, "range": {"start": {"line": 10, "character": 4}, "end": {"line": 10, "character": 4}}, "context": {"diagnostics": []}})
check("assist: declare a diagnostic for the assert", any(x["title"] == "declare a diagnostic for fits" and x["edit"]["changes"][main_uri][1]["newText"] == "else fits(w)" for x in dg), json.dumps([x["title"] for x in dg]))
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 71}, "contentChanges": [{"text": "type T = {\nx: int,\n    }\n"}]})
next_diagnostics(main_uri)
otf = request("textDocument/onTypeFormatting", {"textDocument": {"uri": main_uri}, "position": {"line": 1, "character": 0}, "ch": "\n", "options": {"tabSize": 4, "insertSpaces": True}})
check("on-type formatting indents after an opening brace", otf == [{"range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0}}, "newText": "    "}], json.dumps(otf))
notify_server("textDocument/didChange", {"textDocument": {"uri": main_uri, "version": 41}, "contentChanges": [{"text": main_src}]})
next_diagnostics(main_uri)
tree = request("workspace/executeCommand", {"command": "decl.showSyntaxTree", "arguments": [main_uri]})
check("decl.showSyntaxTree returns the tree", bool(tree) and tree["tree"].startswith("(module"), json.dumps(tree)[:120])
request("shutdown", {})
notify_server("exit", {})
server.stdin.close()
server.wait(timeout=10)


print("== python API: evaluate binds inputs and returns outputs by name ==")
sys.path.insert(0, str(ROOT / "decl-py"))
import decl as api  # noqa: E402

cfg = str(ROOT / "docs/examples/02_config.decl")
allv = api.evaluate(cfg)
check("default: the exported outputs by name", list(allv) == ["base", "prod", "dev"], str(list(allv)))
check("outputs selects roots", list(api.evaluate(cfg, outputs=["prod"])) == ["prod"])
check("a module exporting nothing yields {}", api.evaluate(fixture) == {})
byfile = api.evaluate(fixture, inputs={"base": doc_path}, outputs=["base", "copy"])
check("an input bound from a file is a root", byfile == {"base": {"host": "h", "port": 8}, "copy": {"host": "h", "port": 8}}, str(byfile))
byvalue = api.evaluate(fixture, inputs=[("base", {"host": "v"})], outputs=["copy"])
check("an input bound from a value completes", byvalue == {"copy": {"host": "v", "port": 80}}, str(byvalue))
try:
    api.evaluate(fixture, outputs=["nope"]); check("an unknown root is a DeclError", False)
except api.DeclError as e:
    check("an unknown root is a DeclError", str(e) == "no root named nope", str(e))
try:
    api.evaluate(fixture, inputs={"base": os.path.join(cli_tmp, "missing.json")}); check("an unreadable document is E6004", False)
except api.DeclError as e:
    check("an unreadable document is E6004", e.diagnostics and e.diagnostics[0]["code"] == "E6004", str(e.diagnostics))
bad_doc = {"host": "x", "port": 70000, "workers": 100, "tls": {"enabled": True}}
try:
    api.evaluate(cfg, inputs={"deployed": bad_doc}, outputs=["deployed"]); check("error diagnostics come back on the DeclError", False)
except api.DeclError as e:
    check("error diagnostics come back on the DeclError", any(d["severity"] == "error" for d in e.diagnostics) and str(e) == e.diagnostics[0]["message"], str(e))
v = api.validate(cfg, inputs={"deployed": bad_doc})
check("validate: diagnostics of a bound document", any(d["severity"] == "error" for d in v), str(v[:2]))
check("check: clean file -> []", api.check(str(ROOT / "tests/validation/types/valid/predicates.decl")) == [])
bad = api.check(str(ROOT / "tests/validation/types/invalid/empty_range.decl"))
check("check: static error with file first", bad and bad[0]["code"] == "E4011" and list(bad[0])[0] == "file", str(bad))
check("format_source", api.format_source("const x=1+2\n") == "const x = 1 + 2\n")
check("__version__", api.__version__ == "0.3.0")
print("== repl: the session corpus, byte for byte, and the command line ==")
import subprocess  # noqa: E402

for case in sorted(p for p in (ROOT / "tests/repl").iterdir() if (p / "session.txt").exists()):
    rel = str(case.relative_to(ROOT))
    entry = [f"{rel}/main.decl"] if (case / "main.decl").exists() else []
    r = subprocess.run([sys.executable, "-m", "decl.cli", "repl", *entry, "--script", f"{rel}/session.txt"],
                       capture_output=True, text=True, cwd=str(ROOT))
    want = (case / "transcript.txt").read_text(encoding="utf-8")
    want_code = 1 if re.search(r"^error: ", want, re.M) else 0
    check(f"repl {case.name}: transcript", r.stdout == want,
          next((f"line {i + 1}: expected {a!r}, got {b!r}" for i, (a, b) in enumerate(zip(want.split("\n"), r.stdout.split("\n"))) if a != b), r.stderr[:300]))
    check(f"repl {case.name}: exit {want_code}", r.returncode == want_code, f"got {r.returncode} {r.stderr[:200]}")
print("== repl: the incremental step is observationally identical to a full recomputation ==")
for case in sorted(p for p in (ROOT / "tests/repl").iterdir() if (p / "session.txt").exists()):
    rel = str(case.relative_to(ROOT))
    entry = [f"{rel}/main.decl"] if (case / "main.decl").exists() else []
    args = [sys.executable, "-m", "decl.cli", "repl", *entry, "--script", f"{rel}/session.txt"]
    inc = subprocess.run(args, capture_output=True, text=True, cwd=str(ROOT))
    full = subprocess.run(args, capture_output=True, text=True, cwd=str(ROOT), env={**os.environ, "DECL_FULL_RECOMPUTE": "1"})
    check(f"repl {case.name}: incremental == full", inc.stdout == full.stdout and inc.returncode == full.returncode,
          next((f"line {i + 1}: full {a!r}, incremental {b!r}" for i, (a, b) in enumerate(zip(full.stdout.split("\n"), inc.stdout.split("\n"))) if a != b), inc.stderr[:300]))
piped = subprocess.run([sys.executable, "-m", "decl.cli", "repl", "tests/repl/documents/main.decl", "--input", "deployed=tests/repl/documents/doc.json", "--script", "-", "--compact"],
                       capture_output=True, text=True, cwd=str(ROOT), input="deployed\n")
check("repl: --input, --script -, --compact", piped.stdout == '> deployed\n{"port":9000,"name":"doc","replicas":1,"label":"doc:9000"}\n(partial)\n', piped.stdout)
check("repl: an unknown option is a usage error", subprocess.run([sys.executable, "-m", "decl.cli", "repl", "--nope"], capture_output=True, cwd=str(ROOT)).returncode == 2)
check("repl: a missing script is a usage error", subprocess.run([sys.executable, "-m", "decl.cli", "repl", "--script", os.path.join(cli_tmp, "nope.txt")], capture_output=True, cwd=str(ROOT)).returncode == 2)

print(f"\nTOTAL {passed} ok, {failed} failed")
sys.exit(1 if failed else 0)
