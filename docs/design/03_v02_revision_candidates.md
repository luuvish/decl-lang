# v0.2 Revision Candidates

Findings collected while implementing Phases 2–4 against the frozen
v0.1 specification and validating it on real-world corpora in Phase 5.
Nothing here changes v0.1: each entry is a candidate for the v0.2
revision cycle, to be adjudicated one by one through the standard
process (charter decision + affected chapters + `REVISIONS.md` in one
change). Entries are ordered by category, then by weight of evidence.

## A. Spec defects — the frozen text conflicts with its own corpus

- **A1. Strict `S ⊑ T` static assignability rejects the guide and
  benchmarks (§3.18, §4.4).** `port: 9000 + i` (with `i: 0..<3`) infers
  `int`, which is not `⊑ 1..65535`; a string template is not `⊑` a
  pattern type. Both forms appear in normative examples. The reference
  implementation keeps the corpus sound with two precision devices —
  interval arithmetic over int ranges for `+`/`-`/`*`, and deferring
  same-kind refinement targets (pattern / range / literal set) to
  binding when membership is statically unprovable. v0.2 should
  standardize both (or an equivalent rule) in §3.18/§4.4.
- **A2. Unit-literal lexing overlapped numeric tokens (§2.7 vs ch. 11).**
  `0o755`, `1e3`, `1e-3` lexed as unit literals under the published
  grammar, contradicting §2.7 ("only decimal forms take units", float
  exponents are part of the number). The tree-sitter grammar was fixed
  to §2.7 by construction (no lookahead); ch. 11's EBNF should state the
  same token-boundary rule explicitly.
- **A3. No parsing-error code band in §12.** The registry starts at
  E3xxx; tools need a code for "syntax error" (the implementation uses a
  provisional `E1000`). Reserve and define an E1xxx band.

## B. Underspecified semantics surfaced by use

- **B1. Module-`const` records shared into several roots (§7.5).**
  A `const base: Topology` embedded in two `output`s gives its internal
  `ref`s const-rooted places — E4093 territory, but nothing defines what
  embedding a const-rooted record *value* into an output means for the
  references it carries. The working idiom is a constructor `func`
  returning the literal (each output binds its own copy). v0.2 should
  either bless that idiom normatively and add the static E4093 check, or
  define re-rooting semantics for embedded values.
- **B2. Reshaping unbound literals (§4.12, §4.8).** A constructor
  func's result is an unevaluated literal until it reaches a typed
  position; chaining `with` / comprehensions over it before binding has
  no defined semantics (the implementation supports plain `with` on such
  values, nothing deeper). Either define the lazy-structure semantics or
  state the restriction.
- **B3. Declaring-module scope for member expressions (§8.3).**
  Imported types' derived members and asserts must evaluate in the scope
  of the module that declared them (sibling consts, namespaces,
  `$referrers` targets) — the implementation enforces this; the chapter
  implies but never states it.
- **B4. Continuation lines in `assert` (§2.9, ch. 11).** After
  `assert name:` a newline continuation may not begin with `(`; the
  working form wraps the whole condition in parentheses opened on the
  colon line. Specify the continuation points precisely (the grammar has
  them; the prose does not).

## C. Extension demands from the real-world corpora

- **C1. Expression-position access to keyword-named members (§4.3).**
  Real documents use `type` as a member name. Declaration positions
  accept it; expressions must fall back to `$this["type"]`. Consider
  contextual keywords for member access (`x.type`).
- **C2. Numeric leniency at the interchange boundary (§10).** Real
  documents serialize whole floats as integers (`"frequency": 500`).
  Schemas work around it with `int | float`. Consider: a float-typed
  binding position accepts an integer lexeme exactly representable in
  binary64.
- **C3. `std.string` growth (§13.6).** Validating edge keys of the form
  `"a->b.c"` needs `split` (and would use `contains`/`ends_with`).
  One-hop operations stayed sufficient otherwise — `std.graph` remains
  unjustified (OQ3 verdict unchanged).
- **C4. Empty-string sentinels as an idiom.** Real parameter bags use
  `""` for "not configured" where a name is otherwise required; the
  `"" | Ident` union reads well and needs no language change — worth a
  guide/idiom note.

## D. Implementation gaps to close (no spec change needed)

- **D1. E4090 (embedding-site context bounds, §7.3) has no static
  check yet.**
- **D2. E4093 (ref navigating a module const) has no static check yet**
  (see B1).
- **D3. Generic type arguments that reference importer-local types**
  resolve in the exporting module's scope and can miss (documented
  limitation in the module linker).
- **D4. `matches` evaluates only where the checker types it; the engine
  lacks a `matches` case.**

## Evidence base

Phases 2–4 test suites (251 checks across eight runners) plus the
Phase 5 sweeps: the three committed domain examples under `examples/`
(service graph, fixture generation, synthetic network fabric with
scale and corruption probes), and a local-only schema additionally
validated against the full proprietary fixture corpus — 178 documents
including the complete real set (artifacts kept out of the repository
by security policy).
