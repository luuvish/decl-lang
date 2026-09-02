// Public API of the decl-lang package: parse, check, load/link modules,
// evaluate, format, and judge fixture corpora — the same building blocks
// the `decl` CLI and `decl-lsp` server are made of.
export { initParser, parseSource, WASM } from './parse.ts';
export type { ParseResult } from './parse.ts';
export { checkModule } from './checker.ts';
export { loadModules, runUniverse } from './module.ts';
export type { Module, LoadResult, PackageResolver } from './module.ts';
export { openPackageUniverse, parseManifest, packageHash, writeLock, verifyLock, lockText } from './package.ts';
export { format, initFormatter } from './fmt.ts';
export { judgeCorpus, judgeFixture, runPipeline, walkDecl } from './conformance.ts';
export { Env, readJson, isArr, isMap, isRec, isRef, isQ } from './semantics.ts';
export type { Diag, RT } from './semantics.ts';
export { Engine } from './engine.ts';
export { subsumes, structurallyEmpty } from './subsume.ts';
export * from './ast.ts';
