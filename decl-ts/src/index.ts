// Public API of the decl-lang package: the platform-neutral core plus
// the Node.js side — parser setup from disk, modules and packages, the
// corpus judge — the same building blocks the `decl` CLI and `decl-lsp`
// server are made of. Browsers import `decl-lang/core` instead.
export * from './core.ts';
// the high-level API: evaluate / check / validate / formatSource (reads files)
export { evaluate, check, validate, formatSource, DeclError } from './api.ts';
export type { Diagnostic, EvaluateOptions, InputDocument, JsonValue } from './api.ts';
export { initParser, grammarPath, nodeHost } from './node.ts'; // shadows core's: locates the grammar on disk, installs the file-system host
export { judgeCorpus, judgeFixture, walkDecl } from './conformance.ts';
export { Repl, runRepl, needsMore, COMMANDS } from './repl.ts';
