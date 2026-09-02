// Public API of the decl-lang package: the platform-neutral core plus
// the Node.js side — parser setup from disk, modules and packages, the
// corpus judge — the same building blocks the `decl` CLI and `decl-lsp`
// server are made of. Browsers import `decl-lang/core` instead.
export * from './core.ts';
export { initParser, grammarPath } from './node.ts';   // shadows core's: locates the grammar on disk
export { loadModules, runUniverse } from './module.ts';
export type { Module, LoadResult, PackageResolver } from './module.ts';
export { openPackageUniverse, parseManifest, packageHash, writeLock, verifyLock, lockText } from './package.ts';
export { judgeCorpus, judgeFixture, walkDecl } from './conformance.ts';
