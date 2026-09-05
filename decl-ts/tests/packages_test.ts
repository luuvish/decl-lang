// Packages, manifests, and the lock file (§8.6–8.7) through the package
// corpus (tests/packages/cases.json): the manifest and resolution errors
// as the command line reports them, and the lock — written by the API into
// a copy of the package, bit for bit the committed text, verified clean,
// then failing closed on content drift, version drift, and a missing entry.
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, appendFileSync, mkdtempSync, cpSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { initParser } from '../src/node.ts';
import { openPackageUniverse, writeLock, verifyLock } from '../src/package.ts';
import { check, total, root, cli } from './common/check.ts';

await initParser();
const cases = JSON.parse(readFileSync(join(root, 'tests/packages/cases.json'), 'utf8'));

/** the codes `decl check` reports for an entry, in order */
const codesOf = (entry: string): string[] => {
  const r = spawnSync(process.execPath, [cli, 'check', entry], { encoding: 'utf8', cwd: root });
  return [...r.stderr.matchAll(/\[(E\d{4})\]/g)].map((m) => m[1]);
};

console.log('== packages: manifest and resolution errors ==');
for (const e of cases.errors)
  check(
    `${e.entry}: ${e.codes.join(', ')}`,
    JSON.stringify(codesOf(e.entry)) === JSON.stringify(e.codes),
    JSON.stringify(codesOf(e.entry)),
  );

console.log('== packages: the lock file, reproducible and fail-closed ==');
{
  const lock = cases.lock;
  const expected = readFileSync(join(root, lock.lock), 'utf8');
  // a fresh copy of the package, with its lock written by the API
  const fresh = (): { dir: string; entry: string } => {
    const dir = mkdtempSync(join(tmpdir(), 'decl-pkg-'));
    cpSync(join(root, lock.package), dir, { recursive: true });
    const entry = join(dir, lock.entry);
    writeLock(openPackageUniverse(entry)!);
    return { dir, entry };
  };
  const { dir, entry } = fresh();
  check(
    'the lock is the committed text, byte for byte',
    readFileSync(join(dir, 'decl.lock'), 'utf8') === expected,
  );
  check('a fresh lock verifies clean', verifyLock(openPackageUniverse(entry)!).length === 0);
  check('the command line accepts the locked package', codesOf(entry).length === 0);
  rmSync(dir, { recursive: true, force: true });
  for (const d of lock.drift) {
    const { dir, entry } = fresh();
    for (const [file, text] of Object.entries(d.append ?? {}))
      appendFileSync(join(dir, file), text as string);
    if (d.lock_replace)
      writeFileSync(
        join(dir, 'decl.lock'),
        readFileSync(join(dir, 'decl.lock'), 'utf8').replace(d.lock_replace[0], d.lock_replace[1]),
      );
    if (d.lock_text !== undefined) writeFileSync(join(dir, 'decl.lock'), d.lock_text);
    const got = codesOf(entry);
    check(d.name, JSON.stringify(got) === JSON.stringify(d.codes), JSON.stringify(got));
    rmSync(dir, { recursive: true, force: true });
  }
}
total();
