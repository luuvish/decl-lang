# Why Decl

**Describe your data.**

Decl is a **semantic modeling language for structured data**. Describe the
values your data holds, how values are derived, which entities are related,
what must be true, and what a failure means. Types, values, relationships,
constraints, and diagnostics belong to the same executable model.

That model is a single source of truth for describing data, generating resolved
values, and validating documents supplied from elsewhere. Evaluation produces
**resolved values and diagnostics** together. A configuration file is one useful
application; the model can also describe a service topology, a hardware
interconnect, or a collection of related records.

## Trees and relationships

> Trees describe structure. Graphs describe relationships. Decl models both.

JSON and YAML documents have a tree structure. The systems they describe often
have relationships across that tree: a service calls another service, a link
connects two ports, a deployment refers to a shared resource.

Decl keeps the ownership tree and adds typed references over it. A `ref<T>`
points to a value at a named place. It is checked for type compatibility and
existence, serializes as a canonical path, and participates in reverse queries
through `$referrers`. Ordinary expressions can read the target's members and
constraints can relate the two ends of a connection.

The [Service Graph walkthrough](01_overview_by_example.md) models links, derives
endpoints, and reports incompatible protocols. The
[hardware interconnect](../examples/01_interconnect.decl) uses the same core for
ports, propagated widths, connection locality, and diagnostic messages. Neither
domain needs its own language keywords.

## Content and identity

> Values describe content. References preserve identity.

Why does immutable data need references? **Not for mutation. For identity and
relationships.** Two services with identical settings can still occupy different
places in the model. A link to one is different from a link to the other.

Ordinary values compare by content; references compare by canonical target
path. Identity here is a place within the evaluation universe, not a permanent
ID that survives moving a value to another key or index. The
[identity tutorial](06_identity_and_relationships.md) proves the distinction
with two equal-valued services and a link between them.

## One model, three uses

| Describe | Generate | Validate |
|---|---|---|
| State types, relationships, defaults, derivations, constraints, and diagnostic messages. | Evaluate a model into resolved values, then render the required documents. | Bind external data to the same types and apply the same defaults, derivations, and rules. |

An `input` and an `output` use the same evaluation semantics. The rules for data
you receive stay with the rules for data you generate. Diagnostics carry stable
codes and declaration IDs, severities, messages, and paths. Invalid dependencies
are tainted so downstream failures do not obscure the original cause.
See [validating documents](02_validating_documents.md).

The language's type relation is subsumption, written `v ⊑ T`: a value is allowed
by a type. [The type system](../specification/03_types.md) and
[evaluation semantics](../specification/09_semantics.md) define the exact rules;
this guide explains their use.

## More semantics, not more programming

Decl's core is pure, deterministic, and terminating. It has immutable values,
bounded comprehensions, derived members, and constraints. It does not add
network requests, mutable state, async execution, or unrestricted recursion to
the data model. The CLI and host load inputs and write outputs around that core.

Rendering is delivered tooling: `@render` can produce JSON, YAML, or text from a
template. [The renderer](../tooling/05_render.md) explains the supported forms.
Reference graphs do not imply a general graph query engine; `std.graph` is
reserved and has no current API. Additions should follow demonstrated needs
in real models.

## Choose by the problem you can express

Use Decl when relationships, derived meaning, and checks across entities belong
in the same model as the data's shape. An existing schema may already be enough
when all you need is shape validation. General application behavior belongs in
the surrounding program.

Compare models by solving the same problem: **type + relationship + derivation + constraint + diagnostic**. The [examples index](../examples/README.md)
identifies what the existing Decl models prove and sets out a reproducible
comparison with CUE, Pkl, and Nickel. It does not treat an unimplemented
comparison as a result.

[AI agents and conceptual models](07_using_the_model.md) are applications of
this semantic model. Their value should be demonstrated using the existing core.

## Start with a working model

1. [Decl by example](01_overview_by_example.md): a Service Graph from description
   through output and validation.
2. [Identity and relationships](06_identity_and_relationships.md): equal content,
   distinct entities, and references between them.
3. [Hardware interconnect](../examples/01_interconnect.decl): a larger relationship
   model with propagated widths and cross-entity constraints.

Then explore [generation](03_generating_configuration.md),
[quantities](04_quantities_and_units.md), and [modules](05_modules_and_packages.md).
