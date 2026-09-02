// Packages, manifests, and the lock file (§8.6–8.7): cross-package
// imports under exact pins, fail-closed manifests, and bit-for-bit
// lock reproducibility.
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync, writeFileSync, appendFileSync, unlinkSync, existsSync } from 'node:fs';
import { initParser } from '../src/parse.ts';
import { loadModules, runUniverse } from '../src/module.ts';
import { checkModule } from '../src/checker.ts';
import { openPackageUniverse, writeLock, verifyLock, lockText } from '../src/package.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};

await initParser();

console.log('== cross-package imports under exact pins ==');
{
  const entry = join(root, 'tests/packages/app/main.decl');
  const u = openPackageUniverse(entry)!;
  check('universe opens clean', u.diags.length === 0, JSON.stringify(u.diags));
  check('closed set resolved', u.packages.size === 1 && u.packages.get('corelib')!.version === '1.0.0');
  const { modules, entry: em, diags } = loadModules(entry, u.resolver);
  check('modules load across packages', modules.length === 2 && diags.length === 0, JSON.stringify(diags));
  const checks = modules.flatMap(m => checkModule(m.decls, m.env));
  check('modules check clean', checks.length === 0, JSON.stringify(checks.slice(0, 3)));
  const { eng, diags: ed } = runUniverse(modules, em!);
  check('evaluates clean', ed.filter(d => d.severity === 'error').length === 0, JSON.stringify(ed.slice(0, 2)));
  check('imported const and defaults', em!.env.roots.get('w') === 16n && eng.resolveSegs(['box', 'width']) === 8n);
}

console.log('== lock file: reproducibility, fail-closed drift ==');
{
  const entry = join(root, 'tests/packages/app/main.decl');
  const lockPath = join(root, 'tests/packages/app/decl.lock');
  const modPath = join(root, 'tests/packages/app/decl_modules/corelib/types/base.decl');
  try {
    const u1 = openPackageUniverse(entry)!;
    writeLock(u1);
    check('fresh lock verifies clean', verifyLock(u1).length === 0);
    const u2 = openPackageUniverse(entry)!;
    check('lock text is reproducible', lockText(u1) === lockText(u2) && lockText(u1).includes('corelib 1.0.0 '));

    const original = readFileSync(modPath, 'utf8');
    appendFileSync(modPath, '// drift\n');
    try {
      const u3 = openPackageUniverse(entry)!;
      check('content drift is E3017', verifyLock(u3).some(d => d.code === 'E3017'), JSON.stringify(verifyLock(u3)));
    } finally { writeFileSync(modPath, original); }

    writeFileSync(lockPath, lockText(u1).replace('1.0.0', '1.0.1'));
    check('version drift is E3016', verifyLock(openPackageUniverse(entry)!).some(d => d.code === 'E3016'));
    writeFileSync(lockPath, '');
    check('missing entry is E3015', verifyLock(openPackageUniverse(entry)!).some(d => d.code === 'E3015'));
  } finally { if (existsSync(lockPath)) unlinkSync(lockPath); }
}

console.log('== manifest and resolution errors ==');
{
  const bad = openPackageUniverse(join(root, 'tests/packages/bad_manifest/main.decl'))!;
  check('unknown field is E3011', bad.diags.some(d => d.code === 'E3011'), JSON.stringify(bad.diags));
  check('range pin is E3012', bad.diags.some(d => d.code === 'E3012'));

  const und = openPackageUniverse(join(root, 'tests/packages/undeclared/main.decl'))!;
  const { diags } = loadModules(join(root, 'tests/packages/undeclared/main.decl'), und.resolver);
  check('undeclared dependency is E3010', diags.some(d => d.code === 'E3010'), JSON.stringify(diags));

  const con = openPackageUniverse(join(root, 'tests/packages/conflict/main.decl'))!;
  check('conflicting versions is E3014', con.diags.some(d => d.code === 'E3014'), JSON.stringify(con.diags));
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
