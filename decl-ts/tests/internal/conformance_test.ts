// conformance (tests/internal/checks.json): the fixture judge and the
// corpus walk it rests on.
import { join } from 'node:path';
import { initParser } from '../../src/node.ts';
import { walkDecl, judgeFixture } from '../../src/conformance.ts';
import { check, total, root } from '../common/check.ts';

await initParser();
const files = [...walkDecl(join(root, 'tests/modules'))];
const sorted = files.every((f, i) => i === 0 || files[i - 1] < f);
const valid = judgeFixture(join(root, 'tests/validation/types/valid/predicates.decl'), true);
const wrong = judgeFixture(join(root, 'tests/validation/types/invalid/empty_range.decl'), true);
const right = judgeFixture(join(root, 'tests/validation/types/invalid/empty_range.decl'), false);
check(
  'judge',
  files.length === 11 &&
    sorted &&
    files.some((f) => f.endsWith('tests/modules/basic/main.decl')) &&
    valid.ok &&
    !wrong.ok &&
    wrong.detail.includes('E4011') &&
    right.ok,
  JSON.stringify({ n: files.length, sorted, valid, wrong, right }),
);
total();
