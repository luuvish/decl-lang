"""Locate a Node.js runtime and run the bundled decl JavaScript with it.

The Python package ships the same bytes as the npm package (``_js/``:
``cli.js``, ``lsp.js``, ``index.js`` and the two wasm files). Node is
found, in order, from ``$DECL_NODE``, the ``nodejs-wheel-binaries``
package (``pip install 'decl[node]'``), or ``node`` on ``PATH``.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Sequence

JS_DIR = Path(__file__).resolve().parent / "_js"
MIN_NODE_MAJOR = 20


class NodeNotFound(RuntimeError):
    """No usable Node.js runtime could be located."""


def _from_wheel() -> str | None:
    try:
        import nodejs_wheel  # type: ignore[import-not-found]
    except ImportError:
        return None
    base = Path(nodejs_wheel.__file__).resolve().parent
    for cand in (base / "bin" / "node", base / "node.exe", base / "node"):
        if cand.exists():
            return str(cand)
    return None


def node_executable() -> str:
    """Return the path of a Node.js >= 20 executable or raise NodeNotFound."""
    explicit = os.environ.get("DECL_NODE")
    if explicit:
        return explicit
    for cand in (_from_wheel(), shutil.which("node")):
        if cand:
            return cand
    raise NodeNotFound(
        "decl needs Node.js >= %d: install it from https://nodejs.org, "
        "or run `pip install 'decl[node]'` to get one through pip, "
        "or point $DECL_NODE at a node executable." % MIN_NODE_MAJOR
    )


def check_node_version(node: str) -> None:
    out = subprocess.run([node, "--version"], capture_output=True, text=True, check=False).stdout.strip()
    try:
        major = int(out.lstrip("v").split(".")[0])
    except ValueError:
        return
    if major < MIN_NODE_MAJOR:
        raise NodeNotFound(f"decl needs Node.js >= {MIN_NODE_MAJOR}, found {out} at {node}")


def run(script: str, args: Sequence[str], *, capture: bool = False) -> subprocess.CompletedProcess:
    """Run ``_js/<script>`` under Node with ``args``; stdio passes through unless captured."""
    node = node_executable()
    check_node_version(node)
    cmd = [node, str(JS_DIR / script), *args]
    if capture:
        return subprocess.run(cmd, capture_output=True, text=True, check=False)
    return subprocess.run(cmd, check=False, stdin=sys.stdin, stdout=sys.stdout, stderr=sys.stderr)
