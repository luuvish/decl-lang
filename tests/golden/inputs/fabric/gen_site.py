"""A synthetic spine-leaf site for examples/fabric/fabric.decl, generated
deterministically: S spines, L leaves with four host ports each, one link
per spine-leaf pair. `site_2x4.json` beside this file is the 2x4 site
(the generator must reproduce it byte for byte — the corpus checks); the
parity harness generates a larger site as a scale row.

    python tests/golden/inputs/fabric/gen_site.py 10 20 > site.json
"""

from __future__ import annotations

import json
import sys
from typing import Any


def eth(name: str, direction: str, gbps: int, vlan: int | None = None) -> dict[str, Any]:
    params: dict[str, Any] = {"speed": {"value": gbps * 10**9, "unit": "bps"}, "mtu": 9000}
    if vlan:
        params["vlan"] = vlan
    return {"kind": "fabric.port.eth", "name": name, "dir": direction, "params": params}


def gen_site(spines: int, leafs: int) -> dict[str, Any]:
    nodes: dict[str, Any] = {}
    edges: dict[str, Any] = {}
    for s in range(spines):
        ports = {f"dn{l}": eth(f"dn{l}", "down", 100) for l in range(leafs)}
        nodes[f"spine{s}"] = {"kind": "fabric.node.switch.spine", "name": f"spine{s}", "ports": ports}
    for l in range(leafs):
        ports = {f"up{s}": eth(f"up{s}", "up", 100) for s in range(spines)}
        for h in range(4):
            ports[f"host{h}"] = eth(f"host{h}", "down", 25, 100 + (l % 5))
        nodes[f"leaf{l}"] = {
            "kind": "fabric.node.switch.leaf",
            "name": f"leaf{l}",
            "ports": ports,
            "nodes": {f"rack{l}": {"kind": "fabric.node.rack", "name": f"rack{l}"}},
        }
    for s in range(spines):
        for l in range(leafs):
            n = f"sl{s}x{l}"
            edges[n] = {"kind": "fabric.edge.link", "name": n, "endpoints": [f"spine{s}", f"leaf{l}"]}
    return {
        "kind": "fabric.node.site",
        "name": "site_a",
        "params": {
            "uplink": "wan0",
            "oversubscription": 4,
            "subnets": [
                {"cidr": "10.0.0.0/16", "vlan": 100, "gateway": "wan0"},
                {"cidr": "10.1.0.0/16", "vlan": 101},
                {"cidr": "172.16.0.0/12", "vlan": 200},
            ],
        },
        "ports": {"wan0": eth("wan0", "up", 400)},
        "nodes": nodes,
        "edges": edges,
    }


def site_text(spines: int, leafs: int) -> str:
    return json.dumps(gen_site(spines, leafs), indent=2) + "\n"


if __name__ == "__main__":
    spines, leafs = (int(a) for a in sys.argv[1:3])
    sys.stdout.write(site_text(spines, leafs))
