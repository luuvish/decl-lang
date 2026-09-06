/**
 * The platform-neutral core of `decl-lang` (`decl-lang/core`): everything
 * that runs anywhere JavaScript runs — the parser binding (given the
 * grammar wasm's location), the static checker, the evaluator, the
 * single-module pipeline, the canonical formatter, and, over a host, the
 * modules and packages, the session, and the language server's core. The
 * file system, the command line, and the stdio server live in the full
 * entry (`decl-lang`); the website's playground and the browser's worker
 * import this one.
 *
 * @packageDocumentation
 */
/** The parser binding: `initParser` loads the grammar's wasm; `parseSource` lowers a text to the AST (chapter 11). */
export { initParser, parseSource, getLanguage } from './parse.ts';
export type { ParseResult, ParserOptions } from './parse.ts';
/** The static checks of a module (§3, §4, §6, §9.7); the hooks record types and resolutions for a client. */
export { checkModule } from './checker.ts';
/** One module end to end: `runPipeline` over declarations, `evaluateSource` over text with a source-level report. */
export { runPipeline, evaluateSource } from './pipeline.ts';
export type { Pipeline, Report } from './pipeline.ts';
/** The canonical formatter (`decl fmt`): idempotent, AST-preserving, the author's line structure kept (§2.9). */
export { format, initFormatter } from './fmt.ts';
/** Values and environments: `Env` loads declarations and resolves types; `readJson` reads a document (§10.2); the `is*` guards tell a value's kind. */
export { Env, readJson, isArr, isMap, isRec, isRef, isQ, parsePath, segText } from './semantics.ts';
export type { Diag, RT, Seg } from './semantics.ts';
/** The evaluator (§9): binds, forces lazily with dependency tracking, validates, serializes. */
export { Engine } from './engine.ts';
/** The subsumption judgment ⊑ (§3.17) and structural emptiness (§3.19). */
export { subsumes, structurallyEmpty } from './subsume.ts';
/** The syntax tree (chapter 11): declarations, types, members, expressions, source ranges. */
export * from './ast.ts';
/** The host the core reads files through: the current one, an in-memory one for browsers, and the path helpers. */
export {
  host,
  setHost,
  memoryHost,
  resolvePath,
  dirname,
  basename,
  join,
  relative,
  sha256Hex,
} from './host.ts';
export type { Host } from './host.ts';
/** Modules and the universe (§8): load an entry's graph, evaluate every root of every module. */
export { loadModules, runUniverse } from './module.ts';
export type { Module, LoadResult, PackageResolver } from './module.ts';
/** Packages (§8.6–8.7): the manifest, the closed set of dependencies, the content hash, the lock file. */
export {
  openPackageUniverse,
  parseManifest,
  packageHash,
  writeLock,
  verifyLock,
  lockText,
} from './package.ts';
/** The evaluation session behind the REPL and the server: an operation log with undo and redo, incremental re-evaluation. */
export { Session, SessionError, prettyJson, fmtDiag } from './session.ts';
export type { Op, BindSource, Document, Run, RootInfo } from './session.ts';
/** The language server's core over any transport: `connect` feeds it messages, `Io` is what it needs of the transport; `diagnosticsFor` is its diagnostics of one document, anchored to source ranges. */
export { connect as connectLanguageServer, diagnosticsFor } from './lsp-core.ts';
export type { Io as LanguageServerIo } from './lsp-core.ts';
