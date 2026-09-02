# Decl Development Roadmap

Decl is developed in the order **specification → parser → reference
implementation → tooling → real-world validation**. Each phase must satisfy
its **exit criteria** before the next phase begins. Progress is recorded by
updating the phase table in this document.

This repository is a fresh start. The specification is written from scratch
here; prior assets live in the reference repositories below and are consulted,
never migrated wholesale or modified from this repo.

## Confirmed approach

| Decision | Content |
|---|---|
| Methodology | **Spec-first with an evidence gate** — author the complete specification, then a throwaway minimal-evaluator spike (§0.6) must meet it before v0.1 is frozen; implementation proper begins only after the freeze. Post-freeze spec changes are recorded as revisions. |
| Parser | **tree-sitter as the single canonical parser**, written against the spec's formal grammar chapter. The reference implementation consumes it through bindings. |
| Reference implementation | **TypeScript first, Rust later** — type checker, evaluator, and CLI in TypeScript (BigInt aligns with arbitrary-precision integers; `Number::toString` with shortest round-trip float printing). A Rust runtime is considered only after the spec and implementation stabilize. *Done 2026-09-02:* native **Rust** (`decl-rust/`, crate `decl-lang`) and **Python** (`decl-python/decl/runtime`) implementations cover the whole language: parser binding, static checker, evaluator, modules and packages, the canonical formatter, and the `decl-lsp` server (the Python package needs no Node.js). The layout is parallel (`decl-typescript/`, `decl-rust/`, `decl-python/`, same module names) and `make verify` — each implementation's tests, then `tests/parity/differential.py` — keeps the three byte-identical over every output-bearing example and fixture, plus document binding with root-cause diagnostics; CI runs it on every push. The three now cover the whole language (checker, formatter, LSP, and packages ported 2026-09-02) and share one module layout; the reference's platform-neutral core (`decl-lang/core`) is what the website's playground runs. |

## Reference assets

- `../decl-lang` — previous iteration: Decl 2 spec drafts (reference for the
  rewrite), legacy Decl 1 tree-sitter grammar, and 142 validation fixtures
  (rewrite sources for Phase 1)
- `../../research/oic-design-suite` — real-world NoC spec tooling: diagnostics
  / determinism / reproducibility discipline, editor/LSP contract docs, and
  real NoC design fixtures (JSON) that become direct validation targets in
  Phase 5

## Phase overview

| Phase | Name | Deliverables | Status |
|---|---|---|---|
| 0 | Specification (v0.1 freeze) | design docs, spec chapters, stdlib spec, validation corpus desk check, evaluator spike | **done — v0.1 frozen 2026-08-31** |
| 1 | Grammar & parser | tree-sitter grammar + corpus tests + fixtures | **done — 2026-08-31** |
| 2 | Reference implementation core (TS) | type check, evaluation, constraint validation, serialization + conformance runner | **done — 2026-09-01** (full static checker ⊑/§4.10/§4.7/§4.13/§3.15/§3.16; guide + benchmarks e2e; byte-identical round-trips) |
| 3 | Modules & standard library | import/export, manifest + lock, `std.*` | **done — 2026-09-01** (module linking §8, packages + reproducible lock §8.6–8.7, std 1:1 with SI catalog §13) |
| 4 | CLI & tooling | `decl` CLI, formatter, minimal LSP | **done — 2026-09-01** (check/evaluate/validate/fmt; formatter idempotent + AST-safe over the corpus; stdio LSP with diagnostics/hover/definition) |
| 5 | Real-world validation & feedback + v0.2 cycle | 3 domain examples, v0.2 revisions adjudicated | **done — 2026-09-01** (three domain examples under `examples/`: service graph, fixture generation, and a synthetic network fabric with scale + corruption probes; the full proprietary fixture corpus — 178 documents incl. the complete real set — additionally validated locally, artifacts kept out of the repo by security policy; v0.2 candidates adjudicated 2026-09-01 → revisions v0.1.4–v0.1.8, **v0.2 declared**) |

---

## Phase 0 — Specification (v0.1 freeze)

Goal: author the complete language specification from scratch and raise it
from "draft" to "frozen v0.1". All later phases build against the frozen
spec; subsequent changes go through a revision process (decision record plus
simultaneous chapter updates).

### 0.0 Repository bootstrap — done

- Repository reset: prior migrated docs removed, git history re-initialized
- Agent rules (`AGENTS.md`, `CLAUDE.md` import pointer), README, this roadmap

### 0.1 Design documents

- Vision and background — done: [docs/design/00_vision.md](docs/design/00_vision.md)
- Requirements — done: [docs/design/01_requirements.md](docs/design/01_requirements.md)
  (goals, non-goals, capability requirements, generality benchmark, quality bar)
- Design principles and decisions — done:
  [docs/design/02_design_decisions.md](docs/design/02_design_decisions.md)
  (P1–P7, D1–D29, rejected-syntax table, vision-checklist traceability; all
  open questions resolved except the two spike-gated ones, OQ3 and OQ7)

### 0.2 Specification chapters — done

Chapters 01–12 authored under `docs/specification/`: introduction, lexical
structure, type system, expressions, declarations and schemas, constraints
and diagnostics, relationships, modules and packages, evaluation semantics,
data interchange, formal grammar (EBNF), error and diagnostic codes.

### 0.3 Standard library specification — done

`13_stdlib.md`: per-module signatures, semantics, and error conditions for
the complete v0.1 `std.*` surface (array, math, int, float, string, object,
map, SI unit catalog; `std.graph` reserved pending OQ3), scoped to what the
spec and corpus actually use.

### 0.4 Guide — done

`docs/guide/01_overview_by_example.md`: one scenario end to end
(describe → generate → validate), showing the evaluated JSON, the
diagnostic output, and root-cause reporting in action.

### 0.5 Cross-consistency review and validation corpus desk check — done

- Three parallel review sweeps (charter↔chapters, grammar↔examples,
  code-coverage↔cross-references) surfaced ~40 findings; all resolved
- The three generality benchmark cases are written and desk-checked under
  `docs/examples/` — the desk check itself surfaced and fixed seven spec
  defects (see the findings log in `docs/examples/README.md`)

### 0.6 Minimal evaluator spike (evidence pass) — done

The throwaway evaluator lives in `spike/` (`node spike/src/run.ts` —
31 checks, 0 failures): parse → bind → evaluate → validate → serialize →
round-trip over all three benchmark cases, including the ported 2x2
crossbar with `$referrers`-based width propagation and byte-identical
round-trips under renamed roots.

Execution caught five defects reading had missed — nested-literal name
resolution, `$referrers`×laziness ordering, integral-float round-trip
breakage, `with` copying derived members, and the arbiter's missing
width rule — each fed back into the chapters and charter
(`spike/FINDINGS.md`). The spike resolved the two gated questions:
**OQ3 — `std.graph.*` not admitted** (the whole corpus is one-hop);
**OQ7 — no expression-level bindings** (the corpus never hurt without
them).

### 0.7 v0.1 freeze declaration — done (2026-08-31)

**v0.1 is frozen.** `docs/README.md` carries the frozen status and the
revision process; `docs/REVISIONS.md` is the revision log, seeded with the
freeze entry.

Exit criteria, verified: zero open questions across charter and chapters
(OQ1–OQ7 all resolved) · stdlib spec exists (ch. 13) · grammar/prose
mismatches resolved (§0.5 sweeps) · all three validation cases written and
desk-checked (`docs/examples/`) · evaluator spike run green with every
finding resolved (`spike/FINDINGS.md`).

---

## Phase 1 — Grammar & parser — done (2026-08-31)

`tree-sitter-decl/`: grammar.js against chapter 11, with the §2.9 newline
rule and nested block comments in the external scanner; corpus tests
(incl. error-recovery smoke); `queries/highlights.scm`; wasm build and
playground verified.

`tests/validation/`: 40 fresh fixtures with `@expect-*` metadata and the
parsing-phase runner (`node tests/run_parsing.mjs`). The legacy 142
fixtures were not ported 1:1 — they test Decl 1 syntax; the corpus is
authored against v0.1 and keeps growing through Phase 2, which also picks
up the deferred `@expect-phase: checking/binding` fixtures.

**Exit criteria, verified**: every complete spec/guide example parses
(51/51 module-level blocks; the rest are deliberate fragments) · all
valid fixtures parse · all parsing-phase invalid fixtures are detected
(including one fixture per rejected-syntax form) · error-recovery smoke
green (corpus `:error` tests).

---

## Phase 2 — Reference implementation core (TypeScript)

Goal: implement the full evaluation pipeline for a single module:
parse → name resolution → type check → dependency analysis → lazy
evaluation → constraint validation → emission.

- Skeleton: consume tree-sitter bindings → CST-to-AST layer
- Name resolution (single file first; cross-module resolution deferred to
  Phase 3)
- Type checker per the spec's type system chapter
- Evaluator: lazy evaluation with a dependency graph (cycle errors),
  deterministic numeric semantics, comprehensions, record update,
  defaults-then-derived ordering
- Constraint validation and diagnostics: stable ids, automatic path
  attachment, error-severity invalidation and propagation
- Serialization per the data interchange chapter
- **Conformance runner**: a test harness judging `@expect-error` /
  `@expect-message` / `@expect-phase` over `tests/validation/` fixtures — the
  regression baseline for every later phase

**Exit criteria**: guide example reproduced end to end
(describe → generate → validate) · all fixtures judged correctly ·
byte-identical output across repeated runs on the same input.

---

## Phase 3 — Modules & standard library

- Module resolution: import/export semantics per the spec, name-collision
  diagnostics
- Packages: manifest, exact-pinned dependencies, lock file with content
  hashes, deterministic resolution tests under the same lock
- `std.*` implementation matching the stdlib spec 1:1, including specified
  error conditions

**Exit criteria**: multi-file validation case works · lock reproducibility
tests pass · a fixture exists for every stdlib function.

---

## Phase 4 — CLI & tooling

- `decl` CLI: `check` (parse + types), `evaluate` (output evaluation → JSON),
  `validate` (input binding validation, `--expect-errors` support), `fmt`
- Formatter: enforce the canonical form the spec defines
- Minimal LSP surface, in order: diagnostics → hover → definition. The
  editor LSP contract docs in the reference suite (diagnostics, completion,
  definition, …) serve as design references

**Exit criteria**: `decl validate tests/` judges the full fixture corpus ·
formatter idempotency (`fmt(fmt(x)) == fmt(x)`) · diagnostics displayed in an
editor.

---

## Phase 5 — Real-world validation & feedback

- **NoC domain library**: describe a NoC element/parameter/constraint system
  as a Decl library and validate the real design fixtures (JSON) from the
  reference suite bound directly as `input` — the final test of
  "generality without domain keywords"
- Grow the API/config schema and fixture-generation cases to production level
- Collect spec defects and extension demands discovered in use; plan the v0.2
  revision
- Decide whether to start the Rust runtime (when performance or deployment
  needs are confirmed by measurement) — *decided 2026-09-02: native Rust
  and Python implementations of the whole language, verified by differential
  testing against the reference (see the decision table above)*

**Exit criteria**: successful validation of at least part of the real
fixture set · documented v0.2 revision list.

---

## Working rules

- When adding documents, update the index in `docs/README.md`; when fixtures
  change, update the statistics table in `tests/validation/README.md` in the
  same change.
- Commit messages: short, lower-case, descriptive.
- Update the phase table in this document when a phase advances.
