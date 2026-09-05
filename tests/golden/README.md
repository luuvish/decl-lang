# Goldens

The expected bytes of evaluations and of bound documents, named by
`manifest.json`; every implementation — the reference included — must
print exactly them, so an expectation is reviewed data, not whatever
the reference happens to print. An entry names a module and what a
command line prints:

```json
{ "module": "docs/examples/02_config.decl", "golden": "tests/golden/docs__examples__02_config.json" }
{ "module": "tests/golden/inputs/fabric/site.decl", "inputs": ["site=tests/golden/inputs/fabric/site_2x4.json"],
  "output": "site", "golden": "tests/golden/fabric__site_2x4.json" }
{ "module": "tests/golden/inputs/fabric/site.decl", "inputs": ["site=…/zero_line_rate.json"],
  "rejected": true, "golden": "tests/golden/fabric__zero_line_rate.txt" }
{ "markdown": "docs/guide/01_overview_by_example.md", "golden": "tests/golden/docs__guide__01_overview_by_example.json" }
```

- without `output`, the golden is what `decl evaluate <module>` prints:
  the exported outputs as one object (§5.5), or `{}`;
- with `inputs` and `output`, the golden is the bare document of that
  root after the documents are bound (`evaluate --input … --output`);
- with `rejected`, the golden is what `decl validate --input …` prints
  on standard error — the diagnostics, in canonical order — and the
  exit status must be 1;
- with `markdown` instead of `module`, the module is the markdown
  file's ```decl blocks in order, assembled by the driver into a
  temporary file — the guide is a module of the corpus, not prose.

Paths are repository-relative; the drivers run from the repository
root. The documents under `inputs/`:

- `interconnect/`, `config/` — each benchmark's bare output, bound back
  as its `input` root (a golden that is its own input is a §10.5 round
  trip), and a corrupted variant rejected with the root-cause
  diagnostic;
- `fabric/` — the fabric example's site (`site.decl` imports the
  schema), its six corrupted variants, and `gen_site.py`, which
  generates a spine-leaf site deterministically: it must reproduce
  `site_2x4.json` byte for byte, and the parity harness generates a
  10×20 site as a scale row;
- `match/`, `generics/`, `quantity/` — small modules with their
  documents: `match` over record and literal variants, generic
  instantiation and its size violation (§3.15), dimension algebra with
  a document in a derived unit (§3.16).

Every implementation's suite replays the manifest
(`decl-ts/tests/golden_test.ts`, `decl-rs/tests/golden_test.rs`,
`decl-py/tests/golden_test.py`); the harness's `golden` section runs it
over the three. A golden is regenerated only for a deliberate change,
in the same commit as the change.
