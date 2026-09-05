// Bundle the web suite for the browser (mocha's browser build inside)
import { build } from 'esbuild';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = join(dirname(fileURLToPath(import.meta.url)), '..');
await build({
  entryPoints: [join(root, 'test/web/index.ts')],
  bundle: true,
  platform: 'browser',
  format: 'cjs',
  target: 'es2022',
  outfile: join(root, 'dist/test/web/index.js'),
  external: ['vscode'],
  define: { 'process.env.NODE_ENV': '"test"' },
});
console.log('built dist/test/web');
