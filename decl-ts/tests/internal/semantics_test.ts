// semantics (tests/internal/checks.json): type resolution, the number and
// string writers, canonical paths, and the order of diagnostics.
import { initParser } from '../../src/node.ts';
import { parseSource } from '../../src/parse.ts';
import { Env, parsePath, pathStr, cmpPath, sortDiags } from '../../src/semantics.ts';
import { typeText } from '../../src/infer.ts';
import { check, total } from '../common/check.ts';

await initParser();
{
  const env = new Env();
  env.load(
    parseSource('type A = int\ntype Vec<T, N: int> = T[N]\ntype V3 = Vec<int, 3>\ntype Small = 1..10\n')
      .decls,
  );
  const t = (name: string) => typeText(env.resolve({ k: 'named', name, args: [] } as any));
  check(
    'resolve_types',
    t('A') === 'int' && t('V3') === 'int[3..3]' && t('Small') === '1..10',
    [t('A'), t('V3'), t('Small')].join(' | '),
  );
}
{
  // the reference relies on JavaScript's own number text
  const want: [number, string][] = [
    [1, '1'],
    [100, '100'],
    [2.5, '2.5'],
    [0.1 + 0.2, '0.30000000000000004'],
    [1e21, '1e+21'],
    [1e-7, '1e-7'],
    [123456789.125, '123456789.125'],
  ];
  check(
    'number_text',
    want.every(([x, s]) => String(x) === s),
    want.map(([x]) => String(x)).join(' '),
  );
}
{
  // and on JSON.stringify for strings
  const got = JSON.stringify('a"b\\c\n\t\x01é');
  check('json_string', got === '"a\\"b\\\\c\\n\\t\\u0001é"', got);
}
{
  const segs = parsePath('$.a.b[0]["k"]', 'r');
  const p = (s: string) => parsePath(s, 'r');
  check(
    'paths',
    pathStr(segs) === 'r.a.b[0]["k"]' &&
      pathStr(segs, 'r') === '$.a.b[0]["k"]' &&
      cmpPath(p('$.a.b'), p('$.a.c')) < 0 &&
      cmpPath(p('$.a[1]'), p('$.a[2]')) < 0 &&
      cmpPath(p('$.a'), p('$.a.b')) < 0,
    `${pathStr(segs)} | ${pathStr(segs, 'r')}`,
  );
}
{
  const d = (path: string, id?: string) => ({ severity: 'error', message: 'm', path, ...(id ? { id } : {}) });
  const sorted = sortDiags([d('x.b'), d(''), d('x.a', 'T.z'), d('x.a', 'T.a'), d('x[2]'), d('x[10]')]);
  const keys = sorted.map((x) => `${x.path}/${x.id ?? ''}`).join(' ');
  check('diag_order', keys === '/ x[2]/ x[10]/ x.a/T.a x.a/T.z x.b/', keys);
}
total();
