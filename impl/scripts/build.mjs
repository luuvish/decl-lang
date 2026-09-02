// Build the publishable artifacts: bundle the CLI, the LSP server, and
// the library entry (web-tree-sitter included) into dist/ as ESM for
// Node 20+, ship both wasm files next to them, carry the repository
// LICENSE into the package root, and mirror dist/ into the Python
// package (python/decl/_js) so pip users get the same bytes.
import { build } from 'esbuild';
import { mkdirSync, copyFileSync, existsSync, rmSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

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
copyFileSync('../tree-sitter-decl/tree-sitter-decl.wasm', 'dist/tree-sitter-decl.wasm');
copyFileSync('node_modules/web-tree-sitter/tree-sitter.wasm', 'dist/tree-sitter.wasm');
if (existsSync('../LICENSE')) copyFileSync('../LICENSE', 'LICENSE');

const py = '../python/decl/_js';
if (existsSync('../python/decl')) {
  rmSync(py, { recursive: true, force: true });
  mkdirSync(py, { recursive: true });
  for (const f of readdirSync('dist')) copyFileSync(join('dist', f), join(py, f));
  copyFileSync('../LICENSE', '../python/LICENSE');
  // the grammar C sources for the native Python parser extension
  const gsrc = '../tree-sitter-decl/src', gdst = '../python/decl/_tree_sitter/src';
  rmSync(gdst, { recursive: true, force: true });
  mkdirSync(join(gdst, 'tree_sitter'), { recursive: true });
  for (const f of ['parser.c', 'scanner.c']) copyFileSync(join(gsrc, f), join(gdst, f));
  for (const f of readdirSync(join(gsrc, 'tree_sitter'))) copyFileSync(join(gsrc, 'tree_sitter', f), join(gdst, 'tree_sitter', f));
  // ... and for the Rust crate (cargo publish needs them inside the package)
  const rdst = '../rust/grammar';
  if (existsSync('../rust')) {
    rmSync(rdst, { recursive: true, force: true });
    mkdirSync(join(rdst, 'tree_sitter'), { recursive: true });
    for (const f of ['parser.c', 'scanner.c']) copyFileSync(join(gsrc, f), join(rdst, f));
    for (const f of readdirSync(join(gsrc, 'tree_sitter'))) copyFileSync(join(gsrc, 'tree_sitter', f), join(rdst, 'tree_sitter', f));
    copyFileSync('../LICENSE', '../rust/LICENSE');
  }
}
console.log('built dist/ (cli.js, lsp.js, index.js, tree-sitter-decl.wasm, tree-sitter.wasm)' + (existsSync(py) ? ' + mirrored to python/decl/_js' : ''));
