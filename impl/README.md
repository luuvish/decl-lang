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
node src/conformance.ts   # judge every tests/validation fixture by phase
```

## Status

Working: full dynamic pipeline — binding (closedness, restatement,
discrimination, quantity/reference interchange forms), lazy evaluation
with dependency cycles and `$referrers` universe ordering, taint /
root-cause diagnostics with stable ids and `(path, id)` order, `with`,
comprehensions, canonical serialization with byte-identical round-trips
under renamed roots.

Growing: the full static checker of chapters 3–4 (subsumption `⊑`,
assignability, absence discipline, D30 context obligations,
discriminability) — currently mixed-range (E4010) and nullish-mix
(E4052) are static; everything else is enforced dynamically. `match`
evaluation, `func` declarations, and modules/`decl.toml` (Phase 3) are
not yet implemented.
