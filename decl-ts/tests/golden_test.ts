// The golden manifest (tests/golden/manifest.json) through the reference
// command line: every entry's bytes, exactly (tests/golden/README.md).
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const cli = join(root, 'decl-ts/src/cli.ts');
let pass = 0,
  fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    fail++;
    console.log(`  FAIL ${name} ${detail}`);
  }
};

type Entry = {
  module?: string;
  markdown?: string;
  inputs?: string[];
  output?: string;
  rejected?: boolean;
  golden: string;
};

// a markdown entry's module: its ```decl blocks in order, in a temporary file
const tmp = mkdtempSync(join(tmpdir(), 'decl-golden-'));
const moduleOf = (e: Entry): string => {
  if (e.module) return e.module;
  const md = readFileSync(join(root, e.markdown!), 'utf8');
  const src = [...md.matchAll(/```decl\n([\s\S]*?)```/g)].map((m) => m[1]).join('\n');
  const p = join(tmp, 'guide.decl');
  writeFileSync(p, src);
  return p;
};

console.log('== golden: the manifest, byte for byte ==');
const manifest: Entry[] = JSON.parse(
  readFileSync(join(root, 'tests/golden/manifest.json'), 'utf8'),
);
for (const e of manifest) {
  const args = [e.rejected ? 'validate' : 'evaluate', moduleOf(e)];
  for (const spec of e.inputs ?? []) args.push('--input', spec);
  if (e.output) args.push('--output', e.output);
  const r = spawnSync('node', [cli, ...args], { encoding: 'utf8', cwd: root });
  const expected = readFileSync(join(root, e.golden), 'utf8');
  const wantExit = e.rejected ? 1 : 0;
  const got = e.rejected ? r.stderr : r.stdout;
  check(
    e.golden,
    r.status === wantExit && got === expected,
    `exit ${r.status}, ${e.rejected ? 'stderr' : 'stdout'} ${got === expected ? 'same' : 'differs: ' + got.slice(0, 200)}`,
  );
}
console.log(`TOTAL ${pass} ok, ${fail} failed`);
process.exit(fail ? 1 : 0);
