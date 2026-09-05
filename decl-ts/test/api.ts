// The high-level API (src/api.ts): evaluate / check / validate /
// formatSource, in the command line's vocabulary — inputs bound by
// name, outputs returned by name.
import { writeFileSync, mkdtempSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { evaluate, check, validate, formatSource, DeclError } from '../src/index.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
let pass = 0,
  fail = 0;
const ok = (name: string, cond: boolean, detail = '') => {
  if (cond) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    fail++;
    console.log(`  FAIL ${name} ${detail}`);
  }
};
const caught = async (f: () => Promise<unknown>): Promise<DeclError | null> => {
  try {
    await f();
    return null;
  } catch (e) {
    return e instanceof DeclError
      ? e
      : (() => {
          throw e;
        })();
  }
};

console.log('== evaluate ==');
{
  const cfg = join(root, 'docs/examples/02_config.decl');
  const all = await evaluate(cfg);
  ok(
    'default: the exported outputs by name',
    Object.keys(all).join(',') === 'base,prod,dev',
    Object.keys(all).join(','),
  );
  const one = await evaluate(cfg, { outputs: ['prod'] });
  ok(
    'outputs selects roots',
    Object.keys(one).join(',') === 'prod' && (one.prod as any).host === 'api.internal',
    JSON.stringify(one).slice(0, 80),
  );
  const fb = join(root, 'tests/validation/declarations/valid/output_from_input_fallback.decl');
  const none = await evaluate(fb);
  ok('a module exporting nothing yields {}', Object.keys(none).length === 0);
  const dir = mkdtempSync(join(tmpdir(), 'decl-api-'));
  const doc = join(dir, 'base.json');
  writeFileSync(doc, '{"host": "h", "port": 8}');
  const byFile = await evaluate(fb, { inputs: { base: doc }, outputs: ['base', 'copy'] });
  ok(
    'an input bound from a file is a root',
    JSON.stringify(byFile.base) === '{"host":"h","port":8}' &&
      JSON.stringify(byFile.copy) === '{"host":"h","port":8}',
    JSON.stringify(byFile),
  );
  const byValue = await evaluate(fb, { inputs: { base: { host: 'v' } }, outputs: ['copy'] });
  ok(
    'an input bound from a value completes',
    JSON.stringify(byValue.copy) === '{"host":"v","port":80}',
    JSON.stringify(byValue),
  );
  const e1 = await caught(() => evaluate(fb, { outputs: ['nope'] }));
  ok('an unknown root is a DeclError', e1?.message === 'no root named nope');
  const e2 = await caught(() => evaluate(fb, { inputs: { nope: {} } }));
  ok('an unknown input is a DeclError', e2?.message === 'no input named nope');
  const e3 = await caught(() => evaluate(fb, { inputs: { base: join(dir, 'missing.json') } }));
  ok(
    'an unreadable document is E6004',
    e3?.diagnostics[0]?.code === 'E6004',
    JSON.stringify(e3?.diagnostics),
  );
  const e4 = await caught(() =>
    evaluate(cfg, {
      inputs: { deployed: { host: 'x', port: 70000, workers: 100, tls: { enabled: true } } },
      outputs: ['deployed'],
    }),
  );
  ok(
    'error diagnostics come back on the DeclError',
    !!e4 &&
      e4.diagnostics.some((d) => d.severity === 'error') &&
      e4.message === e4.diagnostics[0].message,
    JSON.stringify(e4?.diagnostics.slice(0, 2)),
  );
}

console.log('== check / validate / formatSource ==');
{
  const clean = await check(join(root, 'tests/validation/types/valid/predicates.decl'));
  ok('check: clean file -> []', clean.length === 0, JSON.stringify(clean));
  const bad = await check(join(root, 'tests/validation/types/invalid/empty_range.decl'));
  ok(
    'check: static error reported with file, code, path',
    bad.length > 0 && bad[0].code === 'E4011' && Object.keys(bad[0])[0] === 'file',
    JSON.stringify(bad),
  );
  const cfg = join(root, 'docs/examples/02_config.decl');
  const v = await validate(cfg, {
    inputs: { deployed: { host: 'x', port: 70000, workers: 100, tls: { enabled: true } } },
  });
  ok(
    'validate: diagnostics of a bound document',
    v.some((d) => d.severity === 'error'),
    JSON.stringify(v.slice(0, 2)),
  );
  const e = await caught(() =>
    validate(join(root, 'tests/validation/lexical/invalid/semicolon.decl')),
  );
  ok('validate: a file that does not parse is a DeclError', !!e && /parse error/.test(e.message));
  ok('formatSource', (await formatSource('const x=1+2\n')) === 'const x = 1 + 2\n');
  const f = await caught(() => formatSource('type T = {'));
  ok('formatSource: parse error is a DeclError', !!f);
}
console.log(`TOTAL ${pass} ok, ${fail} failed`);
if (fail) process.exit(1);
