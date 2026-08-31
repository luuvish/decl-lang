# Decl Language

**Decl** is a general-purpose declarative language for describing, generating,
and validating structured data. The three capabilities are not separate modes
but three views of a single evaluation semantics — evaluation always produces
a pair of *(resolved values, diagnostics)*.

- **Describe** — declare types, schemas, constraints, and diagnostics in a
  form people can read and review.
- **Generate** — deterministically evaluate defaults, derived properties, and
  comprehensions into fully resolved value trees, exportable to standard
  formats such as JSON.
- **Validate** — check both language-defined values and externally supplied
  data against the same rules, producing diagnostics with stable ids.

Design goals:

- **JSON superset** — every JSON document is a valid Decl value; external
  data becomes a validation target without conversion.
- **Pure, deterministic, terminating** — no side effects and no recursion, so
  every evaluation terminates; the same input yields the same values and the
  same diagnostics regardless of implementation.
- **First-class diagnostics** — constraints and diagnostics are language
  constructs with stable ids, severities, and message templates.
- **No domain-specific features** — domain semantics (hardware, networking,
  configuration, …) are expressed at the library level with types, derived
  properties, functions, and constraints, never as language keywords.

## Status

**Specification v0.1 — frozen (2026-08-31).** Phase 0 is complete: the
full 13-chapter normative spec, its design charter (P1–P7, D1–D30), an
example-driven guide, three generality benchmark cases, and the
throwaway evaluator spike that gated the freeze by executing all of it
end to end (`spike/`, 31/31 checks green). Post-freeze changes follow
the revision process recorded in
[docs/REVISIONS.md](docs/REVISIONS.md).

Next: **Phase 1 — grammar & parser** (tree-sitter, written against
chapter 11). See [ROADMAP.md](ROADMAP.md) for the phase plan and exit
criteria.

## Documents

- [ROADMAP.md](ROADMAP.md) — development roadmap; owns the plan and progress
- `docs/` — language documentation (design docs, specification, guide);
  created as Phase 0 proceeds
