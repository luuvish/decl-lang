# Validation Fixtures

The conformance corpus (ROADMAP Phase 1+). Layout:
`<feature>/valid/*.decl` must be accepted; `<feature>/invalid/*.decl`
carry `@expect-phase` / `@expect-error` (and optionally
`@expect-message`) comments naming the phase that must reject them and
the code from spec chapter 12.

**Phase 1 judges parsing only**: run `node tests/run_parsing.mjs` —
valid fixtures must parse clean, `@expect-phase: parsing` fixtures must
fail to parse, and later-phase fixtures are recorded and skipped (the
Phase 2 conformance runner picks them up).

## Statistics

| Feature | valid | invalid (parsing) | invalid (later phases) |
|---|---|---|---|
| lexical | 4 | 4 | 0 |
| types | 17 | 3 | 29 |
| expressions | 12 | 3 | 14 |
| declarations | 4 | 4 | 8 |
| constraints | 2 | 1 | 4 |
| stdlib | 7 | 0 | 4 |
| relationships | 1 | 0 | 0 |
| **total** | **47** | **15** | **59** |

Update this table in the same change whenever fixtures are added or
removed (AGENTS.md rule).

## Notes

- The parsing-phase invalid set doubles as the **rejected-syntax
  regression suite**: `let`, `->`, ternary, `where`, bare
  `import * from`, `export * from`, `;` each have a fixture proving the
  grammar refuses them.
- Later-phase fixtures (`checking`, `binding`) are authored now so the
  Phase 2 runner starts with expectations already in place.
- Valid fixtures are judged end to end: they must parse, check clean,
  AND evaluate their outputs without error-severity diagnostics — the
  `stdlib/` feature uses self-verifying asserts on this rule to cover
  every standard-library function (Phase 3 exit criterion).
