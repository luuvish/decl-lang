# Editor extensions — VS Code and Zed

Two editor extensions ship with the language: **`vscode-decl`** for
Visual Studio Code and **`zed-decl`** for Zed. Both are the editor face
of the language server ([03. Language server](03_lsp.md)) and the REPL
([02. REPL](02_repl.md)): a language contribution, a pointer to
`decl-lsp`, and the few features that belong to the editor rather than
the server. They are thin on purpose: every answer they show comes from
the server, and the server is the same core as the command line
([01. Command line](01_cli.md)), so what an editor says and what `decl`
prints never disagree — and what VS Code says and what Zed says is one
server's answer twice.

The two live beside the implementations, `extension/vscode/` (a member of
the npm workspace) and `extension/zed/` (a Rust crate Zed builds for
`wasm32-wasip1`), and their versions follow the language's. This
document is informative: the behavior it describes is the server's, and
the server's is the specification's.

## 1. Two extensions, one server

| | VS Code (`vscode-decl`) | Zed (`zed-decl`) |
|---|---|---|
| distribution | Visual Studio Marketplace and Open VSX, `luuvish.vscode-decl` | the Zed extension registry, id `decl` |
| the server | bundled (npm `decl-lang`), or any `decl-lsp` by path | `decl-lsp` from `PATH`, a setting, or a prebuilt release binary Zed downloads |
| highlighting | a TextMate grammar generated from the tree-sitter grammar, plus semantic tokens | the tree-sitter grammar itself, compiled by Zed, with Zed's queries |
| editor-side features | output preview, trace view, syntax tree, tasks, Test Explorer, REPL terminal, web extension | runnables (fixtures, outputs), outline, tasks templates, REPL in the terminal |
| in the browser | vscode.dev / github.dev on `decl-lang/core` | — |

What follows is VS Code in §2–§13, Zed in §14–§16, and the shared
sections — other editors, packaging, what is not applicable,
verification — after.

## 2. VS Code: setup

Install **Decl** from the Extensions view. Nothing else is needed: the
extension bundles the reference implementation's `decl-lsp` (npm
`decl-lang`) and runs it on the extension host's own Node.js. To use
another implementation's server — the Rust or Python `decl-lsp`, or a
development build — set `decl.server.path`; the three servers answer
identically, so the switch is invisible except for speed. Open a `.decl`
file and the server starts; the status bar shows `Decl` with its state.

The extension also runs in the browser (vscode.dev, github.dev) on the
platform-neutral core, §13.

## 3. VS Code: principles

- **Nothing is computed in the extension.** Diagnostics, completion,
  navigation, formatting, evaluation, tracing: all of it is the server's
  (03_lsp.md §2), and each feature below names the server section it
  surfaces. The extension owns presentation — views, pickers, terminals,
  settings — and the protocol plumbing.
- **The three servers are interchangeable**, and the extension's tests
  run against all three (§20).
- **Same bytes as the command line.** The output preview shows exactly
  what `decl evaluate --output root` prints; the fixture runner judges
  exactly as `decl validate <dir>` does; the REPL terminal is `decl repl`.
- **Zero configuration to start**, every choice a setting (§10), all of
  them under `decl.`.

## 4. VS Code: the language contribution

- **Files**: `.decl` is language `decl`; `decl.toml` and `decl.lock` are
  associated with `toml` (highlighted by a TOML extension when one is
  installed) and receive the server's diagnostics — a lock that drifted
  (E3015–E3017) is marked in the lock file itself.
- **Language configuration**: `//` and `/* */` comments (toggle,
  block), bracket pairs `{}` `[]` `()` with auto-closing and surrounding,
  `"` and `` ` `` auto-closed (not `/`: a pattern literal is rare and a
  division is not), a word pattern that keeps `$this`, `$referrers`, and
  a hidden member `name$` whole on double-click, indentation rules
  (increase after an opening bracket, decrease before its closer), and
  on-Enter continuation of `///` documentation comments. Folding and
  selection ranges come from the server (03_lsp.md §9); `// #region` /
  `// #endregion` markers fold too.
- **Syntax highlighting**: a TextMate grammar
  (`syntaxes/decl.tmLanguage.json`) generated from the tree-sitter
  grammar's token definitions by a script in `tree-sitter-decl/`, so the
  two never drift: keywords, declaration heads, literals with unit
  suffixes (`250ms`), template and pattern literals with their `${…}`
  holes as embedded expressions, `///` and `@doc` documentation, context
  variables, hidden members. It is the fallback and the first paint; the
  server's semantic tokens (03_lsp.md §13) layer member kinds, keyword
  member names, unresolved names, and unit symbols on top, and semantic
  highlighting is on for `decl` by default.
- **Snippets**: `type`, `output`, `input`, `func`, `assert`, `match`,
  `for`, `import` — each the canonical form the formatter would produce.

## 5. VS Code: the server

- **Which server**: `decl.server.path` empty (the default) runs the
  bundled reference `decl-lsp`; a path or a bare name (`decl-lsp`,
  resolved on `PATH`) runs that one; `decl.server.args` adds arguments.
  `Decl: Select Server` offers the bundled server, the ones found on
  `PATH` (with their `--version`), and a custom path, and writes the
  setting.
- **Lifecycle**: one client per window, started when the first `.decl`
  file opens and given every workspace folder (the server keeps one
  session object per package, 03_lsp.md §14). `Decl: Restart Server`
  restarts it; a crash restarts it automatically, and repeated crashes
  stop it with a notification that links to the output channel.
- **Status**: the status bar item `Decl` shows starting / ready /
  evaluating (with the server's `$/progress`) / stopped, and the last
  evaluation's time; clicking it opens a menu — restart, show output,
  select server, open the REPL.
- **Output**: the channel *Decl Language Server* carries the server's
  log; `decl.trace.server` (`off` / `messages` / `verbose`) adds the
  protocol traffic.

## 6. VS Code: editing

Everything in 03_lsp.md §3–§14 reaches the editor through the client
with no extension code, in VS Code's own UI:

| VS Code | Server section |
|---|---|
| Problems panel, squiggles, the code's link to the registry | §3 (and §8 here) |
| hover | §4 |
| IntelliSense, signature help, snippet completions | §5 |
| Go to Definition / Type Definition / Declaration, Peek, Find All References, symbol highlight | §6 |
| Outline, breadcrumbs, Go to Symbol in Workspace, Call Hierarchy and Type Hierarchy views | §7 |
| Rename (F2, with the prepare check), linked editing of paired names | §8 |
| Format Document, format on save (`editor.formatOnSave`), format on type, folding, Expand Selection | §9 |
| the lightbulb, Quick Fix, refactor and source-action menus, `source.fixAll.decl` in `editor.codeActionsOnSave` | §10 |
| inlay hints, each kind toggled by a `decl.inlayHints.*` setting | §11 |
| code lenses (`decl.codeLens.enable`) | §12 |
| semantic highlighting | §13 |

Formatting is the canonical formatter: format on save keeps every file
in the form `decl fmt --check` accepts.

## 7. VS Code: evaluation in the editor

- **Output preview** — `Decl: Open Output Preview` (also the *evaluate*
  lens on an `output`, and the preview button in the editor title, as
  Markdown has) opens, beside the editor, a read-only JSON document
  `decl-evaluate://<file>/<root>`: the document of the chosen root — a
  picker when the module has several, the exported outputs as one object
  when none is chosen — exactly as `decl evaluate --output root` prints
  it, pretty-printed (`decl.preview.compact` for the wire form). It
  refreshes on save, or while typing after the idle delay, or only on
  demand (`decl.preview.refresh`). When the root is invalid the preview
  shows the diagnostics in the document's place, and the Problems panel
  has them too. Several previews may be open, one per root.
- **Inputs** — `Decl: Bind Input…` picks a JSON file for an `input` root
  (also the *validate with …* lens) and records it in `decl.inputs`, a
  workspace setting mapping input names to files — the binding `decl
  evaluate --input` and the REPL's `:bind` take, kept in the workspace so
  the command line and the editor agree. A bound document is validated
  continuously: its diagnostics appear on the JSON file itself, at the
  offending path (`$.nodes["x"].ports["si0"]` mapped to its position in
  the JSON), and in the previews that read it. `Decl: Unbind Input`
  removes the binding.
- **Validate** — `Decl: Validate` runs the whole-document validation of
  a root (the REPL's `:validate`) and reports the verdict — `ok`, or the
  counts — in a notification, with the diagnostics in Problems.
- **Trace** — `Decl: Trace Value` on the place under the cursor (a
  member in a literal, a path in a preview, a path in a bound document)
  opens the *Decl: Trace* view: the derivation tree — the member
  expression and the places it read, down to supplied values and
  literals — or, for an invalid place, the chain of invalidation to the
  root-cause diagnostic (§6.6); every node navigates to its source or
  document position. The REPL's `:trace`, as a tree.
- **Syntax tree** — `Decl: Show Syntax Tree` opens
  `decl-syntax://<file>`, the tree-sitter tree of the buffer, following
  the cursor: the node under the cursor is highlighted in the tree and
  selecting a node selects its range. The grammar's debugging view.
- **Reload** — `Decl: Reload Workspace` re-reads every module, lock, and
  bound document from disk (the REPL's `:reload`).

## 8. VS Code: diagnostics

Diagnostics arrive from the server (03_lsp.md §3) and are shown as VS
Code shows them, with three additions:

- the **root-cause chain** (§6.6) comes as related information: under a
  derived diagnostic, the places it was invalidated through, each a
  link, ending at the diagnostic that caused it;
- the **code** (`E4001`, a constraint's `Type.assert` id) links to the
  registry on the website (§12.4) or to the `assert` declaration;
- in a fixture whose `@expect-error` names the code, the expected
  diagnostic is shown as information tagged *expected by the fixture*,
  so a corpus opens clean and an unexpected diagnostic stands out.

Quick fixes and assists are the server's (03_lsp.md §10); the extension
adds none.

## 9. VS Code: commands

All commands are in the palette under the *Decl:* prefix; none has a
default keybinding, all are bindable. Server commands are executed
through `workspace/executeCommand` (03_lsp.md §12); the others are the
extension's own.

| Command | Title | Does |
|---|---|---|
| `decl.openOutputPreview` | Open Output Preview | the preview of a root (§7) |
| `decl.evaluate` | Evaluate | evaluate a root and show its document (server) |
| `decl.validate` | Validate | the whole-document verdict of a root (server) |
| `decl.bindInput` | Bind Input… | bind a JSON file to an input root (server; records `decl.inputs`) |
| `decl.unbindInput` | Unbind Input | remove a binding |
| `decl.trace` | Trace Value | the derivation or root cause of the place under the cursor (server) |
| `decl.showSyntaxTree` | Show Syntax Tree | the tree-sitter tree of the buffer (server) |
| `decl.runFixtures` | Run Fixtures | judge the corpus the current file belongs to (§12) |
| `decl.openRepl` | Open REPL | a REPL terminal attached to the workspace's session (§12) |
| `decl.sendToRepl` | Send Selection to REPL | the selection, or the current line, as REPL input |
| `decl.reloadWorkspace` | Reload Workspace | re-read modules, lock, and documents from disk (server) |
| `decl.restartServer` | Restart Server | stop and start the server |
| `decl.selectServer` | Select Server | choose the bundled or another `decl-lsp` |
| `decl.showOutput` | Show Server Output | the output channel |

## 10. VS Code: settings

| Setting | Default | Meaning |
|---|---|---|
| `decl.server.path` | `""` | the `decl-lsp` to run; empty is the bundled reference server |
| `decl.server.args` | `[]` | extra arguments for the server |
| `decl.trace.server` | `off` | protocol trace in the output channel (`messages`, `verbose`) |
| `decl.inputs` | `{}` | input name → JSON file (workspace scope), the editor's `--input` |
| `decl.preview.refresh` | `save` | when previews refresh: `save`, `type` (after the idle delay), `manual` |
| `decl.preview.compact` | `false` | previews in the wire form instead of pretty-printed |
| `decl.evaluate.idleDelay` | `300` | milliseconds of quiet before re-evaluation while typing |
| `decl.inlayHints.types` | `true` | inferred types of unannotated derived members |
| `decl.inlayHints.parameterNames` | `true` | parameter names at call sites |
| `decl.inlayHints.values` | `false` | evaluated values of defaulted and derived members of outputs |
| `decl.inlayHints.units` | `true` | base-unit value of quantity literals |
| `decl.inlayHints.contextVariables` | `false` | the bound of a context variable at its use |
| `decl.inlayHints.closingBraces` | `false` | `} // Name` after long records |
| `decl.codeLens.enable` | `true` | the lenses of 03_lsp.md §12 |
| `decl.fixtures.directories` | `[]` | globs of fixture corpora; empty detects directories holding `valid/` or `invalid/` |
| `decl.fixtures.runOnSave` | `false` | re-judge a fixture when it is saved |

The inlay-hint and idle-delay settings are forwarded to the server as
its configuration (03_lsp.md §14); Zed forwards the same keys (§15).

## 11. VS Code: tasks and the problem matcher

The task type `decl` runs the command line: `{ "type": "decl", "command":
"check" | "evaluate" | "validate" | "fmt", "args": [...] }`, with three
tasks contributed ready to run — *decl: check*, *decl: validate*,
*decl: fmt --check* — over the current file, the workspace's entry
modules, or as `args` says. The problem matcher `$decl` reads the
command line's diagnostic line (01_cli.md §4),
`<file>: <severity> [<code>] <id> at <path>: <message>`, into Problems;
it carries the file, severity, code, and message — the path is in the
message, and positions come from the server, not the matcher. A task
that must consume diagnostics programmatically uses `--json`.

## 12. VS Code: fixtures in the Test Explorer, the REPL in the terminal

A fixture corpus — a directory with `valid/` and `invalid/` children of
`.decl` files carrying `@expect-phase` / `@expect-error` /
`@expect-message` headers (`tests/validation/README.md`) — appears in the
Test Explorer as a test tree: corpus, feature, fixture. Running a node
judges it as `decl validate <dir>` does (the server's judge, through
`decl.runFixtures`); a fixture passes or fails with the `FAIL` detail
the command line prints, shown inline. The *run* lens on a fixture's
header and on a corpus directory triggers the same;
`decl.fixtures.runOnSave` re-judges a fixture as it is edited. Corpora
are detected, or named by `decl.fixtures.directories`. The language's
own corpus is the first client of this view.

The REPL:

- The terminal profile *Decl REPL* runs `decl repl` on the current file
  with the workspace's `decl.inputs` as `--input` bindings, using the
  `decl` beside the selected server.
- `Decl: Open REPL` (the server's `decl.openRepl`) attaches the REPL to
  the workspace's session object, so the session sees the open buffers
  as the server does — an unsaved edit is already loaded.
- `Decl: Send Selection to REPL` sends the selection, or the current
  line, as one input: an expression is evaluated partially, a declaration
  is added, a command runs. The result appears in the terminal; the
  editor is not changed.

## 13. VS Code: the web extension

The extension declares a browser entry: on vscode.dev and github.dev
the server runs in a web worker on `decl-lang/core` — the
platform-neutral core the website's playground runs — with the wasm
grammar bundled. Diagnostics, hover, completion, navigation,
formatting, the output preview, the trace view, and the syntax tree
work there; the REPL terminal, tasks, and `decl.server.path` do not (no
process to run). The answers are the same bytes: the worker server is
the reference implementation, and the parity harness's scripted session
runs against it too.

## 14. Zed: the extension

Zed extensions are declarative where VS Code's are programmatic: a
manifest, a grammar, a language definition with tree-sitter queries,
and a small Rust module compiled to WebAssembly that tells Zed how to
start the language server. `extension/zed/` is exactly that:

```
extension/zed/
  extension.toml                the manifest: id, version, the grammar, the language server
  Cargo.toml, src/lib.rs        the wasm module: where decl-lsp is, and its configuration
  languages/decl/
    config.toml                 name, path suffixes, comments, brackets, tab size, word characters
    highlights.scm              Zed's capture names, generated from the grammar's queries
    brackets.scm  indents.scm   pairing and indentation
    outline.scm                 types, outputs, inputs, functions, constants → outline and breadcrumbs
    runnables.scm               fixtures and outputs as runnables
    textobjects.scm             declarations and members as Vim-mode text objects
    overrides.scm               string and comment contexts (no auto-close inside them)
```

- **Grammar**: `[grammars.decl]` points at this repository and the
  `tree-sitter-decl` path at a pinned revision; Zed fetches and
  compiles it. Highlighting is therefore the grammar's own — the same
  tree the server parses — with `highlights.scm` regenerated from
  `tree-sitter-decl/queries/highlights.scm` by the script that produces
  the TextMate grammar, so VS Code's fallback, Zed's, and Neovim's agree.
- **Language**: `config.toml` declares `Decl`, suffix `decl`, `//`
  line and `/* */` block comments, the bracket pairs and their
  auto-close, tab size 4, and `$` as a word character (so `$this` and
  `name$` are one word). `decl.toml` and `decl.lock` are left to Zed's
  TOML.
- **Outline**: `outline.scm` lists `type`, `output`, `input`, `func`,
  `const`, `diagnostic`, `dimension`, `unit` declarations and record
  members, giving the outline panel, breadcrumbs, and symbol search.
- **Runnables**: `runnables.scm` tags a fixture's `@expect-*` header
  and an `output` declaration; the tags bind to task templates —
  `decl validate <dir>` for a fixture's corpus, `decl evaluate --output
  <name>` for an output — so Zed shows its run buttons in the gutter
  where VS Code shows lenses. The templates are given for
  `.zed/tasks.json` in the extension's README until Zed lets an
  extension ship them.

## 15. Zed: the server

`src/lib.rs` implements Zed's extension trait for the language server
`decl-lsp`:

- **Locating it**: the `lsp.decl-lsp.binary.path` setting first; then
  `decl-lsp` on `PATH` (any of the three implementations — installed
  from npm, PyPI, crates.io, or Homebrew — is accepted, and they answer
  identically); else the extension downloads the **prebuilt `decl-lsp`**
  for the platform from the language's GitHub release (the Rust
  implementation, one static binary per OS and architecture, a release
  asset the packaging workflow produces, §18) into Zed's extension
  work directory and keeps it current with the release.
- **Configuration**: `lsp.decl-lsp.settings` is forwarded as the
  server's workspace configuration (03_lsp.md §14) — the same keys as
  VS Code's `decl.inputs`, `decl.inlayHints.*`, and
  `decl.evaluate.idleDelay`, so one server reads one shape of settings
  from either editor; `lsp.decl-lsp.initialization_options` is passed
  through.
- **Restart, log, status**: Zed's own — `lsp: restart`, the LSP log
  view, the status in the activity bar.

## 16. Zed: what the editor shows

Zed's client surfaces the server's diagnostics, hover, completion and
signature help, go to definition / type definition / references,
document and workspace symbols, rename, formatting (and format on
save), inlay hints, and code actions (03_lsp.md §3–§11). Requests Zed's
client does not make — semantic tokens, code lenses, the call and type
hierarchies, linked editing — are not visible in Zed; the grammar's
queries stand in for the first, runnables for lenses, and the outline
for the hierarchies' most common use. Zed has no virtual documents, so
the output preview, the trace view, and the syntax tree of VS Code are
reached through the REPL in Zed's terminal — `decl repl` on the file,
`:evaluate`, `:trace`, and `:validate` — and through the task
templates. A Zed user misses nothing the language can say; they read
some of it in a terminal.

## 17. Other editors

Any editor with an LSP client has the same server, and any editor with
tree-sitter has the same highlighting — no extension, a configuration.
The grammar's `queries/` are the source every editor reads:
`highlights.scm`, `locals.scm`, `folds.scm`, `indents.scm`
(nvim-treesitter's dialect), `textobjects.scm` (the captures Helix and
nvim-treesitter-textobjects share), and `helix/indents.scm` for Helix's
indent dialect; Zed's `highlights.scm` is the grammar's verbatim, and
its Zed-only queries (brackets, indents, outline, runnables) sit with
the extension. `extension/zed/test.sh` loads every one of these against
the grammar and runs it over the fixture corpus.

- **Neovim** (0.10+, no plugins): `extension/neovim/init.lua` — the
  parser built with `tree-sitter build -o ~/.config/nvim/parser/decl.so`,
  the queries copied to `~/.config/nvim/queries/decl/`, the filetype,
  `vim.treesitter.start`, folding from the folds query, and
  `vim.lsp.start({ name = 'decl', cmd = { 'decl-lsp' } })` from a
  `FileType decl` autocommand. With nvim-treesitter, the same queries.
- **Helix**: `extension/helix/languages.toml` — the `[[language]]` entry
  with `language-servers = ["decl-lsp"]`, the grammar as a `[[grammar]]`
  source, the queries in the user runtime, `hx --grammar build`.
- **Emacs** (29+, built in `treesit` and eglot):
  `extension/emacs/decl-mode.el` — `decl-ts-mode` over the grammar
  library (`libtree-sitter-decl.dylib`/`.so` in `~/.emacs.d/tree-sitter/`
  or `treesit-extra-load-path`). `treesit` reads Lisp font-lock rules,
  not `.scm` files, so the highlights are mirrored there; the mode's
  smoke compiles every rule against the grammar so they cannot drift
  silently. Eglot is registered with `:language-id "decl"` (it would
  send `decl-ts` otherwise).
- **Vim** (9, no tree-sitter and no built-in client):
  `extension/vim/` — a regex `syntax/decl.vim` (an approximation of the
  grammar's highlighting), `ftdetect`, `ftplugin`, `indent`; the server
  through the yegappan/lsp plugin (`LspAddServer` with `filetype:
  'decl'`), vim-lsp for Vim 8.
- **Sublime Text**: `extension/sublime/` — a package (`Decl.sublime-syntax`,
  a regex syntax with a Sublime syntax test file, settings, comment
  preferences) copied to `Packages/Decl`; the server through the LSP
  package from Package Control, which the package's
  `LSP.sublime-settings` registers for `source.decl`.

`extension/smoke-editors.sh` sets every one of these up in a scratch
directory and checks what the user would see: every query loads, a
keyword is highlighted, a body folds or indents, an invalid fixture's
diagnostic appears, and hover on a type name shows its declaration.
Neovim and Emacs run headless; Helix and Vim on a pseudo-terminal, the
screen read back (Vim's hover popup does not render on a headless
terminal, so its hover is checked at the protocol); Sublime has no
headless mode, so its syntax and the test file run through syntect,
the engine that implements Sublime's syntax format, and the LSP client
is checked by hand in the editor. The script needs the editors
installed and is run by hand before a release.

The output preview, the trace view, and the Test Explorer are VS Code
UI; their content is reachable everywhere through the REPL and the
command line.

## 18. Packaging and release

**VS Code**: built with esbuild into two bundles (Node and browser),
packaged with `vsce` into a `.vsix`, and published to the Marketplace
and Open VSX by a workflow on the release tag, the extension's version
being the language's (0.3.x) and its bundled `decl-lang` the same
version. The `.vsix` is attached to the GitHub release for offline
installation.

**Zed**: published through the Zed extension registry — a pull request
to `zed-industries/extensions` adding `extension/zed/` as a submodule and its
entry to the registry's `extensions.toml`, and a version bump there on
each release; installable as a dev extension from the directory in the
meantime. The registry entry needs the grammar at a pinned revision
and, for the download path of §15, **prebuilt `decl-lsp` binaries** on
the GitHub release: the release workflow builds the Rust `decl-lsp` for
macOS, Linux, and Windows on arm64 and x86_64 — the Linux ones static
against musl, no glibc dependency — and attaches them as
`decl-lsp-<os>-<arch>` (`.exe` on Windows; the name §15's download
composes) — a deliverable Zed adds to the packaging.

`packaging/README.md` lists both channels beside npm, PyPI, crates.io,
and Homebrew.

## 19. Not applicable

No debug adapter (there is nothing to step: evaluation is one
deterministic function, and the trace view or `:trace` explains a value
after the fact); no notebook controller (the REPL's session file is the
durable form of an exploration); no telemetry; no settings UI beyond
the table of §10 and Zed's `lsp.decl-lsp` block.

## 20. Status

Scaffolded (2026-09-04), unpublished: `extension/vscode/` — the manifest with
the language, grammar (the site's TextMate grammar, copied at build),
snippets, commands, settings, task type, and problem matcher; the client
(`src/extension.ts`) with the bundled server, server selection and
restart, the output preview as a `decl-evaluate:` document, input
bindings, validation and tracing into the output channel, the REPL
terminal, the fixture runner as a task — and `extension/zed/` — the manifest
pointing at the grammar, the language configuration and the
`highlights` / `brackets` / `indents` / `outline` / `runnables` queries,
and the wasm module locating `decl-lsp` (setting, `PATH`, release
asset); then the Test Explorer for fixture corpora, the trace view, the
syntax-tree document, and the release workflow
(`.github/workflows/release.yml`: the prebuilt `decl-lsp` assets per
platform, the `.vsix`, publication when the tokens are configured).
The extension tests (`test/`: the runner on `@vscode/test-electron`, a
suite over `test/fixtures` checking the language contribution, the
commands, positioned diagnostics, hover/definition/completion/formatting
through the client, and the output preview's bytes) pass inside a
downloaded VS Code against the bundled server (`npm test -w
vscode-decl`; `.github/workflows/extension.yml` runs them under xvfb,
and its `vscode-rust` job runs the same suite against the Rust server
through `DECL_SERVER_PATH`). The server's `decl.*` commands are
registered by the language client and given their editor face through
its execute-command middleware (a lens, a palette entry, or a keybinding
all open the preview). The web extension (`src/web.ts`, the `browser`
entry) runs the server's core in a worker (`server/lsp-web.js`) over an
in-memory host — the reference implementation's modules, packages,
session, and server no longer touch a file system directly but a host
(`decl-ts/src/host.ts`), which Node binds to the disk and the worker to
the files the extension pushes with `decl/files` — with the output
preview and the syntax tree; a browser suite (`test/web/`, run by
`npm run test:web -w vscode-decl` through `@vscode/test-web` in a
headless Chromium) exercises it. The TextMate grammar is hand-written
and checked against the tree-sitter grammar's keywords at build
(`site/scripts/check-grammar.mjs`) — the check, not generation, is the
mechanism that keeps them together. The Zed extension has its own test
(`extension/zed/test.sh`, the `zed` job of the workflow): the wasm
module builds for Zed's target, the manifest names what Zed needs, and
every query loads against the tree-sitter grammar and runs over the
fixture corpus — the tests each extension can have, since Zed's
extension API has no views or commands to test (§1). The first release
(v0.3.0, 2026-09-05) attaches the `.vsix` and the `decl-lsp` binaries
of six platforms; the extension is on the Marketplace and Open VSX as
`luuvish.vscode-decl` (2026-09-05; the Open VSX namespace claim is
filed); the Zed registry entry is submitted
(zed-industries/extensions#7488, 2026-09-05), tested as a dev extension
at the submitted commit with the grammar fetched by https and the
server downloaded from the release. Open: the extension in
vscode.dev with a real workspace (the suite covers the mechanism, not
the site).

## 21. Verification

- **VS Code extension tests** (`@vscode/test-electron`, and
  `@vscode/test-web` for the browser entry) over the benchmark
  examples: every command of §9 produces the result the command line
  produces — a preview's text is `decl evaluate --output root`'s bytes,
  a fixture run is `decl validate <dir>`'s verdict — and the same
  session is run against the three servers by switching
  `decl.server.path`, with identical answers.
- **Grammar and query tests**: TextMate snapshot tests over every
  fixture of the corpus, regenerated when the tree-sitter grammar
  changes; Zed's queries checked against the grammar with the
  tree-sitter CLI (every capture resolves, every fixture parses and
  highlights) and the wasm module built with `cargo check --target
  wasm32-wasip1`.
- **CI** runs these on Linux; a release installs the `.vsix` in a clean
  VS Code and the dev extension in a clean Zed and opens the examples.
- **Manual smoke** on the three benchmark examples in VS Code and Zed
  closes each delivery, as the roadmap's Phase 6 exit criteria say;
  Neovim, Helix, Emacs, Vim, and Sublime Text through
  `extension/smoke-editors.sh` (§17) — passed on 2026-09-05 with Neovim
  0.12.5, Helix 25.07.1, Emacs 31.1, Vim 9.1, and Sublime Text 4200
  (syntect 5.3 for the syntax; the LSP package seen starting the server
  in the editor): the queries, the highlighting, folding or indentation,
  diagnostics, and hover.
