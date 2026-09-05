// The one version of the release, in every place that carries it
// (docs/DEVELOPMENT.md §7): prints and checks that they agree; `--set x.y.z`
// rewrites them all (make bump VERSION=x.y.z); `--check vX.Y.Z` also
// requires that tag. The Homebrew formula follows the npm publication and
// is not here.
import { readFileSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const places = [
  ['decl-ts/package.json', /("version":\s*")([^"]+)(")/],
  ['decl-ts/src/version.ts', /(export const VERSION = ')([^']+)(')/],
  ['extension/vscode/package.json', /("version":\s*")([^"]+)(")/],
  // the site depends on the workspace's decl-lang at exactly this version
  ['site/package.json', /("decl-lang":\s*")([^"]+)(")/],
  ['decl-rs/Cargo.toml', /(^version = ")([^"]+)(")/m],
  ['extension/zed/Cargo.toml', /(^version = ")([^"]+)(")/m],
  ['extension/zed/extension.toml', /(^version = ")([^"]+)(")/m],
  ['decl-py/pyproject.toml', /(^version = ")([^"]+)(")/m],
  ['decl-py/src/decl/api.py', /(^__version__ = ")([^"]+)(")/m],
];
const read = () =>
  places.map(([file, re]) => {
    const m = re.exec(readFileSync(join(root, file), 'utf8'));
    if (!m) throw new Error(`${file}: no version field`);
    return [file, m[2]];
  });

const args = process.argv.slice(2);
if (args[0] === '--set') {
  const v = args[1];
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(v ?? ''))
    throw new Error('usage: version.mjs --set x.y.z');
  for (const [file, re] of places) {
    const p = join(root, file);
    writeFileSync(p, readFileSync(p, 'utf8').replace(re, `$1${v}$3`));
  }
  // the lockfiles follow their manifests
  execFileSync(
    'npm',
    ['install', '--package-lock-only', '--ignore-scripts', '--no-audit', '--no-fund'],
    { cwd: root, stdio: 'inherit' },
  );
  execFileSync('cargo', ['update', '--offline', '-p', 'decl-lang'], {
    cwd: root,
    stdio: 'inherit',
  });
  execFileSync('cargo', ['update', '--offline', '-p', 'zed-decl'], {
    cwd: join(root, 'extension/zed'),
    stdio: 'inherit',
  });
}
const found = read();
const versions = new Set(found.map(([, v]) => v));
for (const [file, v] of found) console.log(`${v}\t${file}`);
if (versions.size !== 1) {
  console.error('the versions disagree');
  process.exit(1);
}
const [version] = versions;
if (args[0] === '--check' && args[1] && args[1].replace(/^v/, '') !== version) {
  console.error(`the tag ${args[1]} is not the version ${version}`);
  process.exit(1);
}
console.log(version);
