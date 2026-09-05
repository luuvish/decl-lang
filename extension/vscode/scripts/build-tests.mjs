// Bundle the test runner and the suite for VS Code (CommonJS, `vscode` external)
import { build } from 'esbuild';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = join(dirname(fileURLToPath(import.meta.url)), '..');
await build({
  entryPoints: [join(root, 'test/runTest.ts'), join(root, 'test/runWebTest.ts')],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node18',
  outdir: join(root, 'dist/test'),
  external: ['vscode', '@vscode/test-electron', '@vscode/test-web'],
});
await build({
  entryPoints: [join(root, 'test/suite/index.ts'), join(root, 'test/suite/extension.test.ts')],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node18',
  outdir: join(root, 'dist/test/suite'),
  external: ['vscode', 'mocha'],
});
console.log('built dist/test');
