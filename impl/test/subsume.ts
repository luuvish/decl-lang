// Unit tests for the subsumption judgment (§3.17) and structural
// emptiness (§3.19), driven through the real parser + resolver so the
// tested types are exactly what the checker sees.
import { initParser, parseSource } from '../src/parse.ts';
import { Env } from '../src/semantics.ts';
import type { RT } from '../src/semantics.ts';
import { subsumes, structurallyEmpty } from '../src/subsume.ts';

const prelude = `
dimension time
dimension size
func is_even(n: int): bool = n % 2 == 0
func is_pos(n: int): bool = n > 0
func divisible_by(d: int): (int) => bool = (n) => n % d == 0

type Byte = 0..<256
type Small = 10..20
type Wide = 0..100
type Port = 1..65535
type Name = /[a-z]+/
type Word = /[a-z]+/
type Even = int(is_even)
type EvenPos = int(is_even, is_pos)
type Div4 = int(divisible_by(4))
type Ip = "tcp" | "udp"
type Node = { id: string, next?: ref<Node> }
type NodeAlias = { id: string, next?: ref<Node> }
type Base = { name: string, width: int = 8, tag?: string, ... }
type Sub = { name: "x", width: 8, tag: string, extra: bool, ... }
type OptDrop = { name: string, ... }
type NoReq = { width: int = 8, ... }
type ReqAsOpt = { name?: string, width: int = 8, tag?: string, ... }
type Tree = { v: int, kids: Tree[] = [] }
type TreeAlias = { v: int, kids: TreeAlias[] = [] }
type Ints = int[]
type Bytes = Byte[2..4]
type Bytes3 = Byte[3]
type SMap = map<string, int>
type NMap = map<Name, Byte>
type QT = quantity<time>
type QS = quantity<size>
type RN = ref<Node>
type RB = ref<Base>
type Pair<T> = { first: T, second: T }
type Vec<T, N: int> = T[N]
type PairSmall = Pair<Small>
type PairStruct = { first: Small, second: Small }
type Quad = Vec<Small, 4>
type Wide4 = Wide[4]
`;

await initParser();
const { decls, errors } = parseSource(prelude);
if (errors.length) throw new Error(`prelude parse errors: ${errors.length}`);
const env = new Env();
env.load(decls);
const T = (name: string): RT => env.resolve({ k: 'named', name, args: [] } as any);
const lit = (v: any): RT => ({ t: 'lit', v } as RT);
const union = (...arms: RT[]): RT => ({ t: 'union', arms } as RT);

let pass = 0, fail = 0;
const yes = (name: string, a: RT, b: RT) => {
  if (subsumes(env, a, b)) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name}: expected ⊑`); }
};
const no = (name: string, a: RT, b: RT) => {
  if (!subsumes(env, a, b)) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name}: expected not ⊑`); }
};
const empty = (name: string, t: RT, want: boolean) => {
  if (structurallyEmpty(env, t) === want) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name}: emptiness != ${want}`); }
};

console.log('== primitives, literals, ranges ==');
yes('lit int ⊑ int', lit(3n), { t: 'prim', name: 'int' } as RT);
no('lit float ⊄ int', lit(3.5), { t: 'prim', name: 'int' } as RT);
yes('lit in range', lit(15n), T('Small'));
no('lit out of range', lit(9n), T('Small'));
yes('range ⊑ base prim', T('Small'), { t: 'prim', name: 'int' } as RT);
yes('narrow range ⊑ wide', T('Small'), T('Wide'));
no('wide range ⊄ narrow', T('Wide'), T('Small'));
yes('exclusive hi honored', lit(255n), T('Byte'));
no('exclusive hi excluded', lit(256n), T('Byte'));

console.log('== strings and patterns ==');
yes('pattern ⊑ string', T('Name'), { t: 'prim', name: 'string' } as RT);
yes('lit matches pattern', lit('abc'), T('Name'));
no('lit fails pattern', lit('a1'), T('Name'));
yes('pattern text identity', T('Name'), T('Word'));
yes('lit ⊑ string-lit union', lit('tcp'), T('Ip'));
no('lit ⊄ union of others', lit('ip'), T('Ip'));

console.log('== unions ==');
yes('union ⊑ wider prim', T('Ip'), { t: 'prim', name: 'string' } as RT);
yes('subset union ⊑ union', union(lit('tcp')), T('Ip'));
no('union ⊄ one arm', T('Ip'), lit('tcp'));

console.log('== predicates ==');
yes('pred ⊑ base', T('Even'), { t: 'prim', name: 'int' } as RT);
yes('pred identity', T('Even'), T('Even'));
yes('more preds ⊑ fewer', T('EvenPos'), T('Even'));
no('fewer preds ⊄ more', T('Even'), T('EvenPos'));
no('base ⊄ pred', { t: 'prim', name: 'int' } as RT, T('Even'));
yes('lit satisfies pred', lit(4n), T('Even'));
no('lit violates pred', lit(3n), T('Even'));
yes('lit satisfies pred-with-args', lit(8n), T('Div4'));
no('lit violates pred-with-args', lit(6n), T('Div4'));

console.log('== arrays, maps, quantities, refs ==');
yes('elem covariance', T('Bytes'), T('Ints'));
yes('size inside bound', T('Bytes3'), T('Bytes'));
no('unsized ⊄ sized', T('Ints'), T('Bytes'));
yes('map key/value covariance', T('NMap'), T('SMap'));
no('map value widening', T('SMap'), T('NMap'));
yes('quantity same dim', T('QT'), T('QT'));
no('quantity dim mismatch', T('QT'), T('QS'));
yes('ref target covariance', T('RN'), { t: 'ref', target: { t: 'rec', members: [] } } as any);
no('ref target mismatch', T('RN'), T('RB'));

console.log('== records ==');
yes('extra + narrowed members ⊑ base', T('Sub'), T('Base'));
yes('defaulted/optional members omissible', T('OptDrop'), T('Base'));
no('missing required member', T('NoReq'), T('Base'));
no('required weakened to optional', T('ReqAsOpt'), T('Base'));
yes('same shape both ways', T('Node'), T('NodeAlias'));
yes('recursive record coinduction', T('Tree'), T('TreeAlias'));
yes('recursive record reflexive', T('Tree'), T('Tree'));

console.log('== generics (§3.15: structural after substitution) ==');
yes('Pair<Small> ⊑ its structure', T('PairSmall'), T('PairStruct'));
yes('structure ⊑ Pair<Small>', T('PairStruct'), T('PairSmall'));
yes('Vec<Small,4> ⊑ Wide[4]', T('Quad'), T('Wide4'));
no('Wide[4] ⊄ Vec<Small,4>', T('Wide4'), T('Quad'));

console.log('== structural emptiness ==');
empty('inverted range empty', { t: 'range', base: 'int', lo: 5n, hi: 3n } as any, true);
empty('normal range non-empty', T('Small'), false);
empty('disjoint kinds intersection', { t: 'isectN', arms: [{ t: 'prim', name: 'int' }, { t: 'prim', name: 'string' }] } as any, true);
empty('disjoint ranges intersection', { t: 'isectN', arms: [T('Small'), { t: 'range', base: 'int', lo: 30n, hi: 40n }] } as any, true);
empty('overlapping ranges intersection', { t: 'isectN', arms: [T('Small'), T('Wide')] } as any, false);

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
