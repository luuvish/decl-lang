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
node test/e2e.ts          # benchmarks + guide + match + generics + quantities (49 checks)
node test/subsume.ts      # subsumption ⊑ / emptiness unit tests (52 checks)
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

Expression-level analysis (`src/infer.ts`): type inference with
bidirectional literal checking (§3.18), unknown names E3003, shadowing
E3019, static assignability E4001/E4002/E4003, absence discipline
E4050/E4051/E4054 with the two narrowing rules (`in` guards, `&&`/`||`
flow, `!= null`), call arity E4062, operator kinds E4071, `with` misuse
E4080, and the `match` static checks E4100–E4103. `match` and `|>` also
evaluate (engine). Constant positions (§4.13): named range endpoints
and array sizes evaluate at elaboration time and participate in
subsumption/emptiness with their values; non-constant references
(inputs, outputs, context variables, `$referrers`) in endpoints or
predicate arguments are E4021, and an erroring constant surfaces as a
compile-time E5xxx diagnostic. Quantities (§3.16): dimensions resolve
to base-exponent vectors (abelian group — `Speed = Length / Time` and
`Length * Time ^ -1` are the same type); `unit`/`dimension`
declarations load into their own name spaces with the std SI subset
seeded (D15); `+`/`-`/comparison need equal dimensions (E4072), `*`
and `/` compose vectors (a cancelled vector is a plain number), and
unknown units, wrong-dimension interchange forms, redeclarations, and
second base units are E4073 — statically for unit literals and the
unit space, at binding for documents. Generics (§3.15): instantiation
substitutes type and value parameters through the declaration body
(types, sizes, endpoints, member expressions), checks value arguments
against their parameter types (D14 — the type is the constraint,
E4021/E4022/E4023), and memoizes per argument list so recursive
generic records stay coinductive; after substitution typing is fully
structural. Inference is conservative — an undetermined form
types as unknown and suppresses downstream judgments. Two deliberate
precision choices keep the frozen corpus sound under strict `S ⊑ T`:
interval arithmetic on int ranges (`9000 + i` with `i: 0..<3` stays
`9000..9002`), and same-kind refinement targets (pattern / range /
literal-set) defer unprovable membership to binding instead of failing
statically — the guide and benchmarks rely on both (candidate spec
clarifications for §3.18/§4.4).

Growing: modules/`decl.toml` and the std library as real declarations
(Phase 3).
