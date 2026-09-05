// infer (tests/internal/checks.json): the inference boundary — the type
// text of literals, a quantity, an array, and range arithmetic (D31).
import { initParser } from '../../src/node.ts';
import { parseSource } from '../../src/parse.ts';
import { Env } from '../../src/semantics.ts';
import { makeCtx, infer, typeText } from '../../src/infer.ts';
import { check, total } from '../common/check.ts';

await initParser();
const env = new Env();
env.load(parseSource('type Small = 1..10\nconst a: Small = 1\nconst b: Small = 2\n').decls);
const cx = makeCtx(env, () => {});
const ty = (src: string) => typeText(infer(cx, (parseSource(`const z = ${src}\n`).decls[0] as any).expr).rt);
const want: [string, string][] = [
  ['1', '1'],
  ['1.5', '1.5'],
  ['"s"', '"s"'],
  ['true', 'true'],
  ['null', 'null'],
  ['1km', 'quantity<Length>'],
  ['[1, 2]', '(1 | 2)[]'],
  ['a + b', '2..20'],
];
check(
  'expressions',
  want.every(([src, t]) => ty(src) === t),
  want.map(([src]) => `${src} -> ${ty(src)}`).join(' | '),
);
total();
