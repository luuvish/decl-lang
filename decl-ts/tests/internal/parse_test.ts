// parse (tests/internal/checks.json): the parser's boundary — the AST a
// text produces, its source ranges, and the document reader.
import { initParser } from '../../src/node.ts';
import { parseSource } from '../../src/parse.ts';
import { readJson } from '../../src/semantics.ts';
import { check, total } from '../common/check.ts';

await initParser();
const j = (v: unknown) => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? `${x}n` : x));

{
  const r = parseSource('const x = 1 + 2\n');
  const d: any = r.decls[0];
  check(
    'const_binary',
    r.errors.length === 0 &&
      r.decls.length === 1 &&
      d.d === 'const' &&
      d.name === 'x' &&
      d.expr.e === 'bin' &&
      d.expr.op === '+' &&
      d.expr.l.e === 'lit' &&
      d.expr.l.v === 1n &&
      d.expr.r.e === 'lit' &&
      d.expr.r.v === 2n,
    j(d),
  );
}
{
  const t: any = parseSource('type T = { a: int, b?: int, c?: int = 1, d = 2, e$ = 3 }\n').decls[0];
  const kind = (m: any) =>
    m.m === 'value'
      ? m.opt
        ? m.dflt
          ? 'defaulted'
          : 'optional'
        : 'required'
      : m.hidden
        ? 'hidden'
        : 'derived';
  const ms = t.type.members;
  check(
    'member_kinds',
    ms.map(kind).join(',') === 'required,optional,defaulted,derived,hidden' &&
      ms.map((m: any) => m.name).join(',') === 'a,b,c,d,e$',
    ms.map(kind).join(','),
  );
}
{
  const r = parseSource('const a = 1\n\ntype T = {\n    x: int\n}\nexport output o: T = { x: 1 }\n');
  const lines = [0, 2, 5];
  check(
    'decl_locs',
    r.decls.length === 3 &&
      r.decls.every((d: any, i) => d.loc && d.loc.sl === lines[i] && d.loc.el >= d.loc.sl),
    j(r.decls.map((d: any) => d.loc)),
  );
}
{
  const v = readJson('{"a": [1, 2.5, "s", true, null], "n": 12345678901234567890}');
  const a = v.entries[0][1];
  let refused = false;
  try {
    readJson('{"a": 1} x');
  } catch {
    refused = true;
  }
  check(
    'json_documents',
    v.__jobj === true &&
      v.entries[0][0] === 'a' &&
      Array.isArray(a) &&
      a[0] === 1n &&
      a[1] === 2.5 &&
      a[2] === 's' &&
      a[3] === true &&
      a[4] === null &&
      v.entries[1][0] === 'n' &&
      v.entries[1][1] === 12345678901234567890n &&
      refused,
    j(v),
  );
}
total();
