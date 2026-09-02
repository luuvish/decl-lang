// Bundle the reference implementation for the browser (the website's
// playground): src/web.ts -> ../site/public/playground/decl.js, with the
// two wasm files beside it. Node built-ins are mapped to src/web/shims.ts.
import { build } from 'esbuild';
import { mkdirSync, copyFileSync } from 'node:fs';
import { resolve } from 'node:path';

const out = resolve(process.argv[2] ?? '../site/public/playground');
mkdirSync(out, { recursive: true });
const shim = resolve('src/web/shims.ts');

await build({
  entryPoints: ['src/web.ts'],
  bundle: true,
  platform: 'browser',
  format: 'esm',
  target: 'es2022',
  minify: true,
  outfile: resolve(out, 'decl.js'),
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
copyFileSync('../tree-sitter-decl/tree-sitter-decl.wasm', resolve(out, 'tree-sitter-decl.wasm'));
copyFileSync('node_modules/web-tree-sitter/tree-sitter.wasm', resolve(out, 'tree-sitter.wasm'));
console.log(`built ${out}/decl.js (+ tree-sitter-decl.wasm, tree-sitter.wasm)`);
