// The fixture corpus (tests/validation, tests/validation/README.md) judged
// by `decl validate <dir>` (src/conformance.ts): every fixture parses,
// checks, and evaluates as its header says.
import { spawnSync } from 'node:child_process';
import { check, total, root, cli } from './common/check.ts';

console.log('== validation: the fixture corpus ==');
const r = spawnSync(process.execPath, [cli, 'validate', 'tests/validation'], {
  encoding: 'utf8',
  cwd: root,
});
check('every fixture judged as its header says', r.status === 0, r.stderr.slice(-400));
check('the summary counts no failure', /^\d+ ok, 0 failed$/m.test(r.stderr), r.stderr.slice(-200));
total();
