// Put the grammar's wasm files under public/playground/ for the playground
// page; the page itself imports `decl-lang/core` and Vite bundles it. In
// the workspace, `decl-lang` is decl-typescript; build it first if needed.
import { existsSync, mkdirSync, copyFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { createRequire } from 'node:module';
import { execSync } from 'node:child_process';

const require = createRequire(import.meta.url);
const pkg = dirname(require.resolve('decl-lang/package.json'));
const dist = join(pkg, 'dist');
if (!existsSync(join(dist, 'core.js'))) execSync('npm run build', { cwd: pkg, stdio: 'inherit' });
const out = resolve(import.meta.dirname, '../public/playground');
mkdirSync(out, { recursive: true });
for (const f of ['tree-sitter-decl.wasm', 'tree-sitter.wasm']) copyFileSync(join(dist, f), join(out, f));
console.log(`playground: copied the grammar wasm files -> ${out}`);
