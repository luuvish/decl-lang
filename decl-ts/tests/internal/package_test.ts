// package (tests/internal/checks.json): the manifest reader and the
// package hash the lock file rests on.
import { mkdtempSync, writeFileSync, readFileSync, cpSync, appendFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { initParser } from '../../src/node.ts';
import { parseManifest, packageHash } from '../../src/package.ts';
import { check, total, root } from '../common/check.ts';

await initParser();
{
  const dir = mkdtempSync(join(tmpdir(), 'decl-manifest-'));
  const read = (text: string): { m: any; codes: string[] } => {
    writeFileSync(join(dir, 'decl.toml'), text);
    const codes: string[] = [];
    const m = parseManifest(join(dir, 'decl.toml'), (c) => codes.push(c));
    return { m, codes };
  };
  const good = read('name = "app"\nversion = "1.0.0"\n\n[dependencies]\ncorelib = "1.0.0"\n');
  const bad = read('name = "app"\nversion = "1.0.0"\nflavor = "x"\n\n[dependencies]\ncorelib = "^1.0"\n');
  check(
    'manifest',
    good.codes.length === 0 &&
      good.m?.name === 'app' &&
      good.m?.version === '1.0.0' &&
      good.m?.dependencies.get('corelib') === '1.0.0' &&
      bad.codes.includes('E3011') &&
      bad.codes.includes('E3012'),
    JSON.stringify({ good: good.codes, bad: bad.codes }),
  );
}
{
  const corelib = join(root, 'tests/packages/app/decl_modules/corelib');
  const locked = readFileSync(join(root, 'tests/packages/lock/decl.lock'), 'utf8').trim().split(' ')[2];
  const h1 = packageHash(corelib);
  const copy = mkdtempSync(join(tmpdir(), 'decl-hash-'));
  cpSync(corelib, copy, { recursive: true });
  appendFileSync(join(copy, 'types/base.decl'), '// drift\n');
  check(
    'hash',
    h1 === locked && packageHash(corelib) === h1 && packageHash(copy) !== h1,
    `${h1} vs ${locked}`,
  );
}
total();
