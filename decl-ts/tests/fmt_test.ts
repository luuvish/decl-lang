// The formatter: the canonical-form cases (tests/fmt/cases.json), then its
// two properties over every parseable module of the corpora — idempotent
// (fmt(fmt(x)) == fmt(x)) and AST-preserving (formatting moves columns,
// never nodes).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { initFormatter, format } from '../src/fmt.ts';
import { parseSource } from '../src/parse.ts';
import { initParser } from '../src/node.ts';
import { walkDecl } from '../src/conformance.ts';
import { check, total, root } from './common/check.ts';

await initParser();
await initFormatter();

type Case = { name: string; input: string; expected?: string; error?: boolean };

console.log('== fmt: the cases of tests/fmt ==');
const cases: Case[] = JSON.parse(readFileSync(join(root, 'tests/fmt/cases.json'), 'utf8'));
for (const c of cases) {
  let got: string | null = null,
    threw = '';
  try {
    got = format(c.input);
  } catch (e: any) {
    threw = e.message;
  }
  if (c.error) check(c.name, got === null, `formatted anyway: ${JSON.stringify(got)}`);
  else
    check(
      c.name,
      got === c.expected,
      JSON.stringify({ got: got ?? `THROW ${threw}`, want: c.expected }),
    );
}

console.log('== fmt: idempotent and AST-preserving over the corpora ==');
{
  const files: string[] = [];
  for (const dir of ['tests/validation', 'tests/modules', 'tests/packages', 'docs/examples'])
    for (const f of walkDecl(join(root, dir))) files.push(f);
  let idem = 0,
    skipped = 0,
    idemFail = 0,
    tokenFail = 0;
  const tokens = (src: string) =>
    JSON.stringify(parseSource(src).decls, (k, v) =>
      k === 'loc' ? undefined : typeof v === 'bigint' ? `${v}n` : v,
    ); // source ranges are not the AST
  for (const f of files) {
    const src = readFileSync(f, 'utf8');
    if (parseSource(src).errors.length) {
      skipped++;
      continue;
    } // invalid-parsing fixtures
    let once: string, twice: string;
    try {
      once = format(src);
    } catch {
      skipped++;
      continue;
    }
    try {
      twice = format(once);
    } catch (e: any) {
      idemFail++;
      console.log(`  SECOND PASS FAILS ${f.slice(root.length + 1)}: ${e.message}`);
      continue;
    }
    if (once === twice) idem++;
    else {
      idemFail++;
      console.log(`  NOT IDEMPOTENT ${f.slice(root.length + 1)}`);
    }
    if (parseSource(once).errors.length !== 0 || tokens(once) !== tokens(src)) {
      tokenFail++;
      console.log(`  AST CHANGED ${f.slice(root.length + 1)}`);
    }
  }
  check(
    `fmt(fmt(x)) == fmt(x) on ${idem + idemFail} parseable files`,
    idemFail === 0,
    `${idemFail} failures`,
  );
  check(`formatting preserves the AST on all files`, tokenFail === 0, `${tokenFail} failures`);
  console.log(`  (${skipped} unparseable fixtures skipped by design)`);
}
total();
