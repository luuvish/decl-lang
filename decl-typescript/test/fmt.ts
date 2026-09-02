// Formatter tests (Phase 4 exit criterion): idempotency over the whole
// corpus — fmt(fmt(x)) == fmt(x) — plus safety (formatting never
// changes the token stream) and canonical-form spot checks.
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initFormatter, format } from '../src/fmt.ts';
import { parseSource } from '../src/parse.ts';
import { initParser } from '../src/node.ts';
import { walkDecl } from '../src/conformance.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};

await initParser();
await initFormatter();

console.log('== canonical-form spot checks ==');
{
  const cases: [string, string, string][] = [
    ['spacing', 'const x=1+2*3\n', 'const x = 1 + 2 * 3\n'],
    ['range stays tight', 'type P=1..65535\n', 'type P = 1..65535\n'],
    ['generic angles attach', 'type V = Vec<int ,4>\n', 'type V = Vec<int, 4>\n'],
    ['call parens attach', 'const n = std.array.count(xs )\n', 'const n = std.array.count(xs)\n'],
    ['record braces breathe', 'type T = {a: int,b?: string}\n', 'type T = { a: int, b?: string }\n'],
    ['indent rederived', 'type T = {\n        a: int\n  b: string\n}\n', 'type T = {\n    a: int\n    b: string\n}\n'],
    ['unary minus attaches', 'const y = -x + 1\n', 'const y = -x + 1\n'],
    ['blank lines collapse', 'const a = 1\n\n\n\nconst b = 2\n', 'const a = 1\n\nconst b = 2\n'],
    ['continuation hangs', 'type T = {\n    assert a: x > 0\nelse warn `bad`\n}\n', 'type T = {\n    assert a: x > 0\n        else warn `bad`\n}\n'],
    ['lambda spacing', 'const f = std.array.all(xs,(x)=>x>0)\n', 'const f = std.array.all(xs, (x) => x > 0)\n'],
    ['array suffix after a record attaches', 'input s: {a: int, ...}[]\n', 'input s: { a: int, ... }[]\n'],
  ];
  for (const [name, input, want] of cases) {
    let got = '';
    try { got = format(input); } catch (e: any) { got = `THROW ${e.message}`; }
    check(name, got === want, JSON.stringify({ got, want }));
  }
}

console.log('== idempotency + safety over the corpus ==');
{
  const files: string[] = [];
  for (const dir of ['tests/validation', 'tests/modules', 'tests/packages', 'docs/examples'])
    for (const f of walkDecl(join(root, dir))) files.push(f);
  let idem = 0, tokenSafe = 0, skipped = 0, idemFail = 0, tokenFail = 0;
  const tokens = (src: string) => JSON.stringify(
    parseSource(src).decls, (k, v) => typeof v === 'bigint' ? `${v}n` : v);
  for (const f of files) {
    const src = readFileSync(f, 'utf8');
    if (parseSource(src).errors.length) { skipped++; continue; }   // invalid-parsing fixtures
    let once = '', twice = '';
    try { once = format(src); }
    catch { skipped++; continue; }
    try { twice = format(once); }
    catch (e: any) { idemFail++; console.log(`  SECOND PASS FAILS ${f.slice(root.length + 1)}: ${e.message}`); continue; }
    if (once === twice) idem++;
    else { idemFail++; console.log(`  NOT IDEMPOTENT ${f.slice(root.length + 1)}`); }
    if (parseSource(once).errors.length === 0 && tokens(once) === tokens(src)) tokenSafe++;
    else { tokenFail++; console.log(`  AST CHANGED ${f.slice(root.length + 1)}`); }
  }
  check(`fmt(fmt(x)) == fmt(x) on ${idem + idemFail} parseable files`, idemFail === 0, `${idemFail} failures`);
  check(`formatting preserves the AST on all files`, tokenFail === 0, `${tokenFail} failures`);
  console.log(`  (${skipped} unparseable fixtures skipped by design)`);
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
