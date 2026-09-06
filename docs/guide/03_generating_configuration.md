# Generating Configuration

A tutorial on the second capability: producing a large, regular
document from a small description. The scenario is a cluster of cache
nodes laid out over three regions and two tiers; the page builds it
with a constructor function and a comprehension, then derives a second
cluster from the first with `with`. The ```decl blocks form one module,
`regions.decl`.

What the page teaches: functions and comprehensions generate values,
derived members summarize them, `with` layers a change over a value
without touching anything else, and the output is the same bytes on
every run ([09. Semantics](../specification/09_semantics.md)).

## 1. The shape of one node

```decl
export type Region = "us-east" | "eu-west" | "ap-south"
export type Tier = "edge" | "core"

export type Node = {
    name: /[a-z][a-z0-9-]*/
    region: Region
    tier: Tier
    weight?: 1..10 = 1
    capacity?: quantity<DataSize> = if tier == "edge" then 200GB else 2TB
    endpoint = `${name}.${region}.example.net`
}
```

- A default is an expression, and it may read its siblings: an edge
  node gets 200 GB unless told otherwise, a core node 2 TB
  ([05. Declarations](../specification/05_declarations.md) §5.7).
- `endpoint` is derived: the document never supplies it, the schema
  always computes it, and a string template does the formatting
  ([04. Expressions](../specification/04_expressions.md) §4.11).

## 2. The shape of the cluster

```decl
export type Cluster = {
    nodes: Node[1..64]
    node_count = std.array.count(nodes)
    names_by_region = { r: [n.name for n in nodes if n.region == r] for r in regions }
    total_capacity = std.array.fold(nodes, 0B, (a, n) => a + n.capacity)
    assert unique_names: std.array.all_distinct([n.name for n in nodes])
        else error `node names must be unique`
}
```

- `names_by_region` is a map comprehension: one key per region, each
  value an array comprehension with a filter
  ([04. Expressions](../specification/04_expressions.md) §4.8). Its
  type is inferred, `{ [string]: string[] }`.
- `total_capacity` folds quantities; the seed `0B` gives the fold its
  dimension.
- The rule is the only thing here that can fail, and it names what it
  checks.

## 3. Generating the grid

The parameter space is three constants and one function. The
annotations on the constants matter: `regions` must be `Region[]`, not
`string[]`, for `node` to accept its elements
([04. Expressions](../specification/04_expressions.md) §4.8).

```decl
const regions: Region[] = ["us-east", "eu-west", "ap-south"]
const tiers: Tier[] = ["edge", "core"]

func node(r: Region, t: Tier, i: int): Node = { name: `${t}-${r}-${i}`, region: r, tier: t }
```

A function is a pure expression of its parameters
([05. Declarations](../specification/05_declarations.md) §5.3): it
cannot recurse, read a file, or see anything but its arguments and the
module's constants, which is why calling it twelve times gives twelve
predictable nodes. The output is the comprehension over the product of
the three ranges:

```decl
export output grid: Cluster = {
    nodes: [node(r, t, i) for r in regions for t in tiers for i in 0..<2]
}
```

`decl evaluate regions.decl --output grid` prints twelve nodes in the
order the clauses nest — regions outermost, then tiers, then the
index — each with its default weight, its capacity by tier, and its
endpoint, followed by the derived members:

```json
{
  "nodes": [
    { "name": "edge-us-east-0", "region": "us-east", "tier": "edge", "weight": 1,
      "capacity": { "value": 1600000000000.0, "unit": "bit" },
      "endpoint": "edge-us-east-0.us-east.example.net" },
    { "name": "edge-us-east-1", "region": "us-east", "tier": "edge", "weight": 1,
      "capacity": { "value": 1600000000000.0, "unit": "bit" },
      "endpoint": "edge-us-east-1.us-east.example.net" },
    { "name": "core-us-east-0", "region": "us-east", "tier": "core", "weight": 1,
      "capacity": { "value": 16000000000000.0, "unit": "bit" },
      "endpoint": "core-us-east-0.us-east.example.net" },
    …
  ],
  "node_count": 12,
  "names_by_region": {
    "us-east": ["edge-us-east-0", "edge-us-east-1", "core-us-east-0", "core-us-east-1"],
    "eu-west": ["edge-eu-west-0", "edge-eu-west-1", "core-eu-west-0", "core-eu-west-1"],
    "ap-south": ["edge-ap-south-0", "edge-ap-south-1", "core-ap-south-0", "core-ap-south-1"]
  },
  "total_capacity": { "value": 105600000000000.0, "unit": "bit" }
}
```

Quantities print in the base unit of their dimension: 200 GB is
1.6 × 10¹² bit. Map keys keep the order the comprehension produced
them in ([10. Interchange](../specification/10_interchange.md)).

## 4. Deriving a second cluster

A production layout gives core nodes more weight. Rather than
generating again with different parameters, layer the change over the
value that exists:

```decl
export output weighted: Cluster = grid with {
    nodes: [n with { weight: if n.tier == "core" then 5 else 2 } for n in grid.nodes]
}
```

`with` updates exactly the members written and nothing else
([04. Expressions](../specification/04_expressions.md) §4.12): every
node keeps its name, region, tier, capacity, and endpoint, and only
`weight` changes. The derived members of `Cluster` are recomputed on
the result, so `weighted.node_count` and `weighted.total_capacity`
are right without being mentioned. One output referring to another is
ordinary: `grid` is a value of the module like any constant.

For layering that starts from a completed value with defaults, `with`
is the tool; `std.object.merge` is for combining open bags where
default completion does not interfere — the difference is spelled out
in [13. Standard library](../specification/13_stdlib.md) §13.7.

## 5. Sweeps

The same shape scales. Widen the index range and the grid grows;
add a fourth region to `regions` and every comprehension over it
follows; make `capacity` depend on the region as well as the tier and
every node picks it up. The test-fixture example in the repository
generates 32 cases of 16 packets each from a two-dimensional grid the
same way ([Test fixture generation](../examples/03_fixtures.decl)).

Two limits keep generation predictable
([09. Semantics](../specification/09_semantics.md)): a comprehension
iterates a finite array or range, and a function cannot call itself,
so every module terminates, and the same module prints the same bytes
under every implementation.

## Where to go next

- Checking what you generated, or what someone else did:
  [Validating documents](02_validating_documents.md).
- The arithmetic of the quantities used here:
  [Quantities and units](04_quantities_and_units.md).
- Splitting a description across files:
  [Modules and packages](05_modules_and_packages.md).

---

- Index: [Documentation home](../README.md)
