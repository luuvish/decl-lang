# decl-impl

The TypeScript reference implementation (ROADMAP Phase 2). Consumes the
canonical tree-sitter parser through `web-tree-sitter` and the committed
`tree-sitter-decl.wasm` — no native build step.

## Pipeline

```
tree-sitter CST  ->  AST (src/parse.ts lowering)
                 ->  static checks (src/checker.ts, growing)
                 ->  bind / evaluate / validate (src/engine.ts + semantics.ts,
                     promoted from the Phase 0 spike, proven on the corpus)
                 ->  canonical serialization / round-trip
```

## Commands

```bash
npm install
node test/e2e.ts          # 3 benchmark cases + guide, end to end (26 checks)
node test/subsume.ts      # subsumption ⊑ / emptiness unit tests (48 checks)
node src/conformance.ts   # judge every tests/validation fixture by phase
```

## Status

Working: full dynamic pipeline — binding (closedness, restatement,
discrimination, quantity/reference interchange forms, predicate types,
non-record intersections), lazy evaluation with dependency cycles and
`$referrers` universe ordering, taint / root-cause diagnostics with
stable ids and `(path, id)` order, `with`, comprehensions, `func`
declarations as first-class closures, canonical serialization with
byte-identical round-trips under renamed roots.

Static checker (`src/checker.ts` on `src/subsume.ts`, the §3.17
subsumption judgment with coinductive records and §3.19 structural
emptiness): duplicate names E3001, unknown types E3003, mixed range
endpoints E4010, empty ranges/sizes E4011, empty intersections E4012,
union discriminability E4013/E4014, map key shape E4015, inheritance
narrowing E4030/E4032, nullish-mix E4052, and D30 context obligations
E4094 (with the lexical-nesting exemption). Generic declarations are
checked at instantiation (§3.15).

Growing: expression-level type inference / static assignability, the
§4.10 absence discipline, `match` evaluation, and modules/`decl.toml`
(Phase 3).
