# Render corpus

The renderer (docs/tooling/05_render.md) case by case: documents in
YAML, the layouts of a document, the forms a module declares with
`@render`, and — as the templates and the fan-out land — the texts they
produce. Everything here is data every implementation runs; the parity
harness replays it over the three command lines.

| File | What it holds |
|---|---|
| `cases.json` | the renderer case by case, in the shape of `tests/cli/cases.json`: the files of a case (its module with `@render`, its templates and documents), the command line with `<dir>` for the case's directory, and the recorded outcome — exit status, standard output and error, the files left; every key of `@render`, every statement and text form of the template dialect, the whitespace rules, the `render` namespace, functions in a template, every error of §5.8, the template sources (`--template`, `-`), and fan-out over an array and a map |
| `formats.json` | one entry per golden of `tests/golden/`: its YAML form under `formats/` and its indented JSON forms for the indents named — what `evaluate --format yaml` and `evaluate --indent n` print for the golden's own command line, and what the reader reads back to the golden |
| `inputs/` | the YAML twin of every document the golden corpus binds (`tests/golden/inputs/`), same path, `.yaml`: bound in the JSON's place, each gives the golden — which proves the reader on a document the writer produced |
| `invalid/` | documents the reader must refuse, and `cases.json`: each file with the message the reader gives (`<reason> at line L`); `doc.decl` is the input they are bound to, so that the command line reports `E6004 … bound document is not well-formed YAML: <file>: <message>` |

```json
{ "golden": "tests/golden/quantity__trip.json", "yaml": "tests/render/formats/quantity__trip.yaml",
  "indent": { "2": "tests/render/formats/quantity__trip.indent2.json", "4": "tests/render/formats/quantity__trip.indent4.json" } }
{ "file": "duplicate_key.yaml", "message": "mapping repeats the key \"a\" at line 3" }
```

The drivers: `decl-ts/tests/render_test.ts`, `decl-rs/tests/render_test.rs`,
`decl-py/tests/render_test.py`; the harness's `yaml-input`, `format`,
and `render` sections. The expected texts are produced once by the reference and
reviewed, like every golden (tests/golden/README.md): a YAML twin or a
format is regenerated only for a deliberate change of the writer, in
the same commit.
