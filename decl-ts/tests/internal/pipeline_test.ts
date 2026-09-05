// pipeline (tests/internal/checks.json): the source-level report — the
// phase that decided, and what it carries.
import { initParser } from '../../src/node.ts';
import { initFormatter } from '../../src/fmt.ts';
import { evaluateSource } from '../../src/pipeline.ts';
import { check, total } from '../common/check.ts';

await initParser();
await initFormatter();
const parse = evaluateSource('const x = \n');
const checked = evaluateSource('type Bad = 10..3\n');
const clean = evaluateSource('export output x: int = 1\ninput y: int\n');
check(
  'report',
  parse.phase === 'parse' &&
    !parse.ok &&
    parse.parseErrors.length > 0 &&
    checked.phase === 'check' &&
    !checked.ok &&
    checked.checks.some((d) => d.code === 'E4011') &&
    clean.phase === 'evaluate' &&
    clean.ok &&
    JSON.stringify(clean.outputs) === '[{"name":"x","json":"1"}]' &&
    JSON.stringify(clean.inputs) === '["y"]',
  JSON.stringify({ parse, checked, clean }),
);
total();
