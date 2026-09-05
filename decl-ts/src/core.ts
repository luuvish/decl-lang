// The platform-neutral core of decl-lang (`decl-lang/core`): everything
// that runs anywhere JavaScript runs — parser binding (given the grammar
// wasm's location), static checker, evaluator, single-module pipeline,
// canonical formatter. Modules, packages, the CLI, and the language
// server read the file system and live in the full entry (`decl-lang`).
export { initParser, parseSource, getLanguage } from './parse.ts';
export type { ParseResult, ParserOptions } from './parse.ts';
export { checkModule } from './checker.ts';
export { runPipeline, evaluateSource } from './pipeline.ts';
export type { Pipeline, Report } from './pipeline.ts';
export { format, initFormatter } from './fmt.ts';
export { Env, readJson, isArr, isMap, isRec, isRef, isQ } from './semantics.ts';
export type { Diag, RT } from './semantics.ts';
export { Engine } from './engine.ts';
export { subsumes, structurallyEmpty } from './subsume.ts';
export * from './ast.ts';
// Phase 6: the host, the session, and the language server's core run anywhere too
export { host, setHost, memoryHost, resolvePath, dirname, basename, join, relative, sha256Hex } from './host.ts';
export type { Host } from './host.ts';
export { loadModules, runUniverse } from './module.ts';
export type { Module, LoadResult, PackageResolver } from './module.ts';
export { openPackageUniverse, parseManifest, packageHash, writeLock, verifyLock, lockText } from './package.ts';
export { Session, SessionError, prettyJson, fmtDiag } from './session.ts';
export type { Op, BindSource, Document, Run, RootInfo } from './session.ts';
export { connect as connectLanguageServer } from './lsp-core.ts';
export type { Io as LanguageServerIo } from './lsp-core.ts';
