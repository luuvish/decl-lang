# Language-server sessions

Scripted editor sessions for `decl-lsp` (docs/tooling/03_lsp.md), one
directory per capability: `ws/`, the workspace the session opens (a
library module on disk; the entry document's text comes from the
session), `session.json`, the steps, and `transcript.json`, the answers
the reference gives, reviewed. Every implementation's suite replays
every session against its transcript, and the parity harness replays
every session over the three servers.

## Steps

```json
{ "steps": [
  { "label": "initialize", "request": "initialize", "params": { "processId": null, "rootUri": null, "capabilities": {} } },
  { "notify": "initialized", "params": {} },
  { "label": "a syntax error", "open": "main.decl", "text": "const x = \n" },
  { "label": "a checker error", "change": "main.decl", "text": "type Bad = 10..3\n" },
  { "label": "hover top", "request": "textDocument/hover",
    "params": { "textDocument": { "uri": { "$uri": "main.decl" } }, "position": { "line": 1, "character": 7 } } },
  { "label": "code actions at c", "request": "textDocument/codeAction",
    "params": { "textDocument": { "uri": { "$uri": "main.decl" } }, "range": { "$at": "c = a + 1" },
                "context": { "diagnostics": { "$diagnostics": "main.decl" } } } },
  { "config": { "decl": { "inlayHints": { "values": true } } } },
  { "label": "shutdown", "request": "shutdown", "params": {} },
  { "notify": "exit", "params": {} }
] }
```

- `open` / `change` — `textDocument/didOpen` / `didChange` with the
  text (versions counted by the driver), then the driver waits for the
  document's `publishDiagnostics` and records the diagnostics under the
  label; with `"observe": true` it records instead the messages seen
  until then — `[method, the kind of the id, the progress kind]` per
  message, and whether the progress token's create request carried an
  integer id (03_lsp.md §14).
- `request` — sent; its result, or its error as `{ "error": … }`, is
  recorded under the label; with `"between": true`, recorded is whether
  it was answered and the methods of what arrived in between.
- `notify` — sent, nothing recorded.
- `config` — `workspace/didChangeConfiguration` with the settings, then
  one `publishDiagnostics` per open document consumed.
- `respond` — answers the server's pending request of that method:
  `{ "respond": "window/workDoneProgress/create", "result": null }`.

## Placeholders

Anywhere in `params`, an object with one `$` key:

| Placeholder | Resolves to |
|---|---|
| `{ "$uri": file }` | the file's URI in `ws/` |
| `{ "$pos": needle, "nth": 0, "offset": 0 }` | the position of the needle's `nth` occurrence in the current text of the document the request addresses (its `textDocument`), plus `offset` characters |
| `{ "$at": needle, … }` | the collapsed range at that position |
| `{ "$span": needle, … }` | the range covering the needle |
| `{ "$diagnostics": file }` | the diagnostics last published for the file, as the server sent them |
| `{ "$answer": label, "index": 0 }` | an item of an earlier answer, as the server gave it (a hierarchy item passed back) |

## Transcripts

`transcript.json` is the list of `[label, answer]` pairs. Answers are
normalized before recording so a transcript is machine-independent:
the workspace's absolute path becomes `<ws>`, `%2F` becomes `/`, and
the server's `serverInfo.version` becomes `<version>`.

The drivers, one per language: `tests/lsp/replay.py` (imported by
`decl-py/tests/lsp_test.py` and by the parity harness; also
`python tests/lsp/replay.py <case> -- <server command>` to print a
transcript), `decl-ts/tests/lsp_test.ts` (the stdio server) with
`decl-ts/tests/lsp_core_test.ts` (the same sessions over the in-memory host
the browser runs), and `decl-rs/tests/lsp_test.rs`. A new
capability lands with a session step here and its transcript,
regenerated from the reference in the same change and read.
