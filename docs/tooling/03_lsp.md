# The language server — `decl-lsp`

`decl-lsp` speaks the Language Server Protocol over standard input and
output. It runs the same loader, checker, inference, and engine as the
`decl` command line — nothing it shows comes from a second analysis —
and every implementation ships one, answering the same scripted editor
session with the same messages (`tests/parity/differential.py`). This
document is informative: it specifies what the server offers, capability
by capability, at the level of rust-analyzer where that level applies.
Delivery order is the roadmap's (Phase 6); today's server provides the
diagnostics, hover, and definition of §3, §4, and §6.

## 1. Setup

Point an LSP client at the `decl-lsp` binary for the `decl` language
(file extension `.decl`); the workspace root is the directory holding
`decl.toml` when there is one, else the file's directory.

- **VS Code** and **Zed**: the extensions, [04. Editor extensions](04_extension.md)
  — VS Code's bundles this server and manages it, Zed's finds or
  downloads it; a generic LSP client works too (`command: decl-lsp`,
  `languageId: decl`).
- **Neovim**: `vim.lsp.start({ name = 'decl', cmd = { 'decl-lsp' },
  root_dir = vim.fs.root(0, { 'decl.toml', '.git' }) })` from a
  `FileType decl` autocommand.
- **Helix**: a `[[language]]` entry with `language-servers = ["decl-lsp"]`
  and `[language-server.decl-lsp] command = "decl-lsp"`.

Syntax highlighting comes from the grammar (`tree-sitter-decl/`, with
`queries/highlights.scm`), which editors with tree-sitter support load
directly; the server adds a semantic layer on top (§13).

## 2. Principles

- **One analysis.** Every answer comes from the checker, inference, and
  engine of the command line, over the same universe (entry, imports,
  package) the command line opens; the server keeps a **session object**
  per package — the REPL's (02_repl.md §1) — whose dependency tracking
  makes re-analysis incremental: a change re-checks its module and
  re-evaluates only what read it, and the result is observationally
  identical to a full pass (§9.4).
- **Open buffers override the disk** for every universe they take part
  in; a file the editor has not opened is read when needed and watched
  for changes (§14).
- **Positions are exact.** Every AST node carries its source range, and
  the checker records for every expression node its inferred type and the
  declaration a name resolves to, so any position answers "what is this,
  what type has it, where is it declared".
- **Messages are handled in order** (a client may pipe `initialize` and
  `exit` back to back), and the server exits when its input closes.

## 3. Diagnostics

`textDocument/publishDiagnostics` on open, on every change, and after
evaluation:

- **Syntax errors** positioned on the offending token; **static
  diagnostics** (§12's E3xxx/E4xxx) on the exact range of the construct
  they concern; severities `error`, `warning`, `information`, and `hint`
  from the diagnostic's own.
- **Evaluation diagnostics** — binding, assertions, dangling references,
  evaluation errors — on save or after an idle delay, for the exported
  outputs and for the inputs whose documents the workspace configuration
  binds, each at the literal entry that produced the value (or at the
  root when it came from a document).
- **Root causes** (§6.6) as `relatedInformation`: an invalid value points
  at the diagnostic that invalidated it; an assertion failure lists the
  members its condition read.
- **Advisory findings** (severity hint): an unused `import`, `const`,
  `func`, or non-exported type; a `?.` whose left side is definitely
  present; a restatement that repeats the computed value; a derived member
  read only by its siblings and constraints, never navigated from outside
  (a candidate for `x$`).
- Diagnostics carry their code (`E4050`) and the `Type.assert` id, and
  the quick fix for the code when one exists (§10).

## 4. Hover

- **An expression**: its inferred static type, the absence flag when it
  is maybe-absent, and — when the position lies in an evaluated root —
  its evaluated value.
- **A member declaration**: its kind (required / optional / defaulted /
  derived / hidden), type, default or expression, documentation (`///`,
  `@doc`), and for a member of an output its evaluated value.
- **A type**: its declaration and documentation, the `else` diagnostic
  it carries, the arms and discriminant of a union, the parent of an
  extension, the layers of an intersection.
- **A `std.*` function**: its signature and semantics line (§13). **A
  unit literal**: dimension, factor, and base-unit value. **A dimension
  or unit declaration**: its definition.
- **A context variable**: the declared bound (`$parent: ref<…>`) and the
  embedding site that satisfies it. **`$referrers(T, "m")`**: the edge it
  queries and the referrers' declaration.
- **An import**: the resolved module path (or package and version) and
  the declaration it names.

## 5. Completion and signature help

- Members after `.` and `?.` from the static type — hidden members as
  `x$` — with kind and type in the detail; on a union, the members every
  arm declares.
- Context variables where they are meaningful (inside a record's member
  expressions); `$referrers(` with the types that carry a `ref` to the
  enclosing type, then their referencing members.
- `std.` namespaces and functions with signatures; type names, generic
  parameters, dimensions in `quantity<…>`; unit symbols after a number
  literal; the literal keywords.
- Import completion: the names a module exports, the modules a package
  resolves, the packages the lock holds.
- Snippets: a record literal from the expected type with its required
  members as placeholders; `assert name: …`; `when … { }`; a `diagnostic`
  declaration.
- Signature help for `func` and `std.*` calls with the active parameter
  highlighted.

## 6. Navigation

- **Definition** for every name: declarations; members from a
  navigation, an object key, an override, or a `$referrers` member
  string; context declarations; imports one hop and through re-exports;
  units and dimensions.
- **Type definition**: from a member or expression to the declaration of
  its type; from `ref<T>` to `T`; from a value in a discriminated union to
  the arm it selects.
- **Declaration** of a member: the type (or parent type) that declares
  it, when the member at the cursor is inherited.
- **References** across the universe (every module the workspace's
  packages resolve), with `documentHighlight` for the occurrences in the
  open buffer.

## 7. Symbols and hierarchies

- **Document symbols**: an outline of the module — declarations with
  their members and `assert` names, context declarations, `when` groups.
- **Workspace symbols**: declarations across every module, searchable by
  name.
- **Type hierarchy**: supertypes and subtypes along extension chains
  (`Parent {…}`), intersection layers, and union arms.
- **Call hierarchy** for `func`: callers and callees, from the reference
  graph of §5.3.
- **Referrer hierarchy** for `ref` members: which types reference which
  through which member — the graph `$referrers` answers over.

## 8. Rename and linked editing

- **Rename** with `prepareRename`: declarations, members (with every
  navigation, object key, override, and `$referrers` string that names
  them), imports and their `as` names, units. It refuses a rename that
  would shadow (D27) or collide within a record's single name space
  (D19), and it renames a `Type.assert` id with its assert.
- **Linked editing**: a map key and the `name` restating it in the same
  literal; a type and the `else` diagnostic id derived from it.

## 9. Formatting, folding, and typing

- `textDocument/formatting` by the canonical formatter; no range
  formatting (the formatter keeps the author's line structure).
- **Folding ranges** for records, arrays, comprehensions, `when` groups,
  `match` arms, and comment blocks; **selection ranges** that expand from
  a token through the syntax tree.
- **Typing assists** (`onTypeFormatting`): a continuation line indented by
  the §2.9 rule, `///` continued on Enter, brackets closed.

## 10. Code actions

**Quick fixes**, keyed to the diagnostic's code:

| Code | Fix |
|---|---|
| E3003 unknown name | add the `import` from the workspace module that exports it, or qualify a namespace import |
| E4003 undeclared member | declare it on the type with the supplied value's inferred type, or quote a key |
| E4013 union not discriminable | add a literal-typed member to the arms |
| E4030 / E4032 override | change the override's type or kind to the parent's |
| E4050 absence discipline | insert `?.`, `?? …`, or an `in` guard |
| E4094 context variable undeclared | declare `$parent: ref<…>`, `$key: …`, or `$root: ref<…>` with the bound inferred from the uses |
| D16 precedence (`a && b ?? c`) | parenthesize |
| non-exhaustive `match` | add the missing arms |
| `const` in a record body; `x: T = e` a document supplies | `x = e`; `x?: T = e` |

**Assists**, on a selection or position:

- **Extract** an expression into a derived member `x = e`, a hidden member
  `x$ = e`, a module `const` (when it is a constant expression), or a
  `func` with the free names as parameters; **inline** the reverse.
- **Convert** derived ↔ hidden, defaulted ↔ derived, optional ↔ required;
  an inline record type ↔ a named `type`; an inline `else error` ↔ a
  `diagnostic` declaration; an `if` chain over a discriminant ↔ `match`;
  a unit literal to another unit of its dimension.
- **Generate** an `output` skeleton for a type (required members as
  placeholders), an `input` for a type, `export` for a declaration, and
  the `@expect-phase` / `@expect-error` header of a fixture from its
  current diagnostics.
- **Fill** the missing required members of a literal, or every member of
  a literal from its type.
- **Reorder** members into the canonical order (required, optional,
  defaulted, derived, hidden, constraints); flip the operands of a
  comparison.

## 11. Inlay hints

Each hint is a setting, off or on:

- the inferred type of a derived member without an annotation;
- parameter names at `func` and `std.*` call sites;
- the evaluated value of a defaulted or derived member of an output, at
  the end of its line;
- the base-unit value of a quantity literal (`250ms` → `0.25 s`);
- the bound of a context variable at its use, and the `$key` type of a
  map entry's type;
- closing-brace hints for long records (`} // Service`).

## 12. Code lenses, commands, virtual documents

- On an `output`: **evaluate** — opens its document in a live-updating
  virtual buffer (`decl-evaluate://…`) — and **references**.
- On an `input`: **validate with …** — binds a document from the
  workspace configuration or a file picker and shows its diagnostics.
- On a type: the count of references, and **instances** — the outputs and
  inputs whose values carry it.
- On a fixture (`@expect-*` header): **run**, judging it as `decl
  validate` would; on a corpus directory, the whole judgment.
- Commands (`workspace/executeCommand`): `decl.evaluate`,
  `decl.validate`, `decl.bindInput`, `decl.trace` (a path's derivation or
  root cause, as the REPL's `:trace`), `decl.openRepl` (a REPL attached to
  the workspace's session object), `decl.showSyntaxTree` (the tree-sitter
  tree of the buffer), `decl.reloadWorkspace`.

## 13. Semantic tokens

A layer over the grammar's highlights, carrying what the syntax tree
cannot know: a keyword used as a member name (D33); a member's kind —
required, optional, defaulted, derived, hidden — as modifiers; the kind
of a context variable; a unit symbol versus an identifier; an unresolved
name; a `$referrers` member string as a member reference. Full, range,
and delta requests.

## 14. Workspace

- One session object per package (`decl.toml`), multi-root workspaces
  supported; a file outside any package is its own universe.
- `workspace/didChangeWatchedFiles` for `.decl` files, `decl.toml`,
  `decl.lock`, and the bound input documents; a change re-validates what
  read it.
- `workspace/didChangeConfiguration`: the input bindings for evaluation
  diagnostics, the inlay-hint switches, the idle delay.
- `$/progress` for loading and evaluating a large universe; a status
  item with the universe's size and the last evaluation's time.

## 15. Protocol summary

| Request or notification | Section |
|---|---|
| `initialize`, `shutdown`, `exit` | §2 |
| `textDocument/didOpen` / `didChange` / `didSave` / `didClose` | §2 |
| `textDocument/publishDiagnostics` | §3 |
| `textDocument/hover` | §4 |
| `textDocument/completion`, `completionItem/resolve`, `textDocument/signatureHelp` | §5 |
| `textDocument/definition`, `typeDefinition`, `declaration`, `references`, `documentHighlight` | §6 |
| `textDocument/documentSymbol`, `workspace/symbol`, `typeHierarchy/*`, `callHierarchy/*` | §7 |
| `textDocument/prepareRename`, `rename`, `linkedEditingRange` | §8 |
| `textDocument/formatting`, `foldingRange`, `selectionRange`, `onTypeFormatting` | §9 |
| `textDocument/codeAction`, `codeAction/resolve` | §10 |
| `textDocument/inlayHint`, `inlayHint/resolve` | §11 |
| `textDocument/codeLens`, `workspace/executeCommand` | §12 |
| `textDocument/semanticTokens/full` / `range` / `delta` | §13 |
| `workspace/didChangeWatchedFiles`, `didChangeConfiguration`, `$/progress` | §14 |

## 16. Not applicable

rust-analyzer features with no counterpart in Decl: macro expansion,
lifetime and borrow-check hints, trait implementations, `cargo`
integration (build, run, debug), structural search and replace, memory
layout hovers, the debugger. There is no execution to step through:
evaluation is one deterministic function of the sources and the
documents, and `:trace` / `decl.trace` explains a value after the fact.

## 17. Status

Delivered (2026-09-04), in the three implementations and the parity
harness's scripted session: positioned diagnostics — parse errors,
loading, static checks anchored to the declaration or expression they
concern, evaluation diagnostics anchored to the literal their path leads
to (§3); hover with the declaration, its documentation, and the inferred
type (§4); completion over the session's engine (§5); definition, type
definition, references, and document highlights, imports followed (§6);
document symbols with record members (§7); prepare/rename across the
universe (§8); formatting and folding (§9); lenses on outputs and inputs
and the commands `decl.evaluate`, `decl.validate`, `decl.trace`,
`decl.reloadWorkspace` (§12); the input bindings by configuration (§14);
then signature help and workspace symbols (§5, §7), selection ranges
(§9), semantic tokens with member kinds and unresolved names (§13),
inlay hints — inferred types, parameter names, base-unit values (§11) —
the call and type hierarchies (§7), the quick fixes *import the name*
and *add the missing member* and the assist *annotate* (§10), and
`decl.showSyntaxTree` (§12). Open: linked editing, on-type formatting,
the remaining quick fixes and assists, the value and context-variable
hints, `$/progress`, the virtual documents beyond the syntax tree.

## 18. Verification

The parity harness drives a scripted session and requires the three
servers to answer with identical messages: every request kind of §15
with at least one position per kind of answer, every quick fix and
assist with a before and after, every lens with its command; a session
with an edit checks that the incremental analysis answers as a fresh
server would. Manual smoke in Neovim, Helix, and VS Code on the three
benchmark examples closes each delivery.
