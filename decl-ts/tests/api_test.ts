// The API corpus (tests/api/) through the high-level API: every case's
// answer (scripts/api-corpus.ts, the driver the parity harness runs)
// against tests/api/expected.json, the reviewed answers — documents
// compared by value (tests/api/README.md).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { runAll, root, type Answer } from '../scripts/api-corpus.ts';
import { check, total } from './common/check.ts';

const { cases, answers } = await runAll();
// the expected answers are canonical JSON; a document parsed from either
// side compares by value (6.0 and 6 are the same number here)
const expected: Answer[] = JSON.parse(readFileSync(join(root, 'tests/api/expected.json'), 'utf8'));
console.log('== api: the corpus against tests/api/expected.json ==');
check('every case answered', answers.length === expected.length, `${answers.length} of ${expected.length}`);
for (let i = 0; i < Math.min(answers.length, expected.length); i++) {
  const got = JSON.parse(JSON.stringify(answers[i]));
  const same = isDeepStrictEqual(got, expected[i]);
  check(
    cases[i].name,
    same,
    same ? '' : `\n       expected ${JSON.stringify(expected[i])}\n       got      ${JSON.stringify(got)}`,
  );
}
total();
