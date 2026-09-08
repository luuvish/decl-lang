// Differential over multi-module universes (§8): a program's modules are loaded
// and its whole universe evaluated by both the current engine (runUniverse) and
// the query engine (qevaluateUniverse); the serialized outputs and `ok` must
// match. Entry programs whose modules fail to load, or that the checker rejects,
// are not evaluator comparisons and are skipped.
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { initParser } from '../../src/node.ts';
import { loadModules, runUniverse } from '../../src/module.ts';
import { qevaluateUniverse } from '../../src/qengine/qeval.ts';
import { check, total, root } from '../common/check.ts';

await initParser();
console.log('== qengine: differential over multi-module universes ==');

const entries = [
  'tests/modules/basic/main.decl',
  'examples/svcgraph/main.decl',
].filter((p) => existsSync(join(root, p)));

for (const rel of entries) {
  const entryPath = join(root, rel);
  const loaded = loadModules(entryPath);
  if (loaded.diags.length || !loaded.entry) {
    console.log(`  SKIP ${rel}: module load errors`);
    continue;
  }
  const entry = loaded.entry;

  // reference: evaluate the universe and serialize every module's outputs
  const { eng } = runUniverse(loaded.modules, entry);
  const refOut: string[] = [];
  for (const m of loaded.modules)
    for (const o of m.env.outputs)
      if (entry.env.roots.has(o.name))
        refOut.push(`${o.name}=${eng.serialize(entry.env.roots.get(o.name), o.name)}`);
  const refOk = !entry.env.diagnostics.some((d) => d.severity === 'error');

  // query engine: a fresh load (runUniverse mutated the first universe's envs)
  const fresh = loadModules(entryPath);
  if (!fresh.entry) continue;
  const got = qevaluateUniverse(fresh.modules, fresh.entry);
  const gotOut = got.outputs.map((o) => `${o.name}=${o.json}`);

  const same =
    refOut.sort().join('\n') === [...gotOut].sort().join('\n') && refOk === got.ok;
  check(
    rel,
    same,
    same ? '' : `\n    ref(${refOk}): ${refOut.sort().join(' ')}\n    got(${got.ok}): ${gotOut.sort().join(' ')}`,
  );
}

total();
