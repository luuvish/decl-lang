# Identity and relationships

**Values describe content. References preserve identity.**

Two services can have identical settings and still be different services. A
connection must name which service it reaches. Copying that service's settings
does not express the same relationship.

## Why does immutable data need references?

**Not for mutation. For identity and relationships.**

Composition gives a value an owner and a place in a tree. A `ref<T>` identifies
a place in that tree without copying its contents. Decl checks the target's
type and existence, and member access can read through the reference.

This complete example deliberately gives two services the same content:

```decl
type Service = { port: 1024..65535 }

type Link = {
    source: ref<Service>
    target: ref<Service>

    assert different_services: source != target
        else error `a service must not link to itself`
}

export output graph: { services: Service[2], links: Link[] } = {
    services: [{ port: 8080 }, { port: 8080 }]
    links: [{ source: services[0], target: services[1] }]
}

export output comparison: {
    same_content: bool
    same_entity: bool
    target_port: int
    target_path: string
} = {
    same_content: graph.services[0] == graph.services[1]
    same_entity: graph.links[0].source == graph.links[0].target
    target_port: graph.links[0].target.port
    target_path: std.ref.path(graph.links[0].target)
}
```

The `comparison` output is:

```json
{
  "same_content": true,
  "same_entity": false,
  "target_port": 8080,
  "target_path": "graph.services[1]"
}
```

The first comparison reads two ordinary values and compares their content.
The second compares references and asks whether they designate the same place.
The `different_services` constraint therefore allows this link. Changing its
target to `services[0]` produces an error at `graph.links[0]` even though the
two original services have equal values.

## A tree of values, a graph of relationships

**Trees describe structure. Graphs describe relationships. Decl models both.**

The `services` and `links` arrays are owned by `graph`. Each service and link
has one place in that tree. The link's endpoints form graph edges between
those places. References serialize as paths rather than duplicated objects:

```json
{
  "services": [{ "port": 8080 }, { "port": 8080 }],
  "links": [{ "source": "$.services[0]", "target": "$.services[1]" }]
}
```

Here `$` means the root of the serialized document. Reference identity uses
the absolute canonical target path internally. It is **identity within an
evaluation universe**, not a globally assigned entity ID: moving a value to a
different key, index, or root changes its place. A model that needs a business
identifier across document versions can declare one as data and constrain it.
Decl does not infer that two entities are the same from equal content.

## Integrity, reverse queries, and rules across entities

Three operations follow from the reference model:

| Question | Decl mechanism | Existing example |
|---|---|---|
| Does this endpoint exist and have the right type? | A `ref<Service>` position checks the target; a missing target is a dangling-reference diagnostic. | The link above |
| Who points to this service? | `$referrers(Link, "target")` returns references to the owning Link instances. | [Service topology](01_overview_by_example.md) |
| Do connected entities satisfy a shared rule? | A constraint reads through `source` and `target`. | Protocol agreement in the service topology; width agreement in the [hardware interconnect](../examples/01_interconnect.decl) |

Reverse queries name a declared referrer type and its reference-bearing member.
They are not arbitrary graph traversal. Their result is distinct referring
instances in canonical path order; the [relationship specification](../specification/07_relationships.md)
defines collection traversal, the evaluation universe, and the treatment of
invalid references.

Reference cycles are permitted. Cyclic value dependencies are a different
matter: a model must still satisfy the evaluation rules. A reference does not
introduce mutation, I/O, or unrestricted recursive computation.

## Try a larger model

- [Decl by example](01_overview_by_example.md) combines typed links, reverse
  references, derived endpoints, protocol constraints, and diagnostics.
- [Modules and packages](05_modules_and_packages.md) develops the Service Graph
  across schema, topology, and deployment modules.
- [Hardware interconnect](../examples/01_interconnect.decl) uses the same
  mechanisms for ports, connections, propagated widths, and locality rules.

Exact rules: [equality](../specification/04_expressions.md) §4.5 and
[relationships](../specification/07_relationships.md) §§7.1–7.6.
