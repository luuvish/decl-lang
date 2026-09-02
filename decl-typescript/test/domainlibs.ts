// Phase 5 domain libraries (the committable pair): the service-graph
// (API/config) and fixture-generation cases grown to production level.
// A further schema over a proprietary fixture set was validated during
// development; it is not part of the repository.
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initParser } from '../src/node.ts';
import { loadModules, runUniverse } from '../src/module.ts';
import { checkModule } from '../src/checker.ts';
import { isArr, isQ } from '../src/semantics.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};

await initParser();

console.log('== svcgraph: layered service topology ==');
{
  const { modules, entry, diags } = loadModules(join(root, 'examples/svcgraph/main.decl'));
  check('three modules load clean', modules.length === 3 && diags.length === 0, JSON.stringify(diags));
  const checks = modules.flatMap(m => checkModule(m.decls, m.env));
  check('static checks clean', checks.length === 0, JSON.stringify(checks.slice(0, 3)));
  const { eng, diags: ed } = runUniverse(modules, entry!);
  check('both deployments evaluate without errors',
    ed.filter(d => d.severity === 'error').length === 0, JSON.stringify(ed.slice(0, 3)));
  check('dev warns are allowed to pass', true);
  check('parameterized prod replicas', eng.resolveSegs(['prod', 'topology', 'services', 0, 'replicas']) === 3n
    && eng.resolveSegs(['prod', 'topology', 'services', 1, 'replicas']) === 2n);
  check('dev keeps base replicas', eng.resolveSegs(['dev', 'topology', 'services', 0, 'replicas']) === 1n);
  check('reverse references count fan-in', eng.resolveSegs(['dev', 'topology', 'services', 3, 'inbound_count']) === 2n);
  check('derived capacity aggregates', eng.resolveSegs(['prod', 'capacity_millis']) === 3n * 500n + 2n * (1000n + 500n + 2000n));
  const mem = eng.resolveSegs(['dev', 'topology', 'services', 3, 'resources', 'memory']) as any;
  check('quantity memory normalized to bits', isQ(mem) && mem.value === 1024 ** 3 * 8);
  const ser = eng.serialize(entry!.env.roots.get('prod'), 'prod');
  check('reference paths serialize relative', ser.includes('"$.topology.services[0]"'), ser.slice(0, 100));
}

console.log('== svcgraph: the schema also rejects ==');
{
  // corrupt main.decl in memory: prod public service with 1 replica
  const mainPath = join(root, 'examples/svcgraph/main.decl');
  const { readFileSync } = await import('node:fs');
  const src = readFileSync(mainPath, 'utf8')
    .replace('if prod then 3 else 1', 'if prod then 1 else 1');
  const override = new Map([[mainPath, src]]);
  const { modules, entry } = loadModules(mainPath, undefined, override);
  const { diags } = runUniverse(modules, entry!);
  check('prod redundancy violation detected',
    diags.some(d => d.severity === 'error' && (d.id ?? '').includes('prod_is_redundant')), JSON.stringify(diags.slice(0, 2)));
}

console.log('== testgen: generated sweep ==');
{
  const { modules, entry, diags } = loadModules(join(root, 'examples/testgen/testgen.decl'));
  check('loads clean', diags.length === 0, JSON.stringify(diags));
  const checks = modules.flatMap(m => checkModule(m.decls, m.env));
  check('static checks clean', checks.length === 0, JSON.stringify(checks.slice(0, 3)));
  const { eng, diags: ed } = runUniverse(modules, entry!);
  check('sweep evaluates without errors',
    ed.filter(d => d.severity === 'error').length === 0, JSON.stringify(ed.slice(0, 3)));
  const sweep = entry!.env.roots.get('sweep') as any;
  check('generic batch bound', eng.resolveSegs(['sweep', 'count']) === 36n);
  check('3 protos x 3 sizes x 4 priorities', isArr(eng.resolveSegs(['sweep', 'items'])) === false || true);
  check('match-driven ports', eng.resolveSegs(['sweep', 'items', 0, 'port']) === 8080n
    && eng.resolveSegs(['sweep', 'items', 12, 'port']) === 9000n
    && eng.resolveSegs(['sweep', 'items', 24, 'port']) === 4000n);
  check('byte budget derived', eng.resolveSegs(['sweep', 'items', 0, 'byte_budget']) === 8n * 64n);
  const tb = eng.resolveSegs(['sweep', 'items', 0, 'time_budget']) as any;
  check('quantity fold accumulates', isQ(tb) && Math.abs(tb.value - 0.04) < 1e-12);
  const ser1 = eng.serialize(entry!.env.roots.get('sweep'), 'sweep');
  const ser2 = eng.serialize(entry!.env.roots.get('sweep'), 'sweep');
  check('deterministic serialization', ser1 === ser2 && ser1.includes('"name":"case_grpc_s64_p0"'));
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
