// checker (tests/internal/checks.json): the checker's boundary — codes
// anchored to their declaration, and a clean module reporting nothing.
import { initParser } from '../../src/node.ts';
import { parseSource } from '../../src/parse.ts';
import { checkModule } from '../../src/checker.ts';
import { check, total } from '../common/check.ts';

await initParser();
const codes = (src: string) => checkModule(parseSource(src).decls);
const bad = codes('type Bad = 10..3\n');
const unknown = codes('const x = y\n');
const clean = codes('type T = { a: int }\nexport output t: T = { a: 1 }\n');
check(
  'anchored',
  bad.length === 1 &&
    bad[0].code === 'E4011' &&
    bad[0].loc === undefined &&
    unknown.some(
      (d) => d.code === 'E3003' && d.loc?.sl === 0 && d.loc?.sc === 10 && d.loc?.ec === 11,
    ) &&
    clean.length === 0,
  JSON.stringify({ bad, unknown, clean }),
);
total();
