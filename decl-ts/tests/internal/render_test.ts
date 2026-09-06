// render (tests/internal/checks.json): the form `@render` declares and
// the layouts of a document.
import { initParser } from '../../src/node.ts';
import { parseSource } from '../../src/parse.ts';
import { readJson } from '../../src/semantics.ts';
import { declaredForm, layout } from '../../src/render.ts';
import { check, total } from '../common/check.ts';

await initParser();
const j = (v: unknown) => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? `${x}n` : x));
const formOf = (src: string) => declaredForm(parseSource(src).decls[0]);

{
  const f: any = formOf('@render({ format: "yaml", indent: 4, file: "out/x.yaml" })\nexport output o: int = 1\n');
  const plain: any = formOf('export output o: int = 1\n');
  const bad: any = formOf('@render({ indent: 99 })\nexport output o: int = 1\n');
  const unknown: any = formOf('@render({ colour: 1 })\nexport output o: int = 1\n');
  check(
    'declared_form',
    f.format === 'yaml' &&
      f.indent === 4 &&
      f.file === 'out/x.yaml' &&
      f.template === undefined &&
      f.each === undefined &&
      plain.format === 'json' &&
      plain.indent === undefined &&
      bad.error === '@render: indent must be an integer in 0..16' &&
      unknown.error === '@render: unknown key colour',
    j([f, plain, bad, unknown]),
  );
}
{
  const raw = readJson('{"a":[1,2],"b":{}}');
  check(
    'layout',
    layout(raw, { format: 'json', indent: 2 }) === '{\n  "a": [\n    1,\n    2\n  ],\n  "b": {}\n}\n' &&
      layout(raw, { format: 'yaml' }) === 'a:\n  - 1\n  - 2\nb: {}\n' &&
      layout(raw, { format: 'json', indent: 0 }) === '{"a":[1,2],"b":{}}\n',
    j([layout(raw, { format: 'json', indent: 2 }), layout(raw, { format: 'yaml' })]),
  );
}
total();
