// Build the publishable artifacts: bundle the CLI, the LSP server, and
// the library entry (web-tree-sitter included) into dist/ as ESM for
// Node 20+, plus the platform-neutral core (dist/core.js —
// `decl-lang/core`, what browsers such as the website's playground
// import); ship both wasm files next to them; carry the repository
// LICENSE into the package root; and copy the grammar sources into the
// sibling Python and Rust packages, which compile them natively.
import { build } from 'esbuild';
import { mkdirSync, copyFileSync, existsSync, rmSync, readdirSync } from 'node:fs';
import { join, dirname, basename, resolve } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const REPO = resolve('..');
const GRAMMAR = join(REPO, 'tree-sitter-decl');

// web-tree-sitter's runtime wasm sits beside its entry (hoisted or not)
let wtsDir = dirname(require.resolve('web-tree-sitter'));
while (!existsSync(join(wtsDir, 'tree-sitter.wasm')) && basename(wtsDir) !== 'web-tree-sitter') wtsDir = dirname(wtsDir);
const runtimeWasm = join(wtsDir, 'tree-sitter.wasm');

rmSync('dist', { recursive: true, force: true });
mkdirSync('dist', { recursive: true });
await build({
  entryPoints: ['src/cli.ts', 'src/lsp.ts', 'src/index.ts'],
  bundle: true,
  platform: 'node',
  format: 'esm',
  target: 'node20',
  outdir: 'dist',
  logLevel: 'info',
});
// the core has no Node imports of its own; web-tree-sitter's emscripten
// glue mentions Node built-ins inside `if (isNode)` branches, which stay
// external and are never reached in a browser
await build({
  entryPoints: ['src/core.ts', 'src/lsp-web.ts'],
  bundle: true,
  platform: 'browser',
  format: 'esm',
  target: 'es2022',
  minify: true,
  outdir: 'dist',
  logLevel: 'info',
  external: ['fs', 'fs/promises', 'path', 'url', 'crypto', 'module', 'worker_threads', 'perf_hooks', 'os', 'util', 'child_process'],
});
copyFileSync(join(GRAMMAR, 'tree-sitter-decl.wasm'), 'dist/tree-sitter-decl.wasm');
copyFileSync(runtimeWasm, 'dist/tree-sitter.wasm');
if (existsSync(join(REPO, 'LICENSE'))) copyFileSync(join(REPO, 'LICENSE'), 'LICENSE');

// the sibling packages (decl-py, decl-rs)
if (existsSync('../decl-py/decl')) {
  copyFileSync(join(REPO, 'LICENSE'), '../decl-py/LICENSE');
  // the grammar C sources for the native Python parser extension
  const gsrc = join(GRAMMAR, 'src'), gdst = '../decl-py/decl/_tree_sitter/src';
  rmSync(gdst, { recursive: true, force: true });
  mkdirSync(join(gdst, 'tree_sitter'), { recursive: true });
  for (const f of ['parser.c', 'scanner.c']) copyFileSync(join(gsrc, f), join(gdst, f));
  for (const f of readdirSync(join(gsrc, 'tree_sitter'))) copyFileSync(join(gsrc, 'tree_sitter', f), join(gdst, 'tree_sitter', f));
  // ... and for the Rust crate (cargo publish needs them inside the package)
  const rdst = '../decl-rs/grammar';
  if (existsSync('../decl-rs')) {
    rmSync(rdst, { recursive: true, force: true });
    mkdirSync(join(rdst, 'tree_sitter'), { recursive: true });
    for (const f of ['parser.c', 'scanner.c']) copyFileSync(join(gsrc, f), join(rdst, f));
    for (const f of readdirSync(join(gsrc, 'tree_sitter'))) copyFileSync(join(gsrc, 'tree_sitter', f), join(rdst, 'tree_sitter', f));
    copyFileSync(join(REPO, 'LICENSE'), '../decl-rs/LICENSE');
  }
}
console.log('built dist/ (cli.js, lsp.js, index.js, core.js, tree-sitter-decl.wasm, tree-sitter.wasm) + grammar sources synced to decl-py and decl-rs');
