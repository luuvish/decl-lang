"""Packages, decl.toml, and decl.lock (§8.6–8.7) — a port of the reference
implementation's package.ts: exact-pinned dependencies, fail-closed
manifests, content-hashed reproducibility. Conventions: dependency
packages live under <root>/decl_modules/<name>/ in a flat layout, and
the lock file is line-based `name version sha256` in name order."""
from __future__ import annotations

import hashlib
import json
import os
import re
from typing import Any, Callable, Optional

NAME_RE = re.compile(r"^[a-z][a-z0-9_-]*$")
VERSION_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
SEMANTIC = ["name", "version"]
METADATA = ["description", "license", "authors", "repository", "keywords"]


def parse_manifest(path: str, report: Callable[[str, str], None]) -> Optional[dict]:
    try:
        with open(path, encoding="utf-8") as f:
            src = f.read()
    except OSError:
        report("E3004", f"manifest not found: {path}")
        return None
    fields: dict = {}
    deps: dict = {}
    section: Optional[str] = None
    ok = True
    for line0 in src.split("\n"):
        line = re.sub(r"#.*$", "", line0).strip()
        if not line:
            continue
        sec = re.match(r"^\[([^\]]+)\]$", line)
        if sec:
            section = sec.group(1)
            if section != "dependencies":
                report("E3011", f"manifest {path}: unknown section [{section}] (fail-closed, D28)")
                ok = False
            continue
        kv = re.match(r"^([A-Za-z0-9_-]+)\s*=\s*(.+)$", line)
        if not kv:
            report("E3011", f'manifest {path}: unparseable line "{line}"')
            ok = False
            continue
        key = kv.group(1)
        raw = kv.group(2).strip()
        if raw.startswith('"'):
            try:
                value: Any = json.loads(raw.replace("\\", "\\\\"))
            except json.JSONDecodeError:
                report("E3011", f'manifest {path}: unparseable line "{line}"')
                ok = False
                continue
        else:
            value = raw
        if section == "dependencies":
            if not NAME_RE.match(key):
                report("E3013", f"manifest {path}: invalid package name {key}")
                ok = False
                continue
            if not isinstance(value, str) or not VERSION_RE.match(value):
                report("E3012", f"manifest {path}: dependency {key} = {raw} is not an exact semantic-version pin")
                ok = False
                continue
            deps[key] = value
        elif section is None:
            if key not in SEMANTIC and key not in METADATA:
                report("E3011", f"manifest {path}: unknown field {key} (fail-closed, D28)")
                ok = False
                continue
            if isinstance(value, str):
                fields[key] = value
    name = fields.get("name", "")
    version = fields.get("version", "")
    if not NAME_RE.match(name):
        report("E3013", f"manifest {path}: invalid package name {json.dumps(name)}")
        ok = False
    if not VERSION_RE.match(version):
        report("E3012", f"manifest {path}: version {json.dumps(version)} is not an exact triple")
        ok = False
    return {"name": name, "version": version, "dependencies": deps} if ok else None


def package_hash(dir_: str) -> str:
    """content hash: SHA-256 over the package's module files in canonical path order (§8.7)"""
    files: list = []

    def walk(d: str) -> None:
        for e in sorted(os.listdir(d)):
            p = os.path.join(d, e)
            if e == "decl_modules":
                continue
            if os.path.isdir(p):
                walk(p)
            elif p.endswith(".decl"):
                files.append(p)
    walk(dir_)
    h = hashlib.sha256()
    for f in sorted(files):
        h.update(os.path.relpath(f, dir_).replace(os.sep, "/").encode("utf-8"))
        h.update(b"\0")
        with open(f, "rb") as fh:
            h.update(fh.read())
        h.update(b"\0")
    return h.hexdigest()


def find_package_root(from_file: str) -> Optional[str]:
    """the enclosing package root (the nearest ancestor with decl.toml)"""
    d = os.path.dirname(os.path.abspath(from_file))
    while True:
        if os.path.exists(os.path.join(d, "decl.toml")):
            return d
        up = os.path.dirname(d)
        if up == d:
            return None
        d = up


def open_package_universe(entry_file: str) -> Optional[dict]:
    diags: list = []

    def report(code: str, message: str) -> None:
        diags.append({"severity": "error", "code": code, "message": message, "path": ""})
    root_dir = find_package_root(entry_file)
    if root_dir is None:
        return None   # not in a package: relative imports only
    manifest = parse_manifest(os.path.join(root_dir, "decl.toml"), report)
    if not manifest:
        return {"root_dir": root_dir, "manifest": {"name": "?", "version": "0.0.0", "dependencies": {}},
                "packages": {}, "resolver": lambda spec, from_dir: {"code": "E3011", "message": "unusable manifest"},
                "diags": diags}

    # resolve the closed dependency set (flat decl_modules layout);
    # conflicting versions for one package are E3014 against both requirers
    packages: dict = {}
    required_by: dict = {}

    def visit(m: dict, by_dir: str) -> None:
        for dep, ver in m["dependencies"].items():
            prev = required_by.get(dep)
            if prev and prev["version"] != ver:
                report("E3014", f"package {dep} required at {prev['version']} (by {prev['by']}) and {ver} (by {m['name']})")
                continue
            required_by[dep] = {"version": ver, "by": m["name"]}
            if dep in packages:
                continue
            d = os.path.join(root_dir, "decl_modules", dep)
            dm = parse_manifest(os.path.join(d, "decl.toml"), report)
            if not dm:
                continue
            if dm["name"] != dep:
                report("E3013", f"package at {d} names itself {dm['name']}, expected {dep}")
            if dm["version"] != ver:
                report("E3016", f"package {dep}: manifest version {dm['version']} differs from required pin {ver}")
            packages[dep] = {"name": dep, "version": dm["version"], "dir": d, "hash": package_hash(d)}
            visit(dm, d)
    visit(manifest, root_dir)

    def resolver(spec: str, from_dir: str):
        slash = spec.find("/")
        pkg = spec if slash < 0 else spec[:slash]
        rest = "" if slash < 0 else spec[slash + 1:]
        # which package does the importing file belong to?
        from_pkg_dir = next((p["dir"] for p in packages.values() if os.path.abspath(from_dir).startswith(p["dir"])), root_dir)
        from_manifest = manifest if from_pkg_dir == root_dir else parse_manifest(os.path.join(from_pkg_dir, "decl.toml"), lambda c, m: None)
        if from_manifest is None or pkg not in from_manifest["dependencies"]:
            return {"code": "E3010", "message": f"package {pkg} not declared in [dependencies] of {from_manifest['name'] if from_manifest else '?'}"}
        p = packages.get(pkg)
        if not p:
            return {"code": "E3004", "message": f"package {pkg} could not be resolved"}
        return os.path.join(p["dir"], rest) if rest else p["dir"]
    return {"root_dir": root_dir, "manifest": manifest, "packages": packages, "resolver": resolver, "diags": diags}


# ---------------- decl.lock (§8.7) ----------------
def lock_text(u: dict) -> str:
    lines = [f"{p['name']} {p['version']} {p['hash']}" for p in sorted(u["packages"].values(), key=lambda p: p["name"])]
    return "\n".join(lines) + ("\n" if lines else "")


def write_lock(u: dict) -> str:
    path = os.path.join(u["root_dir"], "decl.lock")
    with open(path, "w", encoding="utf-8") as f:
        f.write(lock_text(u))
    return path


def verify_lock(u: dict) -> list:
    """fail-closed verification: missing entry, version drift, or hash
    mismatch stops resolution — never a silent re-resolve"""
    path = os.path.join(u["root_dir"], "decl.lock")
    if not os.path.exists(path):
        return []
    out: list = []

    def report(code: str, message: str) -> None:
        out.append({"severity": "error", "code": code, "message": message, "path": ""})
    locked: dict = {}
    with open(path, encoding="utf-8") as f:
        for line in f.read().split("\n"):
            if not line.strip():
                continue
            parts = line.strip().split()
            name = parts[0]
            locked[name] = {"version": parts[1] if len(parts) > 1 else None, "hash": parts[2] if len(parts) > 2 else None}
    for p in u["packages"].values():
        l = locked.get(p["name"])
        if not l:
            report("E3015", f"lock: missing entry for {p['name']}")
            continue
        if l["version"] != p["version"]:
            report("E3016", f"lock: {p['name']} version {l['version']} differs from manifest {p['version']}")
        elif l["hash"] != p["hash"]:
            report("E3017", f"lock: {p['name']} content-hash mismatch")
    return out
