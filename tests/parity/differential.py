"""Parity across the three implementations.

Decl ships a TypeScript reference implementation (decl-ts), a Rust
runtime (decl-rs), and a Python runtime (decl-py). They must be
indistinguishable at the command line: for every module in the fixture
corpus (valid and invalid), the module and package corpora, the
documentation examples, and the domain examples, `check` and `evaluate` —
with and without `--json` — must produce the same exit code, the same
standard output (the diagnostic report, the serialized values), and the
same standard error, byte for byte; binding documents to input roots
(`validate --input`, `evaluate --input`) likewise; the command line's
whole surface — usage, `--version`, `--expect-errors`, `validate <dir>`,
`fmt --check`, every error path — the same; `fmt` must produce the same
bytes for every parseable module; packages (manifests, the resolver, the
lock) the same reports; the goldens (tests/golden) the same bytes from
every implementation, the reference included; the REPL sessions the same
transcripts; the library APIs (tests/api) the same answers; and the
language servers the same answers to every session of tests/lsp. The
reference is the oracle; both natives are diffed against it, which makes
the three pairwise identical. The only normalization is of temporary
directories the harness itself creates.

    python tests/parity/differential.py                 # rust and python vs reference
    python tests/parity/differential.py --only rust     # one runtime
    DECL_PYTHON=decl-py/.venv/bin/python ...             # the interpreter that has `decl` installed

Prerequisites: `npm ci` at the repository root, `cargo build --release
--examples` (the Cargo workspace), and the Python package importable
(`make python-env`). A missing runtime is a failure, not a skip — `make
verify` is the gate.
"""
from __future__ import annotations

import importlib.util
import json
import re
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TS_CLI = ROOT / "decl-ts/src/cli.ts"
TS_LSP = ROOT / "decl-ts/src/lsp.ts"
RUST_BIN = ROOT / "target/release/decl"
RUST_API = ROOT / "target/release/examples/api_corpus"
PYTHON = os.environ.get("DECL_PYTHON") or sys.executable

only = None
if "--only" in sys.argv:
    only = sys.argv[sys.argv.index("--only") + 1]
RUNTIMES: dict[str, list[str]] = {}
LSP_SERVERS: dict[str, list[str]] = {}
API_DRIVERS: dict[str, list[str]] = {}
if only in (None, "rust"):
    RUNTIMES["rust"] = [str(RUST_BIN)]
    LSP_SERVERS["rust"] = [str(ROOT / "target/release/decl-lsp")]
    API_DRIVERS["rust"] = [str(RUST_API)]
if only in (None, "python"):
    RUNTIMES["python"] = [PYTHON, "-m", "decl"]
    LSP_SERVERS["python"] = [PYTHON, "-m", "decl.lsp"]
    API_DRIVERS["python"] = [PYTHON, str(ROOT / "decl-py/scripts/api_corpus.py")]
if not RUNTIMES:
    sys.exit(f"unknown runtime {only!r} (rust | python)")

# ---------------------------------------------------------------- preflight
missing = []
if not TS_CLI.exists() or not (ROOT / "node_modules").exists():
    missing.append("node_modules (run `npm ci` at the repository root)")
if "rust" in RUNTIMES and not RUST_BIN.exists():
    missing.append("target/release/decl (run `cargo build --release --examples`)")
if "rust" in RUNTIMES and not RUST_API.exists():
    missing.append("target/release/examples/api_corpus (run `cargo build --release --examples`)")
if "python" in RUNTIMES:
    probe = subprocess.run([PYTHON, "-c", "import decl"], capture_output=True, text=True, cwd=str(ROOT))
    if probe.returncode:
        missing.append(f"python package not importable by {PYTHON} (run `make python-env`): {probe.stderr.strip().splitlines()[-1] if probe.stderr.strip() else ''}")
if missing:
    print("parity: prerequisites missing —\n  " + "\n  ".join(missing))
    sys.exit(2)

REF = ["node", str(TS_CLI)]
names = list(RUNTIMES)
width = max(len(n) for n in names)
same = diff = 0

# temporary directories this harness creates: the one thing normalized away
tmp = Path(tempfile.mkdtemp(prefix="decl-parity-"))
NORMALIZE: list[tuple[str, str]] = [(str(tmp), "<tmp>")]

# a piped session: what `--script -` reads, and what a repl without --script
# reads when its standard input is not a terminal (02_repl.md §9)
SESSION = "deployed\n:roots\n"


def outcome(cmd: list[str], args: list[str], cwd: Path | None = None, stdin: str | None = None) -> tuple[int, str, str]:
    """(exit code, stdout, stderr) of one command line, temp paths normalized"""
    if stdin is None:
        stdin = SESSION if args[:1] == ["repl"] and ("--script" not in args or args[-2:] == ["--script", "-"]) else ""
    r = subprocess.run(cmd + args, capture_output=True, text=True, check=False, cwd=str(cwd or ROOT), input=stdin)
    out, err = r.stdout, r.stderr
    for a, b in NORMALIZE:
        out, err = out.replace(a, b), err.replace(a, b)
    # milliseconds are the clock's, not the session's (a REPL's :time)
    out = re.sub(r"\d+\.\d ms", "<ms> ms", out)
    return (r.returncode, out, err)


def canonical(v) -> str:
    return json.dumps(v, separators=(",", ":"), ensure_ascii=False)


def same_json(a, b) -> bool:
    """structural equality of two parsed answers: key order counts, and a
    number compares by value (a JavaScript reader gives 30 for 30.0)"""
    if isinstance(a, bool) or isinstance(b, bool):
        return type(a) is type(b) and a == b
    if isinstance(a, dict) and isinstance(b, dict):
        return list(a) == list(b) and all(same_json(a[k], b[k]) for k in a)
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(same_json(x, y) for x, y in zip(a, b))
    return a == b


def describe(o: tuple[int, str, str]) -> str:
    return f"exit {o[0]} out={o[1][-160:]!r} err={o[2][-160:]!r}"


def row(label: str, verdicts: dict[str, bool], detail: dict[str, str]) -> None:
    global same, diff
    ok = all(verdicts.values())
    same += ok
    diff += not ok
    cells = "  ".join(f"{n:<{width}} {'same' if verdicts[n] else 'DIFF'}" for n in names)
    print(f"  {cells}  {label}")
    for n in names:
        if not verdicts[n]:
            print(f"      {n}: {detail[n]}")


def cli_row(label: str, args: list[str], cwd_of=None, stdin: str | None = None, expected: tuple | None = None) -> None:
    """one command line, byte-identical outcome required of every runtime; `cwd_of(name)`
    gives each run its own directory; `expected` is a reviewed outcome the reference must match"""
    ref = outcome(REF, args, cwd_of("ref") if cwd_of else None, stdin)
    verdicts, detail = {}, {}
    ref_ok = expected is None or ref == expected
    for n, prefix in RUNTIMES.items():
        nat = outcome(prefix, args, cwd_of(n) if cwd_of else None, stdin)
        if not ref_ok:
            verdicts[n] = False
            detail[n] = f"the reference differs from the expectation — expected {describe(expected)} | ref {describe(ref)}"
            continue
        verdicts[n] = ref == nat
        if ref != nat:
            what = "exit code" if ref[0] != nat[0] else "stdout" if ref[1] != nat[1] else "stderr"
            detail[n] = f"{what} differs — ref {describe(ref)} | {n} {describe(nat)}"
    row(label, verdicts, detail)


# ---------------------------------------------------------------- check
check_files: list[Path] = (
    sorted((ROOT / "tests/validation").rglob("*.decl"))
    + sorted((ROOT / "tests/modules").rglob("*.decl"))
    + sorted((ROOT / "docs/examples").glob("*.decl"))
)
for d in sorted((ROOT / "examples").iterdir()):
    if d.is_dir():
        mods = sorted(d.glob("*.decl"))
        entry = d / "main.decl" if (d / "main.decl").exists() else (mods[0] if len(mods) == 1 else None)
        if entry:
            check_files.append(entry)

print(f"== check: {len(check_files)} modules, reference vs {', '.join(names)} (exit, stdout, stderr — with and without --json)")
for p in check_files:
    rel = str(p.relative_to(ROOT))
    cli_row(f"{rel} (--json)", ["check", rel, "--json"])
    cli_row(rel, ["check", rel])

# ---------------------------------------------------------------- evaluate
files: list[Path] = []
for d in ("tests/validation", "docs/examples", "examples"):
    for p in sorted((ROOT / d).rglob("*.decl")):
        if "output " in p.read_text(encoding="utf-8") and "/invalid/" not in str(p):
            files.append(p)

print(f"== evaluate: {len(files)} modules (exit, stdout, stderr — with and without --json; then each output by --output)")
for p in files:
    rel = str(p.relative_to(ROOT))
    cli_row(f"{rel} (--json)", ["evaluate", rel, "--json"])
    cli_row(rel, ["evaluate", rel])
    # every output the module declares, exported or not, as the one document on stdout
    for name in re.findall(r"^(?:export\s+)?output\s+([A-Za-z_][A-Za-z0-9_]*)", p.read_text(encoding="utf-8"), re.M):
        cli_row(f"{rel} (--output {name})", ["evaluate", rel, "--output", name])


def file_row(label: str, args_of, out_of) -> None:
    """one `--output name=file` command line: outcome and the written bytes identical"""
    ref_file = out_of("ref")
    ref = outcome(REF, args_of(ref_file)) + (ref_file.read_text(encoding="utf-8") if ref_file.exists() else None,)
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        f = out_of(n)
        nat = outcome(prefix, args_of(f)) + (f.read_text(encoding="utf-8") if f.exists() else None,)
        verdicts[n] = ref == nat
        if ref != nat:
            what = "exit code" if ref[0] != nat[0] else "stdout" if ref[1] != nat[1] else "stderr" if ref[2] != nat[2] else "written file"
            detail[n] = f"{what} differs — ref {describe(ref[:3])} | {n} {describe(nat[:3])}"
    row(label, verdicts, detail)


ic0 = ROOT / "docs/examples/02_config.decl"
file_row("evaluate --output name=file (two roots to two files, one to stdout)",
         lambda f: ["evaluate", str(ic0.relative_to(ROOT)), "--output", f"prod={f}", "--output", f"dev={f}.dev", "--output", "base"],
         lambda n: tmp / f"cfg-{n}.json")

# ---------------------------------------------------------------- documents bound to input roots
cases: list[tuple[str, Path, str, Path]] = []
cfg = ROOT / "docs/examples/02_config.decl"
bad = tmp / "deployed.json"
bad.write_text('{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}', encoding="utf-8")
cases.append(("config: invalid deployment", cfg, "deployed", bad))
ic = ROOT / "docs/examples/01_interconnect.decl"
ser = outcome(REF, ["evaluate", str(ic.relative_to(ROOT)), "--output", "xbar"])[1].strip()
good = tmp / "xbar.json"
good.write_text(ser, encoding="utf-8")
cases.append(("interconnect: round trip", ic, "doc", good))
probe = '"mi1":{"kind":"ext","mode":"mi","width":64}'
assert probe in ser, "corruption probe no longer matches the serialized form"
corrupt = tmp / "xbar_corrupt.json"
corrupt.write_text(ser.replace(probe, probe.replace("64", "32"), 1), encoding="utf-8")
cases.append(("interconnect: corrupted width", ic, "doc", corrupt))
malformed = tmp / "malformed.json"
malformed.write_text('{"host": ', encoding="utf-8")
cases.append(("config: malformed document", cfg, "deployed", malformed))
trailing = tmp / "trailing.json"
trailing.write_text('{"host": "a"} x', encoding="utf-8")
cases.append(("config: document with trailing characters", cfg, "deployed", trailing))
cases.append(("config: unreadable document", cfg, "deployed", tmp / "missing.json"))

print(f"== validate --input / evaluate --input: {len(cases)} documents (exit, stdout, stderr)")
for label, decl, name, doc in cases:
    rel = str(decl.relative_to(ROOT))
    cli_row(f"{label} (validate --json)", ["validate", rel, "--input", f"{name}={doc}", "--json"])
    cli_row(f"{label} (validate)", ["validate", rel, "--input", f"{name}={doc}"])
    cli_row(f"{label} (evaluate --output {name} --json)", ["evaluate", rel, "--input", f"{name}={doc}", "--output", name, "--json"])
    cli_row(f"{label} (evaluate --output {name})", ["evaluate", rel, "--input", f"{name}={doc}", "--output", name])
CFG = str(cfg.relative_to(ROOT))
cli_row("evaluate: --output names nothing", ["evaluate", CFG, "--output", "nope", "--json"])
cli_row("evaluate: --output without a name", ["evaluate", CFG, "--output", "=x.json"])
cli_row("evaluate: two --output to stdout", ["evaluate", CFG, "--output", "prod", "--output", "dev"])
cli_row("evaluate: a module exporting no output", ["evaluate", "tests/validation/declarations/valid/output_from_input_fallback.decl"])
cli_row("evaluate: --output of an unwritable file", ["evaluate", CFG, "--output", f"prod={tmp}/no/such/dir/x.json"])
cli_row("evaluate: --input without name=", ["evaluate", CFG, "--input", "nope"])
cli_row("evaluate: --input of an unknown input", ["evaluate", CFG, "--input", f"nope={bad}"])
cli_row("evaluate: the same input bound twice", ["evaluate", CFG, "--input", f"deployed={bad}", "--input", f"deployed={bad}", "--output", "deployed"])

# ---------------------------------------------------------------- the command line's surface
print("== the command line's surface: usage, --version, several entries, --expect-errors, validate <dir>, fmt --check, error paths")
cli_row("--version", ["--version"])
cli_row("usage: no arguments", [])
cli_row("usage: --help", ["--help"])
cli_row("usage: an unknown command", ["bogus"])
cli_row("usage: check without a file", ["check"])
cli_row("usage: evaluate without a file", ["evaluate"])
cli_row("usage: validate without a target", ["validate"])
cli_row("usage: fmt without a file", ["fmt"])
cli_row("usage: fmt --check without a file", ["fmt", "--check"])
cli_row("usage: repl --help", ["repl", "--help"])
cli_row("check: several entry files", ["check", "tests/modules/basic/main.decl", "tests/modules/errors/collision.decl", "tests/modules/errors/root_a.decl"])
cli_row("check: several entry files (--json)", ["check", "tests/modules/basic/main.decl", "tests/modules/errors/collision.decl", "tests/modules/errors/root_a.decl", "--json"])
cli_row("check: an unknown flag", ["check", "--nope", "tests/modules/basic/main.decl"])
cli_row("check: a missing file", ["check", f"{tmp}/missing.decl"])
cli_row("check: a missing file (--json)", ["check", f"{tmp}/missing.decl", "--json"])
cli_row("evaluate: a missing file", ["evaluate", f"{tmp}/missing.decl"])
cli_row("evaluate: a module with a static error", ["evaluate", "tests/validation/types/invalid/empty_range.decl"])
cli_row("evaluate: a module with a static error (--json)", ["evaluate", "tests/validation/types/invalid/empty_range.decl", "--json"])
cli_row("evaluate --json: a usage error prints no report", ["evaluate", CFG, "--output", "prod", "--json", "--output", "dev"])
cli_row("validate <dir>: the fixture corpus", ["validate", "tests/validation"])
cli_row("validate: a module with imports, nothing bound", ["validate", "tests/modules/basic/main.decl"])
cli_row("validate: a module with imports (--json)", ["validate", "tests/modules/basic/main.decl", "--json"])
cli_row("validate: a file that does not parse", ["validate", "tests/validation/lexical/invalid/semicolon.decl"])
cli_row("validate: a missing file", ["validate", f"{tmp}/missing.decl"])
cli_row("validate --expect-errors: the codes match", ["validate", CFG, "--input", f"deployed={bad}", "--expect-errors", "E4001,E6001"])
cli_row("validate --expect-errors: a code missing and one unexpected", ["validate", CFG, "--input", f"deployed={bad}", "--expect-errors", "E4001,E9999"])
cli_row("validate --expect-errors: --json", ["validate", CFG, "--input", f"deployed={bad}", "--expect-errors", "E4001,E6001", "--json"])
cli_row("validate --expect-errors: a document that cannot be read", ["validate", CFG, "--input", f"deployed={tmp}/missing.json", "--expect-errors", "E4001"])
cli_row("validate --expect-errors: a malformed document, expected", ["validate", CFG, "--input", f"deployed={malformed}", "--expect-errors", "E6004"])
cli_row("validate --expect-errors: a file that does not parse", ["validate", "tests/validation/lexical/invalid/semicolon.decl", "--expect-errors", "E2001"])
cli_row("validate --expect-errors: nothing expected, nothing reported", ["validate", "tests/modules/basic/main.decl", "--expect-errors", ""])
cli_row("validate --expect-errors: without a value", ["validate", "tests/modules/basic/main.decl", "--expect-errors"])
cli_row("fmt --check: a canonical corpus", ["fmt", "--check", *sorted(str(p.relative_to(ROOT)) for p in (ROOT / "tests/modules/basic").glob("*.decl"))])
messy = tmp / "messy.decl"
messy.write_text("const x=1+2\ntype T = {a: int,b?: string}\n", encoding="utf-8")
cli_row("fmt --check: a file that is not canonical", ["fmt", "--check", str(messy)])
unparseable = tmp / "unparseable.decl"
unparseable.write_text((ROOT / "tests/validation/lexical/invalid/semicolon.decl").read_text(encoding="utf-8"), encoding="utf-8")
cli_row("fmt: a file that does not parse", ["fmt", str(unparseable)])
cli_row("fmt: a missing file", ["fmt", f"{tmp}/missing.decl"])
cli_row("fmt --check: a missing file", ["fmt", "--check", f"{tmp}/missing.decl"])

# ---------------------------------------------------------------- the command-line corpus
cli_cases = json.loads((ROOT / "tests/cli/cases.json").read_text(encoding="utf-8"))
REF_VERSION = json.loads((ROOT / "decl-ts/package.json").read_text(encoding="utf-8"))["version"]
def replay_cases(corpus: str, cases: list[dict]) -> None:
    """a recorded corpus (tests/cli/README.md's shape): every case through the three, in its own copy of the files"""
    for c in cases:
        replay_case(corpus, c)


def replay_case(corpus: str, c: dict) -> None:
    dirs: dict[str, Path] = {}

    def dir_of(n: str, c=c, dirs=dirs) -> Path:
        d = tmp / f"{corpus}-{len(dirs)}-{n}-{abs(hash(c['name'])) % 100000}"
        d.mkdir(parents=True, exist_ok=True)
        for f, t in (c.get("files") or {}).items():
            (d / f).parent.mkdir(parents=True, exist_ok=True)
            (d / f).write_text(t, encoding="utf-8")
        dirs[n] = d
        return d

    program_ref = ["node", str(TS_LSP if c.get("program") == "decl-lsp" else TS_CLI)]
    programs = {n: ([PYTHON, "-m", "decl.lsp"] if n == "python" else [str(ROOT / "target/release/decl-lsp")]) if c.get("program") == "decl-lsp" else prefix for n, prefix in RUNTIMES.items()}

    def run_case(cmd: list[str], n: str) -> tuple:
        d = dir_of(n)
        args = [a.replace("<dir>", str(d)) for a in c["args"]]
        r = subprocess.run(cmd + args, capture_output=True, text=True, check=False, cwd=str(ROOT), input=c.get("stdin", ""))
        norm = lambda x: x.replace(str(d), "<dir>").replace(REF_VERSION, "<version>")
        after = {f: ((d / f).read_text(encoding="utf-8") if (d / f).exists() else None) for f in (c.get("after") or {})}
        return (r.returncode, norm(r.stdout), norm(r.stderr), after)

    ref = run_case(program_ref, "ref")
    want = (c["exit"], c["stdout"], c["stderr"], c.get("after") or {})
    ref_ok = ref == want
    verdicts, detail = {}, {}
    for n in names:
        nat = run_case(programs[n], n)
        verdicts[n] = ref_ok and nat == want
        detail[n] = ("the reference differs from the recorded outcome — " if not ref_ok else "") + f"expected {describe(want[:3])} | ref {describe(ref[:3])} | {n} {describe(nat[:3])}" + ("" if ref[3] == nat[3] == want[3] else " (the files left differ)")
    row(f"{corpus}: {c['name']}", verdicts, detail)


print(f"== cli: {len(cli_cases)} cases of tests/cli (the recorded outcome; exit, stdout, stderr, the files left)")
replay_cases("cli", cli_cases)

# ---------------------------------------------------------------- fmt
fmt_files: list[Path] = []
for d in ("tests/validation", "tests/modules", "tests/packages", "tests/golden/inputs", "docs/examples", "examples"):
    fmt_files += sorted((ROOT / d).rglob("*.decl"))
fmt_tmp = tmp / "fmt"
fmt_tmp.mkdir()


def fmt_with(cmd: list[str], src: str) -> tuple:
    p = fmt_tmp / "x.decl"
    p.write_text(src, encoding="utf-8")
    r = subprocess.run(cmd + ["fmt", str(p)], capture_output=True, text=True, check=False, cwd=str(ROOT))
    return (r.returncode, p.read_text(encoding="utf-8"), r.stderr.replace(str(tmp), "<tmp>"))


fmt_cases = json.loads((ROOT / "tests/fmt/cases.json").read_text(encoding="utf-8"))
print(f"== fmt: {len(fmt_cases)} cases of tests/fmt (the expected form; exit, stderr), then {len(fmt_files)} modules, byte-identical output (and exit, stderr)")
for c in fmt_cases:
    ref = fmt_with(REF, c["input"])
    ref_ok = (ref[1] == c["expected"] and ref[0] == 0) if "expected" in c else ref[0] != 0
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = fmt_with(prefix, c["input"])
        verdicts[n] = ref_ok and ref == nat
        detail[n] = ("the reference differs from the expected form — " if not ref_ok else "") + f"ref exit {ref[0]} {ref[1]!r} err={ref[2]!r} | {n} exit {nat[0]} {nat[1]!r} err={nat[2]!r}"
    row(f"fmt case: {c['name']}", verdicts, detail)
for p in fmt_files:
    src = p.read_text(encoding="utf-8")
    ref = fmt_with(REF, src)
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = fmt_with(prefix, src)
        verdicts[n] = ref == nat
        detail[n] = f"ref exit {ref[0]} err={ref[2]!r} | {n} exit {nat[0]} err={nat[2]!r}" + ("" if ref[1] == nat[1] else " (text differs)")
    row(str(p.relative_to(ROOT)), verdicts, detail)

# ---------------------------------------------------------------- packages
print("== packages: manifests, resolver, lock (check + evaluate, exit, stdout, stderr)")
for entry in sorted((ROOT / "tests/packages").glob("*/main.decl")):
    rel = str(entry.relative_to(ROOT))
    for cmd_name in ("check", "evaluate"):
        cli_row(f"{rel} ({cmd_name} --json)", [cmd_name, rel, "--json"])
        cli_row(f"{rel} ({cmd_name})", [cmd_name, rel])

# ---------------------------------------------------------------- golden
# the evaluation of every manifest entry against the committed expected
# bytes (tests/golden/README.md): every implementation — the reference
# included — must print exactly those bytes, so the expected outputs are
# reviewed data, not whatever the reference happens to print
golden_manifest = json.loads((ROOT / "tests/golden/manifest.json").read_text())
print(f"== golden: {len(golden_manifest)} evaluations against tests/golden (every implementation, the reference included)")


def module_of(g: dict) -> str:
    """a markdown entry's module: its ```decl blocks in order, in a temporary file"""
    if "module" in g:
        return g["module"]
    md = (ROOT / g["markdown"]).read_text(encoding="utf-8")
    p = tmp / (Path(g["markdown"]).stem + ".decl")
    p.write_text("\n".join(re.findall(r"```decl\n([\s\S]*?)```", md)), encoding="utf-8")
    return str(p)


for g in golden_manifest:
    # a golden is the evaluation's stdout; a `rejected` document's golden is
    # validate's exit 1 and its stderr — the diagnostics, in canonical order
    rejected = g.get("rejected", False)
    args = ["validate" if rejected else "evaluate", module_of(g)] + [x for spec in g.get("inputs", []) for x in ("--input", spec)] + ([] if "output" not in g else ["--output", g["output"]])
    expected = (ROOT / g["golden"]).read_text()
    want_exit, stream = (1, 2) if rejected else (0, 1)
    ref = outcome(REF, args)
    verdicts, detail = {}, {}
    ref_ok = ref[0] == want_exit and ref[stream] == expected
    for n, prefix in RUNTIMES.items():
        nat = outcome(prefix, args)
        verdicts[n] = ref_ok and nat[0] == want_exit and nat[stream] == expected
        if not verdicts[n]:
            detail[n] = ("the reference differs from the golden — " if not ref_ok else "") + f"ref {describe(ref)} | {n} {describe(nat)}"
    row(f"golden {g['golden']}", verdicts, detail)

# ---------------------------------------------------------------- render: documents in YAML, the layouts
# (tests/render/README.md): every golden document bound from its YAML twin
# must give the golden; the documents under invalid/ must be refused with
# their messages; the layouts of formats.json (`--format yaml`, `--indent n`)
# must print the committed bytes — the reference included
def expect_row(label: str, args: list[str], stream: int, want_exit: int, expected: str) -> None:
    """a reviewed text on one stream (1 stdout, 2 stderr) and an exit code, required of the reference and identical from every runtime"""
    ref = outcome(REF, args)
    verdicts, detail = {}, {}
    ref_ok = ref[0] == want_exit and ref[stream] == expected
    for n, prefix in RUNTIMES.items():
        nat = outcome(prefix, args)
        verdicts[n] = ref_ok and nat == ref
        if not verdicts[n]:
            detail[n] = ("the reference differs from the expectation — " if not ref_ok else "") + f"ref {describe(ref)} | {n} {describe(nat)}"
    row(label, verdicts, detail)


twin_of = lambda spec: re.sub(r"=tests/golden/inputs/(.*)\.json$", r"=tests/render/inputs/\1.yaml", spec)
twin_rows = [g for g in golden_manifest if g.get("inputs") and [twin_of(s) for s in g["inputs"]] != g["inputs"]]
invalid_docs = json.loads((ROOT / "tests/render/invalid/cases.json").read_text())
print(f"== yaml-input: {len(twin_rows)} goldens bound from their YAML twins, {len(invalid_docs)} documents the reader refuses (exit, stdout, stderr)")
for g in twin_rows:
    rejected = g.get("rejected", False)
    args = ["validate" if rejected else "evaluate", module_of(g)] + [x for spec in g["inputs"] for x in ("--input", twin_of(spec))] + ([] if "output" not in g else ["--output", g["output"]])
    expect_row(f"yaml twin of {g['golden']}", args, 2 if rejected else 1, 1 if rejected else 0, (ROOT / g["golden"]).read_text())
for c in invalid_docs:
    doc = f"tests/render/invalid/{c['file']}"
    cli_row(f"refused: {c['file']}", ["validate", "tests/render/invalid/doc.decl", "--input", f"doc={doc}"])
    cli_row(f"refused: {c['file']} (evaluate --json)", ["evaluate", "tests/render/invalid/doc.decl", "--input", f"doc={doc}", "--output", "doc", "--json"])

formats = json.loads((ROOT / "tests/render/formats.json").read_text())
print(f"== format: {len(formats)} goldens laid out as YAML and indented JSON (exit, stdout, stderr; every implementation, the reference included)")
for f in formats:
    g = next(m for m in golden_manifest if m["golden"] == f["golden"])
    base = ["evaluate", module_of(g)] + [x for spec in g.get("inputs", []) for x in ("--input", spec)] + ([] if "output" not in g else ["--output", g["output"]])
    expect_row(f"{f['yaml']}: --format yaml", base + ["--format", "yaml"], 1, 0, (ROOT / f["yaml"]).read_text())
    for n, file in f["indent"].items():
        expect_row(f"{file}: --indent {n}", base + ["--indent", n], 1, 0, (ROOT / file).read_text())
    cli_row(f"{f['golden']}: --pretty", base + ["--pretty"])
    cli_row(f"{f['golden']}: --format yaml --indent 4", base + ["--format", "yaml", "--indent", "4"])
    cli_row(f"{f['golden']}: --json --indent 2 (the report carries the document)", base + ["--json", "--indent", "2"])
FMT0 = "docs/examples/02_config.decl"
cli_row("format: --format of something else", ["evaluate", FMT0, "--format", "xml"])
cli_row("format: --format without a value", ["evaluate", FMT0, "--format"])
cli_row("format: --json with --format yaml", ["evaluate", FMT0, "--json", "--format", "yaml"])
cli_row("format: --indent out of range", ["evaluate", FMT0, "--indent", "17"])
cli_row("format: --indent not a number", ["evaluate", FMT0, "--indent", "two"])
cli_row("format: --indent with --pretty", ["evaluate", FMT0, "--indent", "2", "--pretty"])
cli_row("format: --output name=- to stdout", ["evaluate", FMT0, "--output", "prod=-", "--format", "yaml"])
cli_row("format: a declared file is honoured and -", ["evaluate", "tests/validation/declarations/valid/annotations.decl", "--output", "demo"])

render_cases = json.loads((ROOT / "tests/render/cases.json").read_text())
print(f"== render: {len(render_cases)} cases of tests/render (templates, @render, fan-out — the recorded outcome; exit, stdout, stderr, the files left)")
replay_cases("render", render_cases)

# scale: the fabric site generator must reproduce the committed 2x4 site, and
# a 10x20 site (200 links, 30 switches) must be accepted with identical output
_spec = importlib.util.spec_from_file_location("gen_site", ROOT / "tests/golden/inputs/fabric/gen_site.py")
assert _spec and _spec.loader
gen_site = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gen_site)
assert gen_site.site_text(2, 4) == (ROOT / "tests/golden/inputs/fabric/site_2x4.json").read_text(encoding="utf-8"), "gen_site.py no longer reproduces site_2x4.json"
big = tmp / "site_10x20.json"
big.write_text(gen_site.site_text(10, 20), encoding="utf-8")
print("== scale: a generated 10x20 fabric site (exit, stdout, stderr)")
cli_row("scale: evaluate --output site", ["evaluate", "tests/golden/inputs/fabric/site.decl", "--input", f"site={big}", "--output", "site"])
cli_row("scale: validate", ["validate", "tests/golden/inputs/fabric/site.decl", "--input", f"site={big}"])

# ---------------------------------------------------------------- repl
import shutil

repl_cases = sorted(p for p in (ROOT / "tests/repl").iterdir() if (p / "session.txt").exists())
print(f"== repl: {len(repl_cases)} scripted sessions, each in a fresh copy of its directory (the transcript, the exit status, the files left)")
for case in repl_cases:
    copies: dict[str, Path] = {}

    def copy_of(n: str, case=case, copies=copies) -> Path:
        d = tmp / f"repl-{case.name}-{n}"
        if d.exists():
            shutil.rmtree(d)
        shutil.copytree(case, d)
        copies[n] = d
        return d

    entry = ["main.decl"] if (case / "main.decl").exists() else []
    want = (1 if re.search(r"^error: ", (case / "transcript.txt").read_text(encoding="utf-8"), re.M) else 0, (case / "transcript.txt").read_text(encoding="utf-8"))
    ref = outcome(REF, ["repl", *entry, "--script", "session.txt"], copy_of("ref"))
    ref_ok = (ref[0], ref[1]) == want
    expected_dir = case / "expected"
    left = lambda d: {f.name: ((d / f.name).read_text(encoding="utf-8") if (d / f.name).exists() else None) for f in sorted(expected_dir.iterdir())} if expected_dir.is_dir() else {}
    want_left = {f.name: f.read_text(encoding="utf-8") for f in sorted(expected_dir.iterdir())} if expected_dir.is_dir() else {}
    ref_ok = ref_ok and left(copies["ref"]) == want_left
    verdicts, detail = {}, {}
    for n, prefix in RUNTIMES.items():
        nat = outcome(prefix, ["repl", *entry, "--script", "session.txt"], copy_of(n))
        verdicts[n] = ref_ok and ref == nat and left(copies[n]) == want_left
        detail[n] = ("the reference differs from the transcript — " if not ref_ok else "") + f"ref {describe(ref)} | {n} {describe(nat)}" + ("" if left(copies[n]) == want_left else " (the files left differ)")
    row(f"repl {case.name}", verdicts, detail)
cli_row("repl: --input binds before the first line, --script - reads stdin", ["repl", "tests/repl/documents/main.decl", "--input", "deployed=tests/repl/documents/doc.json", "--script", "-"])
cli_row("repl: a piped standard input without --script is the script", ["repl", "tests/repl/documents/main.decl", "--input", "deployed=tests/repl/documents/doc.json"])
cli_row("repl: a missing script is a usage error", ["repl", "--script", f"{tmp}/nope.txt"])
cli_row("repl: an unknown option is a usage error", ["repl", "--nope"])
cli_row("repl: --input without an entry file is a usage error", ["repl", "--input", "deployed=tests/repl/documents/doc.json"])

# ---------------------------------------------------------------- api
# the library APIs (tests/api/README.md): every driver's answers against the
# reviewed expected answers, documents compared by value
api_expected = json.loads((ROOT / "tests/api/expected.json").read_text(encoding="utf-8"))
print(f"== api: {len(api_expected)} cases of tests/api through the three library APIs, against tests/api/expected.json")


def api_answers(cmd: list[str]) -> list:
    r = subprocess.run(cmd, capture_output=True, text=True, check=False, cwd=str(ROOT))
    if r.returncode:
        return [{"driver failed": r.stderr[-400:]}]
    return json.loads(r.stdout)


api_ref = api_answers(["node", str(ROOT / "decl-ts/scripts/api-corpus.ts")])
api_nat = {n: api_answers(cmd) for n, cmd in API_DRIVERS.items()}
for i, want in enumerate(api_expected):
    ref_v = api_ref[i] if i < len(api_ref) else None
    ref_ok = same_json(ref_v, want)
    verdicts, detail = {}, {}
    for n in names:
        nat_v = api_nat[n][i] if i < len(api_nat[n]) else None
        verdicts[n] = ref_ok and same_json(nat_v, want)
        detail[n] = ("the reference differs from the expected answer — " if not ref_ok else "") + f"expected={canonical(want)[:160]} | ref={canonical(ref_v)[:160]} | {n}={canonical(nat_v)[:160]}"
    row(f"api: {want['name']}", verdicts, detail)

# ---------------------------------------------------------------- lsp
print("== decl-lsp --version: the same line from the three servers (exit, stdout, stderr)")
ref = outcome(["node", str(TS_LSP)], ["--version"])
verdicts, detail = {}, {}
for n, cmd in LSP_SERVERS.items():
    nat = outcome(cmd, ["--version"])
    # the reference's line is `decl-lsp <version>`; a server that prints nothing is not the same server
    verdicts[n] = ref == nat and ref[0] == 0 and ref[1].startswith("decl-lsp ")
    detail[n] = f"ref {describe(ref)} | {n} {describe(nat)}"
row("decl-lsp --version", verdicts, detail)

# every session of tests/lsp over the three servers, against its transcript
# (tests/lsp/README.md): the reference must match the reviewed transcript too
_spec = importlib.util.spec_from_file_location("replay", ROOT / "tests/lsp/replay.py")
assert _spec and _spec.loader
replay = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(replay)
lsp_cases = replay.cases(ROOT)
print(f"== lsp: {len(lsp_cases)} sessions of tests/lsp over the three servers, against their transcripts")
for case in lsp_cases:
    want = json.loads((case / "transcript.json").read_text(encoding="utf-8"))
    ref_t = replay.replay(case, ["node", str(TS_LSP)])
    nat_t = {n: replay.replay(case, cmd) for n, cmd in LSP_SERVERS.items()}
    for i, (label, want_v) in enumerate(want):
        ref_v = ref_t[i][1] if i < len(ref_t) else None
        ref_ok = same_json(ref_v, want_v)
        verdicts, detail = {}, {}
        for n in names:
            nat_v = nat_t[n][i][1] if i < len(nat_t[n]) else None
            verdicts[n] = ref_ok and same_json(nat_v, want_v)
            detail[n] = ("the reference differs from the transcript — " if not ref_ok else "") + f"transcript={canonical(want_v)[:160]} | ref={canonical(ref_v)[:160]} | {n}={canonical(nat_v)[:160]}"
        row(f"lsp {case.name}: {label}", verdicts, detail)

# ---------------------------------------------------------------- internal checks
# the internal checks (tests/internal/checks.json): every check carried, under
# its name, by the three implementations' own suites (tests/internal/README.md)
_spec = importlib.util.spec_from_file_location("coverage", ROOT / "tests/internal/coverage.py")
assert _spec and _spec.loader
coverage = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(coverage)
internal_checks = coverage.declared()
carried = {lang: coverage.carried(lang) for lang in ("typescript", "rust", "python")}
print(f"== internal: {len(internal_checks)} checks of tests/internal carried by the three suites, under their names")
for name in internal_checks:
    ref_ok = name in carried["typescript"]
    verdicts, detail = {}, {}
    for n in names:
        verdicts[n] = ref_ok and name in carried[n]
        detail[n] = ("the reference lacks the check — " if not ref_ok else "") + (f"{n} lacks the check" if name not in carried[n] else "")
    row(f"internal: {name}", verdicts, detail)
for lang, have in carried.items():
    for name in sorted(have - set(internal_checks)):
        diff += 1
        print(f"  {lang} carries a check checks.json does not name: {name}")

print(f"\n{same} identical, {diff} different (reference vs {', '.join(names)})")
sys.exit(1 if diff else 0)
