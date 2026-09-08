// Differential harness for the query engine (qengine/DESIGN.md, stage 2a): run
// scalar/const programs through both the current engine and the query engine and
// assert identical serialized outputs and `ok`. The subset grows stage by stage;
// programs using a form the query engine does not compile yet are skipped.
import { initParser } from '../../src/node.ts';
import { parseSource } from '../../src/parse.ts';
import { Env } from '../../src/semantics.ts';
import { evaluateSource } from '../../src/pipeline.ts';
import { qevaluate, Unsupported } from '../../src/qengine/qeval.ts';

await initParser();

const programs: { name: string; src: string }[] = [
  { name: 'int arithmetic', src: 'export output x: int = 1 + 2 * 3 - 4' },
  { name: 'precedence and parens', src: 'export output x: int = (1 + 2) * (3 - 5)' },
  { name: 'float division', src: 'export output x: float = 7.0 / 2.0' },
  { name: 'integer division truncates', src: 'export output x: int = 7 / 2' },
  { name: 'modulo', src: 'export output x: int = 17 % 5' },
  { name: 'negative and bitnot', src: 'export output x: int = -(~3)' },
  { name: 'bitwise and shift', src: 'export output x: int = (5 & 3) | (1 << 3)' },
  { name: 'comparison to bool', src: 'export output x: bool = 3 < 5' },
  { name: 'if-then-else', src: 'export output x: int = if 3 < 5 then 10 else 20' },
  { name: 'boolean logic short-circuit', src: 'export output x: bool = true && (false || (1 == 1))' },
  { name: 'equality of ints', src: 'export output x: bool = 2 + 2 == 4' },
  { name: 'string concatenation', src: 'export output x: string = "a" + "b" + "c"' },
  { name: 'const chain memoized', src: 'const a = 2\nconst b = a * 5\nexport output x: int = b + a' },
  { name: 'nested consts', src: 'const base = 10\nconst step = base / 2\nexport output x: int = base + step * 3' },
  { name: 'division by zero errors', src: 'export output x: int = 10 / 0' },
  { name: 'mod by zero errors', src: 'export output x: int = 10 % 0' },
  // records: slots that read sibling slots — the query graph does the work
  {
    name: 'record with derived siblings',
    src: 'type Point = { x: int, y: int, sum = x + y, scaled = sum * 2 }\nexport output p: Point = { x: 3, y: 4 }',
  },
  {
    name: 'record derived reads a const',
    src: 'const k = 100\ntype R = { base: int, total = base + k }\nexport output r: R = { base: 5 }',
  },
  {
    name: 'record with mixed scalar members',
    src: 'type T = { n: int, name: string, doubled = n * 2, tag = name }\nexport output t: T = { n: 21, name: "hi" }',
  },
  {
    name: 'record derived chain of three',
    src: 'type C = { a: int, b = a + 1, c = b + 1, d = c + a }\nexport output c: C = { a: 10 }',
  },
  // nested records + member access — a derived slot reads across the boundary
  {
    name: 'nested record with member access',
    src: 'type Inner = { a: int, b: int }\ntype Outer = { inner: Inner, total = inner.a + inner.b }\nexport output o: Outer = { inner: { a: 3, b: 4 } }',
  },
  {
    name: 'two levels of nesting',
    src: 'type L2 = { v: int }\ntype L1 = { deep: L2, twice = deep.v * 2 }\ntype Top = { mid: L1, plus = mid.deep.v + mid.twice }\nexport output t: Top = { mid: { deep: { v: 5 } } }',
  },
  {
    name: 'nested record read by a sibling derived',
    src: 'type P = { x: int, y: int }\ntype Q = { p: P, sx = p.x, sum = p.x + p.y }\nexport output q: Q = { p: { x: 8, y: 9 } }',
  },
  // references and navigation (§7.3, §7.4)
  {
    name: '$this as a reference value',
    src: 'type Node = { name: string, me = $this }\nexport output n: Node = { name: "a" }',
  },
  {
    name: '$this dereferenced by member access',
    src: 'type Node = { name: string, me = $this, echo = me.name }\nexport output n: Node = { name: "a" }',
  },
  // arrays, comprehensions, ranges, indexing (§4)
  { name: 'array literal of ints', src: 'export output xs: int[] = [1, 2, 3]' },
  { name: 'array literal of strings', src: 'export output xs: string[] = ["a", "b", "c"]' },
  { name: 'array elements are expressions', src: 'const k = 5\nexport output xs: int[] = [k, k * 2, k + 1]' },
  { name: 'array spread', src: 'const a = [1, 2]\nexport output xs: int[] = [0, ...a, 3]' },
  { name: 'comprehension maps', src: 'export output xs: int[] = [x * 2 for x in [1, 2, 3]]' },
  { name: 'comprehension over a range', src: 'export output xs: int[] = [x for x in 1..5]' },
  { name: 'comprehension over an exclusive range', src: 'export output xs: int[] = [x for x in 0..<4]' },
  { name: 'comprehension with a filter', src: 'export output xs: int[] = [x for x in 1..10 if x % 2 == 0]' },
  {
    name: 'nested comprehension clauses',
    src: 'export output xs: int[] = [x * 10 + y for x in 1..3 for y in 1..2]',
  },
  {
    name: 'comprehension reads a const',
    src: 'const step = 3\nexport output xs: int[] = [x * step for x in 1..4]',
  },
  { name: 'index into an array literal', src: 'export output x: int = [10, 20, 30][1]' },
  {
    name: 'index a const array',
    src: 'const xs = [7, 8, 9]\nexport output x: int = xs[2]',
  },
  {
    name: 'index out of bounds errors',
    src: 'export output x: int = [1, 2][5]',
  },
  {
    name: 'array member of a record',
    src: 'type R = { ns: int[] }\nexport output r: R = { ns: [4, 5, 6] }',
  },
  {
    name: 'derived array member from a scalar',
    src: 'type R = { n: int, seq = [i for i in 1..n] }\nexport output r: R = { n: 4 }',
  },
  {
    name: 'derived scalar indexes a member array',
    src: 'type R = { ns: int[], first = ns[0], last = ns[2] }\nexport output r: R = { ns: [11, 22, 33] }',
  },
  // maps: object literals and comprehensions bound to map<K, V> (§3.18, §4)
  { name: 'map literal of ints', src: 'export output m: map<string, int> = { a: 1, b: 2 }' },
  {
    name: 'map values are expressions',
    src: 'const k = 3\nexport output m: map<string, int> = { x: k + 1, y: k * 2 }',
  },
  {
    name: 'map comprehension over strings',
    src: 'export output m: map<string, int> = { s: 1 for s in ["a", "b", "c"] }',
  },
  {
    name: 'map comprehension with a filter',
    src: 'export output m: map<string, int> = { s: 9 for s in ["a", "b", "c"] if s != "b" }',
  },
  {
    name: 'map member of a record',
    src: 'type R = { m: map<string, int> }\nexport output r: R = { m: { a: 5, b: 6 } }',
  },
  {
    name: 'derived reads a map member by key',
    src: 'type R = { m: map<string, int>, got = (m["a"] ?? 0) + (m["b"] ?? 0) }\nexport output r: R = { m: { a: 5, b: 6 } }',
  },
  // `in`, string templates, patterns, and `match` (§4.5, §4.7, §4.11)
  { name: 'in over a range', src: 'export output b: bool = 3 in 1..5' },
  { name: 'not in an exclusive range', src: 'export output b: bool = 5 in 0..<5' },
  { name: 'in over an array literal', src: 'export output b: bool = 2 in [1, 3, 5]' },
  {
    name: 'in over a map key',
    src: 'type R = { m: map<string, int>, has = "a" in m }\nexport output r: R = { m: { a: 1 } }',
  },
  { name: 'string matches a pattern', src: 'export output b: bool = "abc123" matches /[a-z]+[0-9]+/' },
  { name: 'string does not match a pattern', src: 'export output b: bool = "ABC" matches /[a-z]+/' },
  {
    name: 'string template interpolation',
    src: 'const n = 5\nexport output s: string = `n is ${n}`',
  },
  {
    name: 'template with several parts',
    src: 'type P = { x: int, y: int, label = `(${x}, ${y})` }\nexport output p: P = { x: 3, y: 4 }',
  },
  {
    name: 'match over a literal union',
    src: 'type Proto = "http" | "grpc" | "tcp"\ntype R = { p: Proto, port = match p { (x: "http") => 80, (x: "grpc") => 50051, (x: "tcp") => 4000 } }\nexport output r: R = { p: "grpc" }',
  },
  {
    name: 'match with a catch-all',
    src: 'type Proto = "http" | "grpc" | "tcp"\ntype R = { p: Proto, port = match p { (x: "http") => 80, (rest) => 0 } }\nexport output r: R = { p: "tcp" }',
  },
];

let pass = 0;
let fail = 0;
let skip = 0;
console.log('== qengine: differential vs the current engine (stage 3) ==');
for (const { name, src } of programs) {
  const ref = evaluateSource(src);
  // the query engine replaces the evaluator, not the parser or static checker;
  // a program the parser or checker decides is not an evaluator comparison
  if (ref.phase !== 'evaluate') {
    console.log(`  SKIP ${name}: decided at the ${ref.phase} stage`);
    skip++;
    continue;
  }
  const { decls, errors } = parseSource(src);
  if (errors.length) {
    console.log(`  SKIP ${name}: parse errors`);
    skip++;
    continue;
  }
  const env = new Env();
  env.load(decls);
  let got;
  try {
    got = qevaluate(env);
  } catch (e) {
    if (e instanceof Unsupported) {
      console.log(`  SKIP ${name}: ${e.message}`);
      skip++;
      continue;
    }
    throw e;
  }
  const refOut = JSON.stringify(ref.outputs);
  const gotOut = JSON.stringify(got.outputs);
  if (refOut === gotOut && ref.ok === got.ok) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    fail++;
    console.log(`  FAIL ${name}`);
    console.log(`    ref: ok=${ref.ok} outputs=${refOut}`);
    console.log(`    got: ok=${got.ok} outputs=${gotOut}`);
  }
}
console.log(`\nTOTAL ${pass} ok, ${fail} failed, ${skip} skipped`);
process.exitCode = fail > 0 ? 1 : 0;
