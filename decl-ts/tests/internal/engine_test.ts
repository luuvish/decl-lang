// engine (tests/internal/checks.json): the engine's boundary through the
// single-module pipeline — quantities, references, $referrers, a cycle.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { qevaluateUniverse } from '../../src/qengine/qeval.ts';
import { Env, readJson, sortDiags } from '../../src/semantics.ts';
import { Engine } from '../../src/engine.ts';
import { initParser } from '../../src/node.ts';
import { parseSource } from '../../src/parse.ts';
import { isQ } from '../../src/semantics.ts';
import { runPipeline } from '../../src/pipeline.ts';
import { check, total, root } from '../common/check.ts';

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
{
  const cases: { file: string; reuse: boolean; cutoff?: boolean }[] = JSON.parse(
    readFileSync(join(root, 'tests/internal/rounds.json'), 'utf8'),
  );
  for (const row of cases) {
    const decls = parseSource(readFileSync(join(root, row.file), 'utf8')).decls;
    const ref = runPipeline(decls);
    const env = new Env();
    env.load(decls);
    const { report, eng } = qevaluateUniverse([{ env }], { env });
    const ok = !ref.diags.some((d) => d.severity === 'error');
    const outputs = ok
      ? ref.env.outputs.filter((o) => ref.env.roots.has(o.name)).map((o) => ({
          name: o.name,
          json: ref.eng.serialize(ref.env.roots.get(o.name), o.name),
        }))
      : [];
    check(
      'reference_round_reuse',
      report.ok === ok &&
        JSON.stringify(report.outputs) === JSON.stringify(outputs) &&
        JSON.stringify(report.diagnostics) === JSON.stringify(ref.diags) &&
        (!row.reuse ||
          (eng.roundCache!.reusedRounds >= 3 && eng.roundCache!.retainedRecords >= 20)) &&
        (!row.cutoff || (eng.revisions!.verifiedCutoffs >= 3 && eng.revisions!.valueCutoffs >= 3)),
      row.file,
    );
  }
}
{
  const cases: { name: string; source: string; input: string; element: string; sizes: number[] }[] = JSON.parse(readFileSync(join(root, 'tests/internal/programs.json'), 'utf8'));
  for (const row of cases) {
    const decls = parseSource(row.source).decls;
    let first: number[] | undefined;
    for (const size of row.sizes) {
      const raw = readJson('[' + Array(size).fill(row.element).join(',') + ']');
      const reference = new Env();
      reference.load(decls);
      const tree = Engine.evaluate(reference, (eng) => {
        eng.bindRoot(row.input, raw, reference.resolve(reference.inputs.get(row.input)!.type), { inst: null, locals: new Map(), rootName: row.input }, false);
        for (const o of reference.outputs) eng.bindRoot(o.name, o.expr, reference.resolve(o.type), { inst: null, locals: new Map(), rootName: o.name }, true);
      }, () => reference.roots.values());
      tree.validateAll('');
      const env = new Env(); env.load(decls);
      const { report, eng } = qevaluateUniverse([{ env }], { env }, [{ input: row.input, raw }]);
      const counts = [eng.programs!.compiled, eng.programs!.compiledSchemas];
      const expected = reference.outputs.map((o) => ({ name: o.name, json: tree.serialize(reference.roots.get(o.name), o.name) }));
      check('shared_programs', report.ok && JSON.stringify(report.outputs) === JSON.stringify(expected) && JSON.stringify(report.diagnostics) === JSON.stringify(sortDiags(reference.diagnostics)) && env.registry.length >= size && counts.every((n) => n > 0) && (!first || JSON.stringify(first) === JSON.stringify(counts)), `${row.name} size=${size} compiled=${JSON.stringify(counts)} first=${JSON.stringify(first)}`);
      first ??= counts;
    }
  }
}
total();
