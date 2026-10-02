# Using the semantic model

An executable description can be useful wherever another tool needs to
understand structured data. The same Decl model can supply resolved values,
relationships, and diagnostics to an application, a human reviewer, or an AI
agent. These are uses of Decl's current core, not additional language modes.

## Decl for AI agents

> Give agents a model of your data, not just its schema.

A shape description tells a consumer what fields may appear. A Decl model can
also explain which entity a reference identifies, which values are derived,
which relationships are valid, and how a failed constraint should be reported.

For example, an application can give an agent the
[Service Graph model](01_overview_by_example.md), accept a proposed JSON
document, and run the existing CLI:

```bash
decl evaluate topology.decl --input external=proposal.json --json
```

The application inspects the complete report's `ok` and `diagnostics`, together
with the command's exit status. Named diagnostic IDs and paths let it associate
a failed rule with the part of the proposal that needs review. A valid proposal
can then be emitted as a completed document:

```bash
decl evaluate topology.decl --input external=proposal.json --output external
```

Run full evaluation or validation for acceptance. A partial Session result
does not certify rules that have not been evaluated. A warning is also distinct
from an error; the application decides which severities its workflow accepts.

This workflow uses current input binding and machine-readable reports. It does
not claim that Decl already supplies an agent runtime, constrained decoding,
automatic repairs, or an ontology export. Network access, tool calls, human
approval, and applying a proposed change belong to the host application.

## A foundation for conceptual models

Types can describe categories of data. References connect instances.
Constraints express rules across them, while derived members calculate meaning
from the supplied facts. The sequence **type → relationship → constraint →
instance** can therefore be demonstrated with ordinary Decl modules.

That is a useful semantic foundation, but it is not a claim of OWL compatibility,
open-world reasoning, inference over arbitrary facts, or globally persistent
entity identity. Decl's references identify canonical places in a fixed
evaluation universe. Its [relationship rules](../specification/07_relationships.md)
remain the authority.

## Prove applications before expanding the core

Start with a concrete model, representative documents, and an expected report.
Show where the model expresses a real rule and where application code is still
needed. Add a graph or inference operation only when a repeatable example
demonstrates the missing capability.

The [Service Graph](01_overview_by_example.md) and
[hardware interconnect](../examples/01_interconnect.decl) are starting points.
The [examples index](../examples/README.md) explains their evidence and the
questions to ask of a comparison with another language.
