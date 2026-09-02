// `decl` CLI end-to-end (Phase 4 exit criterion: `decl validate tests/`
// judges the full fixture corpus).
import { spawnSync } from 'node:child_process';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { writeFileSync, readFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const cli = join(root, 'decl-typescript/src/cli.ts');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};
const run = (...args: string[]) => {
  const r = spawnSync('node', [cli, ...args], { encoding: 'utf8' });
  return { code: r.status ?? -1, out: r.stdout, err: r.stderr };
};

console.log('== decl check ==');
{
  const ok = run('check', join(root, 'tests/validation/types/valid/predicates.decl'));
  check('valid file exits 0', ok.code === 0, ok.err);
  const bad = run('check', join(root, 'tests/validation/types/invalid/empty_range.decl'));
  check('invalid file exits 1 with the code', bad.code === 1 && bad.err.includes('E4011'), bad.err);
  const multi = run('check', join(root, 'tests/modules/basic/main.decl'));
  check('module universe checks', multi.code === 0, multi.err);
}

console.log('== decl evaluate ==');
{
  const dir = mkdtempSync(join(tmpdir(), 'decl-cli-'));
  const f = join(dir, 'demo.decl');
  writeFileSync(f, 'type T = { a: int, const b = a * 2 }\nexport output demo: T = { a: 21 }\n');
  const r = run('evaluate', f, '--root', 'demo');
  check('evaluate prints canonical JSON', r.code === 0 && r.out.trim() === '{"a":21,"b":42}', JSON.stringify(r));
  const all = run('evaluate', join(root, 'tests/modules/basic/main.decl'));
  check('evaluate emits every universe root', all.code === 0 && all.out.includes('"capped":16') && all.out.includes('"net":'), all.out.slice(0, 120));
}

console.log('== decl validate ==');
{
  const corpus = run('validate', join(root, 'tests/validation'));
  check('full corpus judged clean', corpus.code === 0 && / ok, 0 failed/.test(corpus.err), corpus.err.slice(-200));

  const dir = mkdtempSync(join(tmpdir(), 'decl-cli-'));
  const mod = join(dir, 'cfg.decl');
  writeFileSync(mod, 'type Cfg = { port: 1..65535, ... }\ninput deployed: Cfg\n');
  const doc = join(dir, 'doc.json');
  writeFileSync(doc, '{"port": 70000}');
  const exp = run('validate', mod, '--input', `deployed=${doc}`, '--expect-errors', 'E4001');
  check('--expect-errors matches', exp.code === 0, exp.err);
  const noexp = run('validate', mod, '--input', `deployed=${doc}`);
  check('unexpected errors exit 1', noexp.code === 1 && noexp.err.includes('E4001'), noexp.err);
}

console.log('== decl fmt ==');
{
  const dir = mkdtempSync(join(tmpdir(), 'decl-cli-'));
  const f = join(dir, 'messy.decl');
  writeFileSync(f, 'const x=1+2\ntype T = {a: int,b?: string}\n');
  const chk = run('fmt', f, '--check');
  check('--check flags drift, leaves the file', chk.code === 1 && readFileSync(f, 'utf8').includes('x=1'), chk.err);
  const w = run('fmt', f);
  check('fmt rewrites in place', w.code === 0 && readFileSync(f, 'utf8') === 'const x = 1 + 2\ntype T = { a: int, b?: string }\n', readFileSync(f, 'utf8'));
  const again = run('fmt', f, '--check');
  check('formatted file passes --check', again.code === 0, again.err);
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
