// module (tests/internal/checks.json): the module graph — loading a
// universe, and the graph's error codes.
import { join } from 'node:path';
import { initParser } from '../../src/node.ts';
import { loadModules } from '../../src/module.ts';
import { check, total, root } from '../common/check.ts';

await initParser();
{
  const r = loadModules(join(root, 'tests/modules/basic/main.decl'));
  check(
    'graph',
    r.modules.length === 3 && !!r.entry && r.entry.path.endsWith('main.decl') && r.diags.length === 0,
    JSON.stringify({ n: r.modules.length, entry: r.entry?.path, diags: r.diags }),
  );
}
{
  const code = (file: string, want: string) =>
    loadModules(join(root, 'tests/modules', file)).diags.some((d) => d.code === want);
  check(
    'errors',
    code('cycle/a.decl', 'E3007') &&
      code('errors/not_exported.decl', 'E3005') &&
      code('errors/collision.decl', 'E3006') &&
      code('errors/root_a.decl', 'E3018') &&
      code('errors/missing_target.decl', 'E3004'),
  );
}
total();
