# vscode-decl

The VS Code extension for [Decl](https://github.com/luuvish/decl-lang):
the language (syntax from the site's TextMate grammar, comments,
brackets, snippets), a client for `decl-lsp` — the reference server is
bundled, any implementation's `decl-lsp` runs by `decl.server.path` —
and the editor-side features: the live output preview
(`decl-evaluate:` documents beside the editor), input bindings
(`decl.inputs`), validation and tracing into the output channel, the
REPL terminal, `decl` tasks with the `$decl` problem matcher, and the
fixture runner. The design is
[docs/tooling/04_extension.md](../../docs/tooling/04_extension.md) §2–§13.

## Build and try

```bash
npm run build -w decl-lang        # the reference server the extension bundles
cd extension/vscode && npm run build   # dist/extension.js, syntaxes/, server/
```

Then *Run → Start Debugging* with this directory open, or
`npm run package` for a `.vsix` to install anywhere (*Extensions →
Install from VSIX…*).

## Status

Scaffolded, not yet published: the client and every command above are
written against the server's `decl.*` commands; the Test Explorer view,
the trace tree view, the syntax-tree document, and the web extension
(`src/web.ts`) follow, and the Marketplace / Open VSX release goes with
the language's next version.
