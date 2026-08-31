# Specification Revisions

The revision log of the Decl specification. Every post-freeze change
records: the date, the charter decision it rests on (new or amended
D-number), the chapters touched, and a one-line rationale. A change
without an entry here did not happen (docs/README.md, freeze rules).

| Date | Version | Decision | Chapters | Change |
|---|---|---|---|---|
| 2026-08-31 | **v0.1** | — | all | Initial freeze. P1–P7, D1–D29, chapters 01–13, guide, validation cases; gated by the §0.6 evaluator spike (31/31 green, `spike/FINDINGS.md`); all open questions resolved (OQ1–OQ7). |
| 2026-08-31 | v0.1 | — (editorial) | 07 | Spell out §7.3's per-site checking mechanism: context obligations collected at declaration, discharged at each embedding site; diagnostics attributed to the embedding line. No semantic change. |
| 2026-08-31 | **v0.1.1** | **D30** (new) | 03, 07, 11, 12 | Context obligations are **declared, not inferred**: named types using `$parent`/`$root`/`$key` carry explicit context declarations (`$parent: P`); the variable is typed by its declaration, body checks are modular, and the site check is one subsumption test. Lexically nested type expressions are exempt (parent evident). Supersedes the inferred-obligation mechanism of the entry above. New codes E4094; E4090 reworded. |
| 2026-08-31 | **v0.1.2** | D30 (amended) | 07 | Context variables are **references**: `$this`/`$parent`/`$root` type as `ref<…>` (a value reading would be a self-containing value — the cycle D26 forbids); the declaration states the target bound `P`, the invariant `ref` wrapper is never written. `$key` stays a plain value. |
