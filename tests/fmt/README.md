# Formatter cases

The canonical form (docs/tooling/01_cli.md, `decl fmt`) case by case:
`cases.json` lists an input and the text the formatter must produce, or
`"error": true` for a text that does not parse and must be refused.

```json
{ "name": "spacing", "input": "const x=1+2*3\n", "expected": "const x = 1 + 2 * 3\n" }
{ "name": "a text that does not parse is refused", "input": "type T = {", "error": true }
```

Every implementation's formatter suite runs the cases
(`decl-ts/tests/fmt_test.ts`, `decl-rs/tests/fmt_test.rs`, `decl-py/tests/fmt_test.py`),
and then the formatter's two properties over every parseable module of
the corpora: idempotent (`fmt(fmt(x)) == fmt(x)`) and AST-preserving
(formatting moves columns, never nodes). The parity harness runs the
cases through the three `decl fmt` and requires the expected form of
the reference and identical bytes of the natives. A formatting rule
lands with its case here.
