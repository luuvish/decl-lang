// Build the extension: bundle the client, copy the TextMate grammar (the
// site's, the single source: site/grammars/decl.tmLanguage.json), and
// copy the bundled reference server (decl-lang's dist/lsp.js with the
// grammar wasm) into server/.
import { build } from 'esbuild';
import { copyFileSync, mkdirSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');
await build({
  entryPoints: [join(root, 'src/extension.ts')],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node18',
  external: ['vscode'],
  outfile: join(root, 'dist/extension.js'),
  sourcemap: true,
});
// the browser entry (vscode.dev / github.dev): the worker server beside it
await build({
  entryPoints: [join(root, 'src/web.ts')],
  bundle: true,
  platform: 'browser',
  format: 'cjs',
  target: 'es2022',
  external: ['vscode'],
  outfile: join(root, 'dist/web.js'),
  sourcemap: true,
});
// the TextMate grammar is the site's; it must agree with the tree-sitter grammar's keywords
const { checkGrammar } = await import(join(root, '../../site/scripts/check-grammar.mjs'));
console.log(`grammar check: ${checkGrammar()} keywords agree`);
mkdirSync(join(root, 'syntaxes'), { recursive: true });
copyFileSync(
  join(root, '../../site/grammars/decl.tmLanguage.json'),
  join(root, 'syntaxes/decl.tmLanguage.json'),
);
mkdirSync(join(root, 'server'), { recursive: true });
// the LICENSE travels with the package, as it does in decl-ts, decl-rs, decl-py
copyFileSync(join(root, '../../LICENSE'), join(root, 'LICENSE'));
// the Node bundles are ES modules: they keep that as .mjs inside this
// CommonJS package (the client forks the server with node); the worker
// server is a classic script
for (const [f, to] of [
  ['lsp.js', 'lsp.mjs'],
  ['cli.js', 'cli.mjs'],
  ['lsp-web.js', 'lsp-web.js'],
  ['tree-sitter-decl.wasm', 'tree-sitter-decl.wasm'],
  ['tree-sitter.wasm', 'tree-sitter.wasm'],
]) {
  const src = join(root, '../../decl-ts/dist', f);
  if (!existsSync(src)) throw new Error(`${src} missing: run \`npm run build -w decl-lang\` first`);
  copyFileSync(src, join(root, 'server', to));
}
console.log('built dist/extension.js, dist/web.js, syntaxes/, server/');
