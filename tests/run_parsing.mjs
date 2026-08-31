// Phase 1 fixture runner: judges the parsing phase only (ROADMAP).
//   valid/*.decl                        -> must parse with zero errors
//   invalid/* with @expect-phase: parsing -> must FAIL to parse
//   invalid/* with any other phase        -> recorded, skipped (later phases)
import { execFileSync } from 'node:child_process';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const grammarDir = join(root, 'tree-sitter-decl');

function* walk(dir) {
  for (const e of readdirSync(dir)) {
    const p = join(dir, e);
    if (statSync(p).isDirectory()) yield* walk(p);
    else if (p.endsWith('.decl')) yield p;
  }
}

function parseErrors(file) {
  try {
    const out = execFileSync('npx', ['tree-sitter', 'parse', file], {
      cwd: grammarDir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
    });
    return (out.match(/ERROR|MISSING/g) ?? []).length;
  } catch (e) {
    const out = (e.stdout ?? '') + (e.stderr ?? '');
    return Math.max(1, (out.match(/ERROR|MISSING/g) ?? []).length);
  }
}

let ok = 0, bad = 0, skipped = 0;
for (const file of walk(join(root, 'tests/validation'))) {
  const rel = file.slice(root.length + 1);
  const isValid = rel.includes('/valid/');
  const meta = Object.fromEntries(
    [...readFileSync(file, 'utf8').matchAll(/\/\/ @([a-z-]+):\s*(.+)/g)]
      .map(m => [m[1], m[2].trim()]),
  );
  if (!isValid && meta['expect-phase'] !== 'parsing') {
    skipped++;
    console.log(`  skip ${rel} (@expect-phase: ${meta['expect-phase'] ?? '?'})`);
    continue;
  }
  const errs = parseErrors(file);
  const pass = isValid ? errs === 0 : errs > 0;
  if (pass) { ok++; console.log(`  ok   ${rel}`); }
  else { bad++; console.log(`  FAIL ${rel} (${errs} parse errors, expected ${isValid ? 'none' : 'some'})`); }
}
console.log(`\n${ok} ok, ${bad} failed, ${skipped} deferred to later phases`);
process.exitCode = bad > 0 ? 1 : 0;
