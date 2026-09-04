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
  bundle: true, platform: 'node', format: 'cjs', target: 'node18',
  external: ['vscode'],
  outfile: join(root, 'dist/extension.js'),
  sourcemap: true,
});
mkdirSync(join(root, 'syntaxes'), { recursive: true });
copyFileSync(join(root, '../../site/grammars/decl.tmLanguage.json'), join(root, 'syntaxes/decl.tmLanguage.json'));
mkdirSync(join(root, 'server'), { recursive: true });
for (const f of ['lsp.js', 'cli.js', 'tree-sitter-decl.wasm', 'tree-sitter.wasm']) {
  const src = join(root, '../../decl-ts/dist', f);
  if (!existsSync(src)) throw new Error(`${src} missing: run \`npm run build -w decl-lang\` first`);
  copyFileSync(src, join(root, 'server', f));
}
console.log('built dist/extension.js, syntaxes/, server/');
