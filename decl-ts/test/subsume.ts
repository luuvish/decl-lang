// The subsumption judgment (§3.17) and structural emptiness (§3.19) over
// the shared corpus tests/subsume/: prelude.decl declares the types,
// cases.txt lists the judgments — the same file every implementation
// runs (decl-rs/tests/e2e.rs, decl-py/scripts/e2e.py).
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initParser } from '../src/node.ts';
import { parseSource } from '../src/parse.ts';
import { Env } from '../src/semantics.ts';
import type { RT } from '../src/semantics.ts';
import { subsumes, structurallyEmpty } from '../src/subsume.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
await initParser();
const prelude = readFileSync(join(root, 'tests/subsume/prelude.decl'), 'utf8');
const { decls, errors } = parseSource(prelude);
if (errors.length) throw new Error(`prelude parse errors: ${errors.length}`);
const env = new Env();
env.load(decls);

// a side of a case is a type written in the language: parsed as a
// declaration's type and resolved in the prelude's environment
const typeOf = (text: string): RT => {
  const r = parseSource(`type __case = ${text}\n`);
  if (r.errors.length || r.decls.length !== 1 || r.decls[0].d !== 'type')
    throw new Error(`cannot parse the type: ${text}`);
  return env.resolve((r.decls[0] as any).type);
};

let pass = 0,
  fail = 0;
const check = (name: string, ok: boolean, detail: string) => {
  if (ok) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    fail++;
    console.log(`  FAIL ${name}: ${detail}`);
  }
};
console.log('== tests/subsume/cases.txt ==');
for (const line of readFileSync(join(root, 'tests/subsume/cases.txt'), 'utf8').split('\n')) {
  const t = line.trim();
  if (!t || t.startsWith('#')) continue;
  const m = /^(yes|no|empty|full)\s+(.+?)\s+::\s+(.+)$/.exec(t);
  if (!m) {
    fail++;
    console.log(`  FAIL unreadable case: ${t}`);
    continue;
  }
  const [, verdict, name, judgment] = m;
  if (verdict === 'empty' || verdict === 'full') {
    check(
      name,
      structurallyEmpty(env, typeOf(judgment)) === (verdict === 'empty'),
      `emptiness != ${verdict === 'empty'}`,
    );
    continue;
  }
  const sides = judgment.split(' ⊑ ');
  if (sides.length !== 2) {
    fail++;
    console.log(`  FAIL unreadable judgment: ${judgment}`);
    continue;
  }
  const [a, b] = sides.map(typeOf);
  check(
    name,
    subsumes(env, a, b) === (verdict === 'yes'),
    verdict === 'yes' ? 'expected ⊑' : 'expected not ⊑',
  );
}
console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
