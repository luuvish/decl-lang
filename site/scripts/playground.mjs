// Put decl-lang's browser bundle (dist/web.js + the two wasm files) under
// public/playground/ for the playground page. In the workspace,
// `decl-lang` is packages/typescript; build it first if needed.
import { existsSync, mkdirSync, copyFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { createRequire } from 'node:module';
import { execSync } from 'node:child_process';

const require = createRequire(import.meta.url);
const pkg = dirname(require.resolve('decl-lang/package.json'));
const dist = join(pkg, 'dist');
if (!existsSync(join(dist, 'web.js'))) execSync('npm run build', { cwd: pkg, stdio: 'inherit' });
const out = resolve(import.meta.dirname, '../public/playground');
mkdirSync(out, { recursive: true });
copyFileSync(join(dist, 'web.js'), join(out, 'decl.js'));
for (const f of ['tree-sitter-decl.wasm', 'tree-sitter.wasm']) copyFileSync(join(dist, f), join(out, f));
console.log(`playground: copied decl-lang/dist -> ${out}`);
