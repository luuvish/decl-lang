// `decl` CLI end-to-end (Phase 4 exit criterion: `decl validate tests/`
// judges the full fixture corpus).
import { spawnSync } from 'node:child_process';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { writeFileSync, readFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';

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
  writeFileSync(f, 'type T = { a: int, b = a * 2 }\nexport output demo: T = { a: 21 }\n');
  const r = run('evaluate', f, '--output', 'demo');
  check(
    'evaluate prints canonical JSON',
    r.code === 0 && r.out.trim() === '{"a":21,"b":42}',
    JSON.stringify(r),
  );
  const all = run('evaluate', join(root, 'tests/modules/basic/main.decl'));
  check(
    "evaluate emits the entry module's exported outputs",
    all.code === 0 && all.out.includes('"capped":16') && all.out.includes('"net":'),
    all.out.slice(0, 120),
  );
  const dir2 = mkdtempSync(join(tmpdir(), 'decl-cli-'));
  const to = join(dir2, 'demo.json');
  const w = run('evaluate', f, '--output', `demo=${to}`);
  check(
    '--output name=file writes the document',
    w.code === 0 && w.out === '' && readFileSync(to, 'utf8') === '{"a":21,"b":42}\n',
    JSON.stringify(w),
  );
  const two = run('evaluate', f, '--output', 'demo', '--output', 'demo');
  check(
    'two documents cannot share stdout',
    two.code === 2 && two.err.includes('at most one document can go to stdout'),
    JSON.stringify(two),
  );
}

console.log('== decl validate ==');
{
  const corpus = run('validate', join(root, 'tests/validation'));
  check(
    'full corpus judged clean',
    corpus.code === 0 && / ok, 0 failed/.test(corpus.err),
    corpus.err.slice(-200),
  );

  const dir = mkdtempSync(join(tmpdir(), 'decl-cli-'));
  const mod = join(dir, 'cfg.decl');
  writeFileSync(mod, 'type Cfg = { port: 1..65535, ... }\ninput deployed: Cfg\n');
  const doc = join(dir, 'doc.json');
  writeFileSync(doc, '{"port": 70000}');
  const exp = run('validate', mod, '--input', `deployed=${doc}`, '--expect-errors', 'E4001');
  check('--expect-errors matches', exp.code === 0, exp.err);
  const noexp = run('validate', mod, '--input', `deployed=${doc}`);
  check('unexpected errors exit 1', noexp.code === 1 && noexp.err.includes('E4001'), noexp.err);
  const imports = run('validate', join(root, 'tests/modules/basic/main.decl'));
  check('validate <file> follows imports', imports.code === 0 && imports.err === '', imports.err);
  const missing = run('validate', join(dir, 'missing.decl'));
  check(
    'validate of a missing file is E3004',
    missing.code === 1 && missing.err.includes('[E3004]'),
    missing.err,
  );
  const ver = run('--version');
  check(
    '--version prints the package version',
    ver.code === 0 && /^decl \d+\.\d+\.\d+\n$/.test(ver.out),
    ver.out + ver.err,
  );
}

console.log('== decl fmt ==');
{
  const dir = mkdtempSync(join(tmpdir(), 'decl-cli-'));
  const f = join(dir, 'messy.decl');
  writeFileSync(f, 'const x=1+2\ntype T = {a: int,b?: string}\n');
  const chk = run('fmt', f, '--check');
  check(
    '--check flags drift, leaves the file',
    chk.code === 1 && readFileSync(f, 'utf8').includes('x=1'),
    chk.err,
  );
  const unreadable = run('fmt', join(dir, 'missing.decl'));
  check(
    'fmt reports an unreadable file',
    unreadable.code === 1 && unreadable.err.includes('cannot be read'),
    unreadable.err,
  );
  const w = run('fmt', f);
  check(
    'fmt rewrites in place',
    w.code === 0 &&
      readFileSync(f, 'utf8') === 'const x = 1 + 2\ntype T = { a: int, b?: string }\n',
    readFileSync(f, 'utf8'),
  );
  const again = run('fmt', f, '--check');
  check('formatted file passes --check', again.code === 0, again.err);
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
