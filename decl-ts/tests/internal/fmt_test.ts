// fmt (tests/internal/checks.json): what the formatter keeps — comments and
// the author's line structure (§2.9).
import { initParser } from '../../src/node.ts';
import { initFormatter, format } from '../../src/fmt.ts';
import { check, total } from '../common/check.ts';

await initParser();
await initFormatter();
const texts = ['// a comment\ntype T = {\n    a: int // trailing\n    b: string\n}\n', 'const x = [1,\n    2]\n'];
check(
  'structure',
  texts.every((t) => format(t) === t),
  JSON.stringify(texts.map((t) => format(t))),
);
total();
