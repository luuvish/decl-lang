"""Same workload as the TypeScript/Rust drivers, via the public Session API."""

import importlib
import json
import os
import statistics
import sys
import time
from pathlib import Path

repo = Path(os.environ.get("DECL_BENCH_REPO", ".")).resolve()
sys.path.insert(0, str(repo / "decl-py/src"))

Session = importlib.import_module("decl.session").Session

config = json.loads(
    Path(sys.argv[1] if len(sys.argv) > 1 else "tests/benchmarks/session.json").read_text()
)
rows = []
for size in config["sizes"]:
    for equal in (True, False):
        entry = str(repo / "tests/benchmarks/session_case.decl")
        session = Session(entry, {entry: config["source"]})
        session.apply(
            {
                "op": "bind",
                "name": "batch",
                "src": {"kind": "inline", "text": json.dumps({"items": [{"n": 1}] * size})},
            }
        )
        first = session.run()
        assert first.eng is not None and not first.diags
        times = []
        for i in range(config["warmups"] + config["samples"]):
            n = 2 + i % 2 if equal else 1 if i % 2 else -1
            start = time.perf_counter()
            session.apply(
                {"op": "edit", "kind": "update", "path": "batch.items[0].n", "expr": str(n)}
            )
            run = session.run()
            value = run.eng.serialize(run.entry.env.roots["total"], "total")
            elapsed = (time.perf_counter() - start) * 1000
            assert not run.diags and value == str(7 * (size - (1 if n < 0 else 0)))
            if i >= config["warmups"]:
                times.append(elapsed)
        rows.append(
            {
                "size": size,
                "equal": equal,
                "samples_ms": times,
                "median_ms": statistics.median(times),
            }
        )
print(json.dumps(rows))
