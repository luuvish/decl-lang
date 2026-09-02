// Build the publishable artifacts: bundle the CLI, the LSP server, and
// the library entry (web-tree-sitter included) into dist/ as ESM for
// Node 20+, plus the browser bundle (dist/web.js — `decl-lang/web`, what
// the website's playground runs) with Node built-ins shimmed; ship both
// wasm files next to them; carry the repository LICENSE into the package
// root; and mirror dist/ and the grammar sources into the sibling Python
// and Rust packages so every channel ships the same bytes.
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
const shim = resolve('src/web/shims.ts');
await build({
  entryPoints: ['src/web.ts'],
  bundle: true,
  platform: 'browser',
  format: 'esm',
  target: 'es2022',
  minify: true,
  outfile: 'dist/web.js',
  logLevel: 'info',
  plugins: [{
    name: 'node-shims',
    setup(b) {
      b.onResolve(
        { filter: /^(node:)?(fs|fs\/promises|path|url|crypto|module|worker_threads|perf_hooks|os|util|child_process)$/ },
        () => ({ path: shim }),
      );
    },
  }],
});
copyFileSync(join(GRAMMAR, 'tree-sitter-decl.wasm'), 'dist/tree-sitter-decl.wasm');
copyFileSync(runtimeWasm, 'dist/tree-sitter.wasm');
if (existsSync(join(REPO, 'LICENSE'))) copyFileSync(join(REPO, 'LICENSE'), 'LICENSE');

// the sibling packages (decl-python, decl-rust)
const py = '../decl-python/decl/_js';
if (existsSync('../decl-python/decl')) {
  rmSync(py, { recursive: true, force: true });
  mkdirSync(py, { recursive: true });
  for (const f of readdirSync('dist')) copyFileSync(join('dist', f), join(py, f));
  copyFileSync(join(REPO, 'LICENSE'), '../decl-python/LICENSE');
  // the grammar C sources for the native Python parser extension
  const gsrc = join(GRAMMAR, 'src'), gdst = '../decl-python/decl/_tree_sitter/src';
  rmSync(gdst, { recursive: true, force: true });
  mkdirSync(join(gdst, 'tree_sitter'), { recursive: true });
  for (const f of ['parser.c', 'scanner.c']) copyFileSync(join(gsrc, f), join(gdst, f));
  for (const f of readdirSync(join(gsrc, 'tree_sitter'))) copyFileSync(join(gsrc, 'tree_sitter', f), join(gdst, 'tree_sitter', f));
  // ... and for the Rust crate (cargo publish needs them inside the package)
  const rdst = '../decl-rust/grammar';
  if (existsSync('../decl-rust')) {
    rmSync(rdst, { recursive: true, force: true });
    mkdirSync(join(rdst, 'tree_sitter'), { recursive: true });
    for (const f of ['parser.c', 'scanner.c']) copyFileSync(join(gsrc, f), join(rdst, f));
    for (const f of readdirSync(join(gsrc, 'tree_sitter'))) copyFileSync(join(gsrc, 'tree_sitter', f), join(rdst, 'tree_sitter', f));
    copyFileSync(join(REPO, 'LICENSE'), '../decl-rust/LICENSE');
  }
}
console.log('built dist/ (cli.js, lsp.js, index.js, web.js, tree-sitter-decl.wasm, tree-sitter.wasm)' + (existsSync(py) ? ' + mirrored to decl-python/decl/_js' : ''));
