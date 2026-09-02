// Multi-module loading, linking, checking, and evaluation (§8, Phase 3):
// named/renamed/namespace imports, re-export, exported units/dimensions,
// cross-module evaluation, and the module-graph error conditions.
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initParser } from '../src/node.ts';
import { loadModules, runUniverse } from '../src/module.ts';
import { checkModule } from '../src/checker.ts';
import { isArr } from '../src/semantics.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};

await initParser();

console.log('== basic: named / renamed / namespace imports, re-export ==');
{
  const { modules, entry, diags } = loadModules(join(root, 'tests/modules/basic/main.decl'));
  check('three modules load, no diagnostics', modules.length === 3 && diags.length === 0, JSON.stringify(diags));
  const checks = modules.flatMap(m => checkModule(m.decls, m.env));
  check('every module checks clean', checks.length === 0, JSON.stringify(checks.slice(0, 3)));
  const { eng, diags: ed } = runUniverse(modules, entry!);
  const errs = ed.filter(d => d.severity === 'error');
  check('universe evaluates clean', errs.length === 0, JSON.stringify(errs.slice(0, 3)));
  check('imported type derives', eng.resolveSegs(['net', 'services', 0, 'endpoint']) === 'a:8080');
  check('namespace const in assert honored', eng.resolveSegs(['net', 'n']) === 2n);
  check('re-exported type binds', eng.resolveSegs(['first', 'endpoint']) === 'solo:8080');
  check('cross-module func call', entry!.env.roots.get('capped') === 16n);
  check('renamed import evaluates', entry!.env.roots.get('limit_used') === 15n);
  const vel = entry!.env.roots.get('vel') as any;
  check('exported dimension/unit travel', vel && vel.dim === 'Length*Time^-1' && vel.value === 3);
  const ser = eng.serialize(entry!.env.roots.get('net'), 'net');
  check('cross-module serialization', ser.includes('"endpoint":"b:9000"'), ser.slice(0, 120));
}

console.log('== module-graph errors ==');
{
  const cyc = loadModules(join(root, 'tests/modules/cycle/a.decl'));
  check('import cycle is E3007', cyc.diags.some(d => d.code === 'E3007'), JSON.stringify(cyc.diags));

  const ne = loadModules(join(root, 'tests/modules/errors/not_exported.decl'));
  check('unexported name is E3005', ne.diags.some(d => d.code === 'E3005'), JSON.stringify(ne.diags));

  const col = loadModules(join(root, 'tests/modules/errors/collision.decl'));
  check('import collision is E3006', col.diags.some(d => d.code === 'E3006'), JSON.stringify(col.diags));

  const rc = loadModules(join(root, 'tests/modules/errors/root_a.decl'));
  check('root-name clash is E3018', rc.diags.some(d => d.code === 'E3018'), JSON.stringify(rc.diags));

  const nf = loadModules(join(root, 'tests/modules/errors/missing_target.decl'));
  check('missing module is E3004', nf.diags.some(d => d.code === 'E3004'), JSON.stringify(nf.diags));
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
