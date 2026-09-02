// The Node.js side of the parser setup: locate the grammar wasm on disk
// (beside the bundled dist/ files in the published package, under
// tree-sitter-decl/ in the source tree) and initialize the platform-
// neutral parser with it. Browsers call core's initParser with URLs.
import { existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initParser as initCore } from './parse.ts';
import type { ParserOptions } from './parse.ts';

const here = dirname(fileURLToPath(import.meta.url));

/** the grammar wasm's path on this machine */
export function grammarPath(): string {
  return [join(here, 'tree-sitter-decl.wasm'), join(here, '../../tree-sitter-decl/tree-sitter-decl.wasm')]
    .find(p => existsSync(p)) ?? join(here, 'tree-sitter-decl.wasm');
}

/** initialize the parser from disk; options override the located files */
export async function initParser(opts: Partial<ParserOptions> = {}): Promise<void> {
  // web-tree-sitter's runtime (tree-sitter.wasm) is shipped beside the
  // bundled files; in the source tree it resolves from node_modules
  const local = join(here, 'tree-sitter.wasm');
  await initCore({ grammar: opts.grammar ?? grammarPath(), runtime: opts.runtime ?? (existsSync(local) ? local : undefined) });
}
