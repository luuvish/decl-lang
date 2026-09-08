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
console.log('== qengine: differential over the corpus ==');

// every single-module fixture: valid and invalid (the checker decides most
// invalid ones, but E5xxx evaluation errors exercise the engine's error paths)
const files = globSync('tests/validation/**/*.decl', { cwd: root }).sort();
let matched = 0;
let skipUnsup = 0;
let skipStage = 0;
let diagMismatch = 0;
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
  // the whole report must match — serialized outputs, `ok`, and the
  // evaluation/validation diagnostics in field and sort order (§6.7, §12.2)
  const refDiag = JSON.stringify(ref.diagnostics);
  const gotDiag = JSON.stringify(got.diagnostics);
  const same =
    JSON.stringify(ref.outputs) === JSON.stringify(got.outputs) &&
    ref.ok === got.ok &&
    refDiag === gotDiag;
  check(
    rel,
    same,
    same
      ? ''
      : `\n    ref(${ref.ok}): ${JSON.stringify(ref.outputs)} ${refDiag}\n    got(${got.ok}): ${JSON.stringify(got.outputs)} ${gotDiag}`,
  );
  if (same) matched++;
  else if (JSON.stringify(ref.outputs) === JSON.stringify(got.outputs) && ref.ok === got.ok)
    diagMismatch++; // outputs agree but diagnostics differ
}

console.log(
  `\n  matched ${matched}, skipped ${skipUnsup} (unsupported form) + ${skipStage} (parser/checker) / ${files.length} fixtures; diagnostic mismatches: ${diagMismatch}`,
);
if (reasons.size) {
  console.log('  not-yet-compiled forms:');
  for (const [k, v] of [...reasons.entries()].sort((a, b) => b[1] - a[1]))
    console.log(`    ${v}  ${k}`);
}
total();
