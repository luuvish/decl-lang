// Smoke-test the publishable artifact exactly the way a user gets it:
// `npm pack` the tarball, install it into a scratch project, and drive
// the installed `decl` / `decl-lsp` binaries and the library entry.
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, readFileSync, existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';

const impl = resolve(import.meta.dirname, '..');
const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { encoding: 'utf8', ...opts });
let pass = 0,
  fail = 0;
const check = (name, cond, detail = '') => {
  if (cond) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    fail++;
    console.log(`  FAIL ${name} ${detail}`);
  }
};

// 1. pack
const packOut = run('npm', ['pack', '--json', '--silent'], { cwd: impl });
const [{ filename, size, unpackedSize, files }] = JSON.parse(packOut);
const tarball = join(impl, filename);
check(
  `tarball built: ${filename} (${(size / 1024).toFixed(0)} KB packed, ${(unpackedSize / 1024).toFixed(0)} KB unpacked)`,
  existsSync(tarball),
);
const names = files.map((f) => f.path);
check(
  'tarball carries dist bins, wasm, README, LICENSE',
  [
    'dist/cli.js',
    'dist/lsp.js',
    'dist/index.js',
    'dist/tree-sitter-decl.wasm',
    'README.md',
    'LICENSE',
    'package.json',
  ].every((f) => names.includes(f)),
  names.join(' '),
);
check(
  'tarball ships no sources or tests',
  !names.some((f) => f.startsWith('src/') || f.startsWith('test/')),
);
const sha = createHash('sha256').update(readFileSync(tarball)).digest('hex');
console.log(`  sha256 ${sha}`);

// 2. install into a scratch project
const dir = mkdtempSync(join(tmpdir(), 'decl-smoke-'));
writeFileSync(join(dir, 'package.json'), '{"name":"smoke","private":true}');
run('npm', ['install', '--silent', '--no-audit', '--no-fund', tarball], { cwd: dir });
const bin = join(dir, 'node_modules', '.bin', 'decl');
check('decl binary installed', existsSync(bin));

// 3. drive the installed CLI
writeFileSync(
  join(dir, 't.decl'),
  'type T = { a: int, const b = a * 2 }\nexport output t: T = { a: 21 }\n',
);
const ev = spawnSync(bin, ['evaluate', join(dir, 't.decl'), '--root', 't'], { encoding: 'utf8' });
check(
  'installed decl evaluates',
  ev.status === 0 && ev.stdout.trim() === '{"a":21,"b":42}',
  ev.stderr + ev.stdout,
);
writeFileSync(join(dir, 'bad.decl'), 'type Bad = 10..3\n');
const chk = spawnSync(bin, ['check', join(dir, 'bad.decl')], { encoding: 'utf8' });
check(
  'installed decl checks (wasm resolved from dist/)',
  chk.status === 1 && chk.stderr.includes('E4011'),
  chk.stderr,
);
writeFileSync(join(dir, 'm.decl'), 'const x=1+2\n');
const fmt = spawnSync(bin, ['fmt', join(dir, 'm.decl')], { encoding: 'utf8' });
check(
  'installed decl fmt',
  fmt.status === 0 && readFileSync(join(dir, 'm.decl'), 'utf8') === 'const x = 1 + 2\n',
  fmt.stderr,
);

// 4. the LSP binary answers initialize
const lsp = join(dir, 'node_modules', '.bin', 'decl-lsp');
const body = JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} });
const exitBody = JSON.stringify({ jsonrpc: '2.0', method: 'exit' });
const frame = (b) => `Content-Length: ${Buffer.byteLength(b)}\r\n\r\n${b}`;
const l = spawnSync(lsp, [], {
  encoding: 'utf8',
  input: frame(body) + frame(exitBody),
  timeout: 20000,
});
check(
  'installed decl-lsp initializes, then exits on request',
  l.status === 0 && l.stdout.includes('"hoverProvider":true'),
  `status ${l.status} ${l.stderr.slice(0, 200)}`,
);

// 5. the library entry
writeFileSync(
  join(dir, 'lib.mjs'),
  `
import { initParser, parseSource, checkModule } from 'decl-lang';
await initParser();
const { decls, errors } = parseSource('type P = 1..65535');
console.log(JSON.stringify({ errors: errors.length, checks: checkModule(decls).length, decls: decls.length }));
`,
);
const lib = spawnSync('node', [join(dir, 'lib.mjs')], { encoding: 'utf8', cwd: dir });
check(
  'library entry importable',
  lib.status === 0 && lib.stdout.trim() === '{"errors":0,"checks":0,"decls":1}',
  lib.stderr + lib.stdout,
);

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
