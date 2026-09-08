// Differential over the shared valid corpus (tests/validation/**/valid): every
// fixture that the parser and static checker accept, and whose forms the query
// engine already compiles, must evaluate byte-identically to the current engine
// (same serialized outputs and `ok`). A fixture the query engine does not yet
// compile raises Unsupported and is skipped (reported, not failed); a real
// divergence or a crash fails the run. This is the corpus-scale gate the
// hand-written diff_test complements — it grows toward full replacement.
import { globSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { initParser } from '../../src/node.ts';
import { evaluateSource } from '../../src/pipeline.ts';
import { parseSource } from '../../src/parse.ts';
import { Env } from '../../src/semantics.ts';
import { qevaluate, Unsupported } from '../../src/qengine/qeval.ts';
import { check, total, root } from '../common/check.ts';

await initParser();
console.log('== qengine: differential over the valid corpus ==');

const files = globSync('tests/validation/**/valid/*.decl', { cwd: root }).sort();
let matched = 0;
let skipUnsup = 0;
let skipStage = 0;
const reasons = new Map<string, number>();
const bump = (k: string) => reasons.set(k, (reasons.get(k) ?? 0) + 1);

for (const rel of files) {
  const src = readFileSync(join(root, rel), 'utf8');
  // the query engine replaces the evaluator, not the parser or static checker;
  // a fixture those stages decide is not an evaluator comparison
  let ref;
  try {
    ref = evaluateSource(src);
  } catch {
    skipStage++;
    continue;
  }
  if (ref.phase !== 'evaluate') {
    skipStage++;
    continue;
  }
  const { decls, errors } = parseSource(src);
  if (errors.length) {
    skipStage++;
    continue;
  }
  let got;
  try {
    const env = new Env();
    env.load(decls);
    got = qevaluate(env);
  } catch (e) {
    if (e instanceof Unsupported) {
      skipUnsup++;
      bump(e.message.replace('qeval: unsupported ', ''));
      continue;
    }
    check(rel, false, `crash: ${(e as Error).message.slice(0, 80)}`);
    continue;
  }
  const same =
    JSON.stringify(ref.outputs) === JSON.stringify(got.outputs) && ref.ok === got.ok;
  check(
    rel,
    same,
    same ? '' : `ref=${JSON.stringify(ref.outputs)} got=${JSON.stringify(got.outputs)} (ok ${ref.ok}/${got.ok})`,
  );
  if (same) matched++;
}

console.log(
  `\n  matched ${matched}, skipped ${skipUnsup} (unsupported form) + ${skipStage} (parser/checker) / ${files.length} valid fixtures`,
);
if (reasons.size) {
  console.log('  not-yet-compiled forms:');
  for (const [k, v] of [...reasons.entries()].sort((a, b) => b[1] - a[1]))
    console.log(`    ${v}  ${k}`);
}
total();
