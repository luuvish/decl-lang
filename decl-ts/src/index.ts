/**
 * The public API of the `decl-lang` package: the platform-neutral core
 * (`decl-lang/core`, re-exported whole) plus the Node.js side — the parser
 * set up from disk, the file-system host, the high-level API over files,
 * the corpus judge, the REPL — the same building blocks the `decl` command
 * line and the `decl-lsp` server are made of. Browsers import
 * `decl-lang/core` instead.
 *
 * @packageDocumentation
 */
export * from './core.ts';

/**
 * The high-level API in the command line's vocabulary: `evaluate` binds
 * input documents and returns the requested roots by name, `check` reports
 * a module universe's static findings, `validate` a bound document's
 * diagnostics, `formatSource` the canonical form; a failure is a
 * `DeclError` carrying the report. The Rust crate and the Python package
 * offer the same functions with the same semantics.
 */
export {
  evaluate,
  render,
  check,
  validate,
  formatSource,
  toJson,
  toYaml,
  DeclError,
} from './api.ts';
export type {
  Diagnostic,
  EvaluateOptions,
  RenderOptions,
  Rendered,
  TemplateSource,
  InputDocument,
  JsonValue,
} from './api.ts';

/**
 * The Node.js platform: `initParser` locates the grammar's wasm files on
 * disk (shadowing the core's, which takes a URL) and installs the
 * file-system host; `grammarPath` says where; `nodeHost` is that host.
 */
export { initParser, grammarPath, nodeHost } from './node.ts';

/** The fixture corpus judge (`decl validate <dir>`, tests/validation/README.md). */
export { judgeCorpus, judgeFixture, walkDecl } from './conformance.ts';

/** The REPL (docs/tooling/02_repl.md): the loop, its runner, its continuation rule, and its command table. */
export { Repl, runRepl, needsMore, COMMANDS } from './repl.ts';
