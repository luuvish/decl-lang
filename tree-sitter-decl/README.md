# tree-sitter-decl

The canonical Decl parser (ROADMAP Phase 1), written against
[spec chapter 11](../docs/specification/11_grammar.md). The newline
separator rule (spec §2.9) is implemented by the external scanner:
`NEWLINE` is emitted exactly where the parse state admits a separator,
which is the spec's "token before can end / token after can begin"
rule computed by `valid_symbols`. Nested block comments also live in
the scanner (regex cannot nest).

## Commands

```bash
npm install                 # once: tree-sitter CLI
npx tree-sitter generate    # grammar.js -> src/parser.c
npx tree-sitter test        # corpus tests (test/corpus/)
node ../tests/run_parsing.mjs   # judge the validation fixtures (parsing phase)
npx tree-sitter build --wasm    # needs emscripten or docker
npx tree-sitter playground      # web playground at http://127.0.0.1:8000
```

## Layout

- `grammar.js` — the grammar; GLR conflicts mirror the documented
  lookahead points of spec §11.8
- `src/scanner.c` — NEWLINE separator + nested block comments
- `test/corpus/` — parse-tree tests, including error-recovery smoke
- `queries/highlights.scm` — syntax highlighting captures
