// yaml (tests/internal/checks.json): the YAML reader's core schema and
// refusals, the writer's plain-string rule, and the round trip.
import { readYaml, toYaml, toJson, plainSafe } from '../../src/yaml.ts';
import { readJson } from '../../src/semantics.ts';
import { check, total } from '../common/check.ts';

const j = (v: unknown) => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? `${x}n` : x));
const refusal = (src: string): string => {
  try {
    readYaml(src);
    return '';
  } catch (e: any) {
    return e.message;
  }
};

{
  const v = readYaml('a: 1\nb: 2.5\nc: yes\nd: 0x1F\ne: ~\nf: "12"\ng: [x, {h: true}]\n');
  check(
    'core_schema',
    toJson(v) === '{"a":1,"b":2.5,"c":"yes","d":31,"e":null,"f":"12","g":["x",{"h":true}]}' &&
      typeof v.entries[0][1] === 'bigint' &&
      typeof v.entries[1][1] === 'number',
    j(toJson(v)),
  );
}
check(
  'refused',
  refusal('a: !!str 1\n') === 'uses a tag at line 1' &&
    refusal('1: x\n') === 'mapping key is not a string at line 1' &&
    refusal('a: 1\na: 2\n') === 'mapping repeats the key "a" at line 2' &&
    refusal('a: 1\n---\nb: 2\n') === 'stream holds more than one document at line 2',
  [refusal('a: !!str 1\n'), refusal('1: x\n'), refusal('a: 1\na: 2\n'), refusal('a: 1\n---\nb: 2\n')].join(' | '),
);
check(
  'plain_strings',
  ['my-service', 'with space', 'a_b'].every(plainSafe) &&
    ['yes', 'n', 'true', '12', '1e3', 'a: b', '-x', '', 'x #y'].every((s) => !plainSafe(s)),
  '',
);
{
  const doc = '{"name":"s","xs":[{"a":1,"b":[]},2.0],"m":{},"q":{"value":3000.0,"unit":"m"}}';
  const raw = readJson(doc);
  const y = toYaml(raw, 2);
  const want = 'name: s\nxs:\n  - a: 1\n    b: []\n  - 2.0\nm: {}\nq:\n  value: 3000.0\n  unit: m';
  check(
    'round_trip',
    y === want && toJson(readYaml(y)) === doc && toJson(readJson(toJson(raw, 2))) === doc,
    j({ y, back: toJson(readYaml(y)) }),
  );
}
total();
