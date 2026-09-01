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
node test/modules.ts      # multi-module linking + evaluation (§8)
node test/packages.ts     # packages, decl.toml, lock reproducibility (§8.6–8.7)
node src/conformance.ts   # judge every tests/validation fixture by phase
node test/fmt.ts          # formatter idempotency + AST safety over the corpus
node test/cli.ts          # decl check / evaluate / validate / fmt end to end
node test/lsp.ts          # LSP diagnostics / hover / definition over stdio
node test/domainlibs.ts   # Phase 5 domain examples (examples/svcgraph, examples/testgen)
node test/fabric.ts       # synthetic fabric documents against examples/fabric
npm test                  # all of the above
```

## CLI

```bash
node src/cli.ts check <files...>                      # parse + static checks (module-aware)
node src/cli.ts evaluate <file> [--root <name>]       # evaluate outputs -> JSON
node src/cli.ts validate <dir>                        # judge a fixture corpus (@expect-*)
node src/cli.ts validate <file> --input n=doc.json --expect-errors E4001
node src/cli.ts fmt <files...> [--check]              # canonical formatting, idempotent
```

`decl validate tests/validation` judges the full fixture corpus (the
Phase 4 exit criterion). The formatter preserves the author's line
structure (§2.9 makes newlines separators), re-derives 4-space
indentation and token spacing, and is verified idempotent and
AST-preserving over every parseable corpus file.

## LSP

`src/lsp.ts` is a stdio language server: publishDiagnostics on
open/change (syntax errors positioned exactly; checker diagnostics
anchored to the name they mention), hover (declaration kind + source
line), and definition — both following named, renamed, and namespace
imports one hop, with open buffers overriding the disk. Point any LSP
client at `node impl/src/lsp.ts` for `.decl` files, e.g. in VS Code
via a generic LSP client extension, or in Neovim:

```lua
vim.lsp.start({ name = 'decl', cmd = { 'node', '<repo>/impl/src/lsp.ts' }, filetypes = { 'decl' } })
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

Standard library (§13, complete 1:1): every listed function of
`std.array` / `math` / `int` / `float` / `string` / `object` / `map` /
`ref` is implemented with its documented semantics (banker's rounding,
ties-to-even conversions, the §13.7 merge bias rules) and E5008 domain
errors; evaluation errors carry their §12 E5xxx codes. `std.units`
ships the full SI catalog (D15) generated from the §13.10 prefix rule
— base and named units plus all prefixed forms, binary prefixes for
`bit`/`B`, `g`-based mass prefixes. Unlisted `std` names are E3003
statically (§13.1). Every stdlib function has a self-verifying fixture
under `tests/validation/stdlib/`.

Modules (§8, `src/module.ts`): files are modules with explicit
exports; named/renamed/namespace imports and re-export link through
per-module `Env`s (imported members type, evaluate, and assert in
their declaring module's scope), the import graph is checked acyclic
(E3007), exported units/dimensions travel to importers, and the
evaluation universe is every loaded module's roots with §8.8
uniqueness (E3018). Packages (§8.6–8.7, `src/package.ts`): fail-closed
`decl.toml` manifests (E3011–E3013), exact pins with conflict
detection (E3014), and a reproducible `decl.lock` — SHA-256 over
module files in canonical path order, verified fail-closed
(E3015–E3017). Implementation conventions: dependencies live flat
under `<root>/decl_modules/<name>/`, and the lock is line-based
`name version sha256` in name order.

Tooling (Phase 4): the `decl` CLI (`check` / `evaluate` / `validate` with
`--expect-errors` / `fmt --check`), the canonical formatter (§2.1/D1 —
LF, 4-space indent, normalized spacing, line structure preserved), and
the stdio LSP server (diagnostics → hover → definition).

Real-world validation (Phase 5): three domain examples under
`examples/` — a layered service-graph deployment (quantities, health
checks, `$referrers` fan-in, constructor-func topologies, environment
parameterization), a production-scale fixture-generation sweep
(generic containers, match-driven shaping, quantity budgets), and a
fictional spine-leaf network fabric whose deterministic document
generator exercises recursive map containers, type tags, parameter
bags, cross-references, scale (1000-link sites), byte-identical round
trips, and six corruption probes. A fourth, local-only schema
additionally validated the full proprietary fixture corpus (178
documents including the complete real set); it stays out of the
repository by security policy. Findings feed
`docs/design/03_v02_revision_candidates.md`.
