# Models and examples

**Prove the model with a real problem.** These examples put types, relationships,
derivations, constraints, and diagnostics in the same Decl description. Start
with the Service Graph, then explore the Hardware Interconnect.

| Model | What to look for | Run or read |
|---|---|---|
| Service Graph | Typed endpoints, reverse references, derived values, protocol rules, and diagnostics on external documents. | [Walkthrough](../guide/01_overview_by_example.md); [multi-module source](../../examples/svcgraph/main.decl) |
| Identity | Two equal-valued services remain distinct reference targets. | [Runnable tutorial](../guide/06_identity_and_relationships.md) |
| Hardware Interconnect | A 2×2 crossbar, width propagation through reverse references, flow and locality constraints, and reusable diagnostics. | [Source and output](01_interconnect.decl) |
| Configuration | Defaults, optional values, quantities, policy layers, and external validation. | [API/config schema](02_config.decl) |
| Test generation | Comprehension sweeps and constraints over generated cases. | [Fixture generation](03_fixtures.decl) |

```bash
decl evaluate examples/svcgraph/main.decl
decl evaluate docs/examples/01_interconnect.decl
```

The [shared golden corpus](../../tests/golden/README.md) records expected output
for these examples and the assembled tutorials. The three implementations replay
it. The site evaluates its displayed example outputs during the build.

## Compare the same problem

The useful question for CUE, Pkl, Nickel, and Decl is how naturally each expresses
**type + relationship + derivation + constraint + diagnostic** for the same
model. Line count and a checklist of feature names do not establish that.

A reproducible comparison should include:

1. The same service topology or interconnect, with equal-valued but distinct
   entities and valid typed connections.
2. The same changes: a dangling endpoint, an incompatible protocol or width,
   and a self-link. State the required meaning before implementing each model.
3. Runnable source, pinned tool versions, commands, resolved outputs, and
   failure reports. Include any external code needed to preserve identities,
   derive reverse relationships, or check a rule.
4. A discussion of where each model is natural, where conventions are needed,
   and what the implementation cannot express. Separate modeling results from
   separately controlled performance measurements.

This repository currently supplies the Decl cases. Matched CUE, Pkl, and Nickel
implementations and their results are not supplied by this documentation change;
no comparative superiority claim follows from the Decl examples alone.

## Original validation cases (ROADMAP §0.5)

The original generality benchmark: domain specs written with **no domain
keywords**, desk-checked against the specification alone — every point
where writing them stalls is a specification defect by definition
(requirements §4). These files are review artifacts of Phase 0; in
Phase 1 they seed the test corpus.

| Case | File | Status |
|---|---|---|
| 1. Hardware interconnect | [01_interconnect.decl](01_interconnect.decl) | written, desk-checked — ports a real 2x2 crossbar fixture |
| 2. API/config schema | [02_config.decl](02_config.decl) | written, desk-checked |
| 3. Test fixture generation | [03_fixtures.decl](03_fixtures.decl) | written, desk-checked |

## Feature coverage

- **Case 1**: hierarchical node graph as records + maps (recursive
  types), edges as `ref<Port>` records instead of parsed strings,
  **connection-based width propagation as one derived member over
  `$referrers`** (replacing the legacy fixture's ~20 `@readonly`
  lambdas), internal wiring as derived `Edge[]` members, an `&`
  constraint layer (`EdgeHost`) shared by both container roles,
  owner-dependent flow rules via place equality, locality via
  `std.ref.path` + `starts_with`, address-map destination validity via
  map-key membership, fan-in/out counts, discriminated node unions.
- **Case 2**: open records with opaque pass-through, defaults at every
  member, optional-vs-null (`cert_path?`), conditional constraints
  (`when` + `in` presence guard), quantities with defaults,
  pattern-keyed maps, environment layering with `with` (including
  nested `with` for depth), outputs as shared baselines, external
  validation of the same schema.
- **Case 3**: comprehension-parameterized generation (a 4×8 sweep of
  32 cases × 16 packets), range-iteration typing, literal-union
  element types with the annotation they require, derived aggregation
  (`std.array.sum` over a projection), uniqueness asserts,
  conditional expression in generation.

## Desk-check findings

Defects found by writing these cases (each already fixed in the spec —
the point of the exercise):

1. **Comprehension variables had no typing rule** — `for p in 0..<8`
   inferring `p: int` would make `priority: p` fail statically against
   `0..7`. Fixed: the iteration variable of a range takes the range
   itself as its type (spec §4.8).
2. **`std.object.merge` cannot layer schemas with defaults** — the
   patch argument is an evaluated value, so its filled defaults win
   over the base's explicit values. Fixed by documentation, not
   mechanism: the defaults trap is now stated in spec §13.7, and
   layering uses `with` (as case 2 demonstrates).
3. **Sibling references in typed construction were unstated** — case
   drafting (and the guide before it) relied on `source: services[0]`
   inside an output literal; the rule is now explicit in spec §4.2.

4. **`ref` positions collided with the maybe-absent discipline** —
   `source: ports["si0"]` (map access) would have been a compile error,
   making map-keyed ports unreferenceable. Fixed: a navigation in a
   `ref<T>` position denotes a place regardless of maybe-absent steps;
   whether the target exists is the dangling-reference check, not
   absence discipline (spec §4.10, §7.4).
5. **No narrowing through `&&`** — the own-port idiom
   `k in ports && ports[k] == e.source` was ill-typed. Fixed: narrowing
   flows through `&&` (and negated through `||`) (spec §4.10).
6. **Union common-member access was unstated** — `e.source.width` on
   `ref<ExtPort | IntPort>` needed a rule. Fixed: access is legal when
   every arm declares the member, typing as the union of member types
   (spec §4.3).
7. **Reference targets' paths were unobservable** — locality
   constraints ("edge endpoints lie inside me") were inexpressible.
   Fixed: `std.ref.path` added, with `std.string.starts_with`
   (spec §13.6, §13.9) — additions justified by the §0.3 scope rule
   (the corpus now uses them).

**OQ3 evidence from case 1**: every constraint the legacy fixture
family actually enforces — widths, flow direction, locality, fan
counts, destination validity — is **one-hop**; no transitive closure
was needed, and the legacy fixture never checked reachability either
(it could not). Source-to-sink reachability remains a plausible want
for richer versions; the spike should still measure it, but the ported
corpus alone does not justify `std.graph.*`.

**Ergonomics observations** (not defects; recorded for the spike):
overriding one sub-member of a defaulted record member requires
restating the whole member (no deep default merge — related to the
§13.7 merge trap); the own-port test reads heavily
(`any(keys(...), (k) => k in ... && ...)`) — a containment or
same-owner predicate may deserve stdlib entry if the spike confirms
the pain.
