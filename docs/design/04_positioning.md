# Describe your data

Status: informative positioning and documentation direction, 2026-10-02.
This document explains the existing language. It introduces no syntax,
semantics, standard-library operation, or change to the normative specification.

## The promise

**Describe your data.**

Decl is a **semantic modeling language for structured data**: an executable
description of types, values, derived meaning, relationships, constraints, and
diagnostics. That description is one source of truth for generation and
validation. Configuration is an application of this model.

## Four statements, with evidence

| Message | Meaning | Proof or boundary |
|---|---|---|
| Describe your data. | Describe content and the rules that give it meaning in one language. | The [Service Graph walkthrough](../guide/01_overview_by_example.md) includes types, derivation, links, constraints, and diagnostics. |
| Trees describe structure. Graphs describe relationships. Decl models both. | Composition forms the ownership tree; typed references add relationships across it. | [Relationships §7](../specification/07_relationships.md); the [hardware interconnect](../examples/01_interconnect.decl). |
| Values describe content. References preserve identity. | Equal content does not make two referenced places the same entity. | The [identity tutorial](../guide/06_identity_and_relationships.md). Identity is canonical-place identity within an evaluation universe, not a durable database ID. |
| More semantics, not more programming. | Keep a small, pure, deterministic core focused on data meaning. | The host owns I/O and application workflows. `std.graph` has no current API; new operations require concrete evidence. |

## The documentation journey

The homepage introduces the promise, shows a runnable Service Graph and its
actual output, explains the need for identity in immutable data, and leads to
the more demanding hardware case. The first action, "See the model below", leads
to the example on the same page. After the example, the playground is the primary
action and the Service Graph guide is a quieter link. Installation follows the
explanation, under "Use Decl in your project". Each action appears when the reader
has the context to choose it. The guide teaches the model before cataloguing
features; the specification continues to own exact rules.

The opening contains one definition rather than several introductions that
repeat it. Short section headings help readers scan; the four positioning
statements retain their exact wording in the surrounding explanation. The
header carries the mark, so the opening can begin directly with the promise.
This arrangement addresses the early three-way choice observed in the homepage
review; it is a design judgment, not a measured conversion result.

The opening illustration makes the tree and graph statement visible: a paper
`demo` branches into `services` and `links`, while blue reference threads cross
those branches to point to the existing service entities. Attached tabs show
derived endpoints, and a checked relationship shows the self-link constraint.
The illustration uses the same values and targets as the runnable example;
it is a conceptual view, not a second semantic definition. The headline,
caption, and links stay selectable text, with a textual description for the
image and enlargement for small screens.

The existing Graphite identity stays: Literata for reading, IBM Plex for the
interface and code, restrained color, square corners, and thin rules. The
paper artwork adds physical texture and shadow to the opening. Code and
evaluated output continue to carry the argument. A feature count or an unsupported
comparison matrix does not substitute for a working example.

## Applications and comparisons

**Give agents a model of your data, not just its schema.** Keep "Decl for AI
agents" an application of the semantic model, with current CLI capabilities
separated from possible future integrations. Treat conceptual modeling the
same way; the core need not become an ontology or agent language.

Compare CUE, Pkl, and Nickel on the same Service Graph and interconnect problem,
with the same valid and invalid documents. Record versions, runnable commands,
resolved results, and diagnostics. Evaluate how naturally each model expresses
types, relationships, derivations, constraints, and diagnostics, including any
host code required. Publish conclusions after those implementations are run.

The immediate design priority is to discover and demonstrate what the existing
core means. Real cases should reveal both its distinctive strengths and its
actual missing capabilities.

## Keep the claims executable

The homepage's Service Graph and the identity tutorial are entries in the
[shared golden manifest](../../tests/golden/manifest.json). TypeScript, Rust,
and Python compare their evaluated output against independently specified
expected results. The site generates its displayed output from those same
sources and refuses to build if the identity example stops distinguishing
equal content from different targets.

Writing that example exposed a shared evaluator defect: binary equality
materialized reference operands, dereferencing them before comparing their
targets. The three engines now preserve reference operands for identity
comparison and materialize ordinary operands for structural equality, as
required by existing §4.5. This corrects an implementation defect; it does not
revise the specification. Shared relationship fixtures cover equal-valued
distinct targets, repeated references to one target, reference-to-place
comparisons in both directions, and rejection of a self-link.

For subsequent changes, run `make verify`, `make lint`, and `make site`.
Inspect the homepage at desktop and mobile widths, follow the model links,
and run both examples in the browser playground. A wording change that makes
a new semantic claim needs an executable example or an exact specification
reference before it becomes homepage copy.
