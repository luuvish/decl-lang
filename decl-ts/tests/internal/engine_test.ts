// engine (tests/internal/checks.json): the engine's boundary through the
// single-module pipeline — quantities, references, $referrers, a cycle.
import { initParser } from '../../src/node.ts';
import { parseSource } from '../../src/parse.ts';
import { isQ } from '../../src/semantics.ts';
import { runPipeline } from '../../src/pipeline.ts';
import { check, total } from '../common/check.ts';

await initParser();
{
  const q = runPipeline(
    parseSource('dimension Speed = Length / Time\nunit mps: Speed\noutput v: quantity<Speed> = 3km / 2s\n').decls,
  );
  const v = q.eng.resolveSegs(['v']);
  const r = runPipeline(
    parseSource(
      'type S = { name: string, inbound = $referrers(L, "target") }\ntype L = { source: ref<S>, target: ref<S> }\ntype Top = { services: S[], links: L[] }\nexport output top: Top = { services: [{ name: "a" }, { name: "b" }], links: [{ source: services[0], target: services[1] }] }\n',
    ).decls,
  );
  const ser = r.eng.serialize(r.env.roots.get('top'), 'top');
  check(
    'values',
    q.diags.length === 0 &&
      isQ(v) &&
      v.dim === 'Length*Time^-1' &&
      v.value === 1500 &&
      r.diags.length === 0 &&
      ser.includes('"source":"$.services[0]"') &&
      ser.includes('"inbound":["$.links[0]"]'),
    JSON.stringify({ v, ser, qd: q.diags, rd: r.diags }),
  );
}
{
  const p = runPipeline(parseSource('type T = { a = b, b = a }\nexport output t: T = {}\n').decls);
  check('cycle', p.diags.some((d) => d.code === 'E5007'), JSON.stringify(p.diags));
}
total();
