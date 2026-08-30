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
| Reference implementation | **TypeScript first, Rust later** — type checker, evaluator, and CLI in TypeScript (BigInt aligns with arbitrary-precision integers; `Number::toString` with shortest round-trip float printing). A Rust runtime is considered only after the spec and implementation stabilize. |

## Reference assets

- `../decl-lang` — previous iteration: Decl 2 spec drafts (reference for the
  rewrite), legacy Decl 1 tree-sitter grammar, and 142 validation fixtures
  (rewrite sources for Phase 1)
- `../../research/oic-design-suite` — real-world NoC spec tooling: diagnostics
  / determinism / reproducibility discipline, NoC LSP contract docs, and real
  NoC (JSON) fixtures that become direct validation targets in Phase 5

## Phase overview

| Phase | Name | Deliverables | Status |
|---|---|---|---|
| 0 | Specification (v0.1 freeze) | design docs, spec chapters, stdlib spec, validation corpus desk check, evaluator spike | in progress (0.1 started) |
| 1 | Grammar & parser | tree-sitter grammar + corpus tests + fixtures | not started |
| 2 | Reference implementation core (TS) | type check, evaluation, constraint validation, serialization + conformance runner | not started |
| 3 | Modules & standard library | import/export, manifest + lock, `std.*` | not started |
| 4 | CLI & tooling | `decl` CLI, formatter, minimal LSP | not started |
| 5 | Real-world validation & feedback | 3 domain libraries, v0.2 revision list | not started |

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
- Design principles and numbered design decisions — the charter that spec
  chapters must never contradict; open questions are tracked and promoted to
  numbered decisions when resolved

### 0.2 Specification chapters

Author the normative spec. Expected coverage (final chapter structure is
decided during writing): introduction and core concepts, lexical structure,
type system, expressions, declarations and schemas, constraints and
diagnostics, relationships, modules and packages, evaluation semantics, data
interchange, formal grammar (EBNF), error and diagnostic codes.

### 0.3 Standard library specification

Define the `std.*` modules used by the spec and validation corpus: per-module
signatures, semantics, and error conditions. The v0.1 scope is limited to
functions the spec and corpus actually use.

### 0.4 Guide

Example-driven overview walking one scenario end to end
(describe → generate → validate).

### 0.5 Cross-consistency review and validation corpus desk check

- Formal grammar ↔ chapter prose ↔ design-decision examples ↔ guide examples
  all agree; every error condition has an assigned code
- Write the three generality benchmark cases in the new syntax and review
  them against the spec alone (any blocking point is a spec defect):
  1. **Hardware interconnect** — node/port/edge graph, derivations, and
     constraints with no domain keywords
  2. **API/config schema** — open records, defaults, per-environment override
  3. **Test fixture generation** — comprehension-based parameterized instances

### 0.6 Minimal evaluator spike (evidence pass)

Errors caught by reading and errors caught by execution are different sets;
the previous iteration froze specs on reading alone, three times
([00. Vision §5](docs/design/00_vision.md)). Do not declare v0.1 frozen
until a spike implementation has met the spec.

- Time-boxed, **throwaway** evaluator over a representative Decl subset —
  assignability, `input` binding, defaults/derived evaluation, constraint
  checking. Not the Phase 2 reference implementation; no code is promised to
  survive into it
- Run it on the three benchmark cases from 0.5, and bind the real fixture
  `../decl-lang/tests/validation/customs/oic.decl` (ported to the new
  syntax/data) as `input`
- Exercise the checklist in [00. Vision §6](docs/design/00_vision.md)
  end to end — quantity round-trip and input binding, assignability across
  all four member kinds, closedness vs subtyping, whether transitive/fixpoint
  queries are actually needed by the interconnect case
- Every blocking point feeds back into chapters and design decisions before
  the freeze

### 0.7 v0.1 freeze declaration

Update the status in `docs/README.md`; record all subsequent spec changes as
revisions.

**Exit criteria**: zero open questions across all chapters · stdlib spec
exists · zero grammar/prose mismatches · all three validation cases written
and reviewed · evaluator spike run with every finding resolved (spec fixed or
decision recorded).

---

## Phase 1 — Grammar & parser

Goal: a tree-sitter grammar written against the new spec, in a state where
"every spec example parses".

- Write `grammar.js` from the formal grammar chapter and `tree-sitter
  generate` (the legacy grammar in `../decl-lang/tree-sitter-decl/` serves as
  a rewrite source and reference)
- Build corpus tests from per-chapter examples and counterexamples
- Rewrite `tests/validation/` fixtures in the new syntax (keep `@expect-*`
  metadata; only `@expect-phase: parsing` is judged at this stage)
- Highlight queries and playground setup

**Exit criteria**: all spec and guide examples parse · all valid fixtures
parse · all parsing-phase invalid fixtures are detected · error-recovery
smoke test (remaining declarations still parse in a broken file).

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

- `decl` CLI: `check` (parse + types), `eval` (instance evaluation → JSON),
  `validate` (input binding validation, `--expect-errors` support), `fmt`
- Formatter: enforce the canonical form the spec defines
- Minimal LSP surface, in order: diagnostics → hover → definition. The
  contract docs under
  `../../research/oic-design-suite/toolkit-tools/editor/doc-lsp/docs/`
  (diagnostics, completion, definition, …) serve as design references

**Exit criteria**: `decl validate tests/` judges the full fixture corpus ·
formatter idempotency (`fmt(fmt(x)) == fmt(x)`) · diagnostics displayed in an
editor.

---

## Phase 5 — Real-world validation & feedback

- **NoC domain library**: describe OIC's element/parameter/constraint system
  as a Decl library and validate real NoC (JSON) fixtures from
  `oic-design-suite` bound directly as `input` — the final test of
  "generality without domain keywords"
- Grow the API/config schema and fixture-generation cases to production level
- Collect spec defects and extension demands discovered in use; plan the v0.2
  revision
- Decide whether to start the Rust runtime (when performance or deployment
  needs are confirmed by measurement)

**Exit criteria**: successful validation of at least part of the real NoC
fixture set · documented v0.2 revision list.

---

## Working rules

- When adding documents, update the index in `docs/README.md`; when fixtures
  change, update the statistics table in `tests/validation/README.md` in the
  same change.
- Commit messages: short, lower-case, descriptive.
- Update the phase table in this document when a phase advances.
