# Decl by Example

One scenario, walked end to end through the language's three
capabilities: **describe** a specification, **generate** values from
it, **validate** external data against it. This guide is informative —
exact rules live in the [specification](../specification/01_introduction.md);
each section links the chapters it leans on.

The scenario: a **service topology**. Services connect through links;
link endpoints must agree on a protocol; services carry defaults,
derived values, and soft limits. Note what the scenario does *not*
get: no `service` keyword, no `link` keyword — graphs, constraints,
and derivations are expressed with the general language only (P1).
The same method describes hardware interconnects, org charts, and
config trees.

## 1. Describe

### Types carry the value rules

```decl
// topology.decl

export type Protocol = "http" | "grpc" | "tcp"

export type ServiceName = /[a-z][a-z0-9-]*/
    else error `service names are lowercase kebab-case`

export type Port = 1..65535
```

- An enumeration is a union of literals; a string shape is a pattern;
  a numeric bound is a range type — no predicate machinery for the
  common cases ([03. Types](../specification/03_types.md)).
- A named type may attach its own error message with `else` — when a
  value fails `ServiceName`, *this* text is reported instead of a
  generic mismatch ([06. Constraints](../specification/06_constraints.md) §6.5).

### Schemas mix data, derivation, and constraint — flat

```decl
export type Service = {
    name: ServiceName                     // required
    protocol: Protocol
    port: Port = 8080                     // defaulted
    replicas: int = 1
    timeout: quantity<Time> = 250ms       // a physical quantity (SI catalog, std)
    description?: string                  // optional — absent is not null

    const endpoint = `${name}:${port}`    // derived: computed, input can't set it

    const inbound = $referrers(Link, "target")   // who links to me?

    assert scaled: replicas in 1..16
        else warn `replicas ${replicas} is outside the recommended range`
}
```

Four member kinds, distinguished by syntax alone
([05. Declarations](../specification/05_declarations.md)): required,
optional (`?`), defaulted (`= expr`), derived (`const`). The `assert`
is a member too — of the constraint kind: it produces diagnostics, not
data, and this one is a **warning**: a failing value is kept, annotated
([06. Constraints](../specification/06_constraints.md)).

`$referrers(Link, "target")` is the reverse query: "the `Link`
instances whose member `target` references me"
([07. Relationships](../specification/07_relationships.md) §7.6).

### Relationships and cross-entity rules

```decl
export diagnostic protocol_mismatch(src: string, dst: string) {
    severity = error
    message = `link endpoints use different protocols: ${src} vs ${dst}`
}

export type Link = {
    source: ref<Service>                  // reference, not a copy
    target: ref<Service>
    weight: int = 1

    assert no_self_link: source.name != target.name
    assert protocols: source.protocol == target.protocol
        else protocol_mismatch(source.protocol, target.protocol)
}

export type Topology = {
    services: Service[1..64]
    links: Link[]

    const service_count = std.array.count(services)

    assert unique_names:
        std.array.all_distinct([s.name for s in services])

    when service_count > 32 {
        assert dense: std.array.count(links) >= service_count
    }
}
```

- `ref<Service>` is a non-owning reference: links form a graph over
  services owned elsewhere; member access reads through it
  (`source.name`).
- The `diagnostic` declaration is a reusable, translatable template
  with a stable catalog id; the assert that references it keeps its
  own occurrence id (`topology.decl:Link.protocols`).
- `when` guards constraints that only apply conditionally.

### Layering, order-free

```decl
export type Hardened = { protocol: "grpc", ... }
export type Regional = { replicas: 2..16, ... }

export type ProdService = Service & Hardened & Regional
```

Independent policy layers conjoin with `&` — commutative, so a
security baseline and a regional rule need no artificial inheritance
order ([03. Types](../specification/03_types.md) §3.13).

## 2. Generate

```decl
export output demo: Topology = {
    services: [
        { name: `svc-${i}`, protocol: "grpc", port: 9000 + i }
        for i in 0..<3
    ]
    links: [
        { source: services[0], target: services[1] }
        { source: services[2], target: services[1], weight: 2 }
    ]
}
```

An `output` is a value this module produces; its literal runs the full
pipeline — type check, defaults, derived members, constraints
([05](../specification/05_declarations.md) §5.5,
[09](../specification/09_semantics.md)). Inside the literal, `services`
is a sibling reference, and in a `ref<Service>` position it denotes the
*reference*, not a copy. `decl eval` emits
([10. Interchange](../specification/10_interchange.md)):

```json
{
  "services": [
    { "name": "svc-0", "protocol": "grpc", "port": 9000,
      "replicas": 1, "timeout": { "value": 0.25, "unit": "s" },
      "endpoint": "svc-0:9000", "inbound": [] },
    { "name": "svc-1", "protocol": "grpc", "port": 9001,
      "replicas": 1, "timeout": { "value": 0.25, "unit": "s" },
      "endpoint": "svc-1:9001",
      "inbound": ["$.links[0]", "$.links[1]"] },
    { "name": "svc-2", "protocol": "grpc", "port": 9002,
      "replicas": 1, "timeout": { "value": 0.25, "unit": "s" },
      "endpoint": "svc-2:9002", "inbound": [] }
  ],
  "links": [
    { "source": "$.services[0]", "target": "$.services[1]", "weight": 1 },
    { "source": "$.services[2]", "target": "$.services[1]", "weight": 2 }
  ],
  "service_count": 3
}
```

Worth noticing:

- Defaults are filled (`replicas`, `weight`), derived members are
  computed and included (`endpoint`, `inbound`, `service_count`).
- The quantity normalized to its base unit: `250ms` became
  `{ "value": 0.25, "unit": "s" }`.
- References serialize as **document-relative paths**
  (`"$.services[1]"`) — the document never names its own root, so it
  can re-bind anywhere.
- The output is plain JSON. Bind it back to an `input Topology` and it
  validates and re-serializes byte-identically — round-trip is a
  normative property, not a hope
  ([10](../specification/10_interchange.md) §10.5).

## 3. Validate

```decl
export input external: Topology
```

```bash
decl validate topology.decl --input external=prod.json                  # diagnostics only
decl evaluate topology.decl --input external=prod.json --root external  # the completed document
```

`prod.json` — written by another team, never seen Decl:

```json
{
  "services": [
    { "name": "gateway", "protocol": "http" },
    { "name": "Auth", "protocol": "http", "replicas": 20 }
  ],
  "links": [
    { "source": "$.services[0]", "target": "$.services[1]" }
  ]
}
```

The same pipeline runs — same defaults, same derivations, same rules —
and reports:

```json
[
  { "code": "E4001", "id": "topology.decl:ServiceName",
    "severity": "error",
    "message": "service names are lowercase kebab-case",
    "path": "external.services[1].name" },
  { "code": "W6001", "id": "topology.decl:Service.scaled",
    "severity": "warn",
    "message": "replicas 20 is outside the recommended range",
    "path": "external.services[1]" }
]
```

Exactly two diagnostics — and what is *not* reported teaches the most
([06](../specification/06_constraints.md) §6.6,
[09](../specification/09_semantics.md) §9.7):

- `"Auth"` fails `ServiceName` — with the type's own message. That
  invalidates `name`; `endpoint` (derived from it) becomes invalid
  **silently**; `unique_names` (which reads every name) is skipped
  **silently**. One defect, one report.
- `replicas: 20` fires `scaled` as a **warning**: the value stays,
  validation continues, the evaluated document still contains
  `"replicas": 20`.
- Everything else — `gateway`, the link, the fills and derivations —
  evaluates normally. Partial validity is the normal result shape:
  the caller gets every valid value *and* the judgment.

## 4. Where to go next

- Precise rules: read the specification linearly,
  [01](../specification/01_introduction.md) →
  [13](../specification/13_stdlib.md).
- Why the language is shaped this way: the design docs —
  [vision](../design/00_vision.md),
  [requirements](../design/01_requirements.md),
  [decisions](../design/02_design_decisions.md).

---

- Index: [Documentation home](../README.md)
