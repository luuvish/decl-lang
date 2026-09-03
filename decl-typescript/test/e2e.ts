// End-to-end: the three benchmark cases through the reference pipeline —
// tree-sitter parse -> AST -> bind -> evaluate -> validate -> serialize ->
// round-trip. The scenarios (and expected outcomes) are the Phase 0
// spike's, now driven by the canonical parser.
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseSource } from '../src/parse.ts';
import { initParser } from '../src/node.ts';
import { Env, isArr, readJson } from '../src/semantics.ts';
import { Engine } from '../src/engine.ts';
import { checkModule } from '../src/checker.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};

function pipeline(file: string, binds: { input: string; json: string; as?: string }[] = []) {
  const src = readFileSync(file.startsWith('/') ? file : join(root, file), 'utf8');
  const { decls, errors } = parseSource(src);
  if (errors.length) throw new Error(`${file}: ${errors.length} parse errors`);
  const env = new Env();
  env.load(decls);
  const eng = new Engine(env);
  for (const o of env.outputs) {
    const sc = { inst: null, locals: new Map<string, any>(), rootName: o.name };
    try { env.roots.set(o.name, eng.bind(eng.ev(o.expr, sc), env.resolve(o.type), [o.name], null, sc)); }
    catch { }
  }
  for (const b of binds) {
    const decl = env.inputs.get(b.input)!;
    const name = b.as ?? b.input;
    const sc = { inst: null, locals: new Map<string, any>(), rootName: name };
    try { env.roots.set(name, eng.bind(readJson(b.json), env.resolve(decl.type), [name], null, sc)); }
    catch { }
  }
  for (const v of env.roots.values()) eng.forceAll(v, false);
  eng.phase = 2;
  for (let i = 0; i < eng.deferredSlots.length; i++) eng.forceSlotSafe(eng.deferredSlots[i].inst, eng.deferredSlots[i].name);
  for (const v of env.roots.values()) eng.forceAll(v, true);
  eng.validateAll('');
  env.diagnostics.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : (a.id ?? '') < (b.id ?? '') ? -1 : 1);
  return { env, eng };
}
const get = (eng: Engine, segs: (string | number)[]) => eng.resolveSegs(segs);

await initParser();

console.log('== case 3: fixtures ==');
{
  const { env, eng } = pipeline('docs/examples/03_fixtures.decl');
  for (const d of env.diagnostics) console.log('  diag:', d.severity, d.id ?? d.code, d.path, '-', d.message);
  check('no diagnostics', env.diagnostics.length === 0);
  check('32 cases', isArr(env.roots.get('sweep')) && env.roots.get('sweep').items.length === 32);
  check('16 packets each', get(eng, ['sweep', 0, 'packet_count']) === 16n);
  check('total_bytes', get(eng, ['sweep', 0, 'total_bytes']) === 1024n);
  check('derived label', get(eng, ['sweep', 0, 'packets', 3, 'label']) === 'pkt-3-p0');
  const ser = eng.serialize(env.roots.get('sweep'), 'sweep');
  const ser2 = eng.serialize(env.roots.get('sweep'), 'sweep');
  check('repeated serialization byte-identical', ser === ser2);
}

console.log('== case 2: config ==');
{
  const { env, eng } = pipeline('docs/examples/02_config.decl');
  check('outputs clean', env.diagnostics.length === 0, JSON.stringify(env.diagnostics.slice(0, 2)));
  check('prod layering', get(eng, ['prod', 'host']) === 'api.internal' && get(eng, ['prod', 'workers']) === 32n);
  check('derived recomputed after with', get(eng, ['prod', 'insecure']) === false);

  const invalid = `{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}`;
  const r = pipeline('docs/examples/02_config.decl', [{ input: 'deployed', json: invalid }]);
  const errs = r.env.diagnostics.filter(d => d.severity === 'error');
  const warns = r.env.diagnostics.filter(d => d.severity === 'warn');
  check('two errors, one warning', errs.length === 2 && warns.length === 1, JSON.stringify(r.env.diagnostics));
  check('cert assert id', errs.some(d => d.id === 'TlsConfig.cert_present'));
  check('warned value preserved', get(r.eng, ['deployed', 'workers']) === 100n);
}

console.log('== case 1: interconnect ==');
{
  const { env, eng } = pipeline('docs/examples/01_interconnect.decl');
  for (const d of env.diagnostics) console.log('  diag:', d.severity, d.id ?? d.code, d.path, '-', d.message);
  check('xbar clean', env.diagnostics.length === 0);
  check('propagated master si', get(eng, ['xbar', 'nodes', 'dom0', 'nodes', 'mst0', 'ports', 'si', 'width']) === 64n);
  check('arbiter max', get(eng, ['xbar', 'nodes', 'dom0', 'nodes', 'arb0', 'ports', 'mi', 'width']) === 64n);

  const ser = eng.serialize(env.roots.get('xbar'), 'xbar');
  check('relative ref paths', ser.includes('"$.nodes[\\"dom0\\"].ports[\\"si0\\"]"'));

  // round-trip under a different root name
  const src = readFileSync(join(root, 'docs/examples/01_interconnect.decl'), 'utf8');
  const { decls } = parseSource(src);
  const env2 = new Env(); env2.load(decls);
  const eng2 = new Engine(env2);
  const sc2 = { inst: null, locals: new Map<string, any>(), rootName: 'doc' };
  env2.roots.set('doc', eng2.bind(readJson(ser), env2.resolve(env2.inputs.get('doc')!.type), ['doc'], null, sc2));
  eng2.forceAll(env2.roots.get('doc'), false);
  eng2.phase = 2;
  for (let i = 0; i < eng2.deferredSlots.length; i++) eng2.forceSlotSafe(eng2.deferredSlots[i].inst, eng2.deferredSlots[i].name);
  eng2.forceAll(env2.roots.get('doc'), true);
  eng2.validateAll('');
  for (const d of env2.diagnostics) console.log('  rt diag:', d.severity, d.id ?? d.code, d.path, '-', d.message);
  check('round-trip validates', env2.diagnostics.length === 0);
  check('round-trip byte-identical', eng2.serialize(env2.roots.get('doc'), 'doc') === ser);

  // corrupted boundary width -> exactly one root-cause error
  const doc = readJson(ser);
  const ports = doc.entries.find(([k]: any) => k === 'ports')[1];
  const mi1 = ports.entries.find(([k]: any) => k === 'mi1')[1];
  mi1.entries.find(([k]: any) => k === 'width')[1] = 32n;
  const env3 = new Env(); env3.load(parseSource(src).decls);
  const eng3 = new Engine(env3);
  const sc3 = { inst: null, locals: new Map<string, any>(), rootName: 'doc' };
  env3.roots.set('doc', eng3.bind(doc, env3.resolve(env3.inputs.get('doc')!.type), ['doc'], null, sc3));
  eng3.forceAll(env3.roots.get('doc'), false);
  eng3.phase = 2;
  for (let i = 0; i < eng3.deferredSlots.length; i++) eng3.forceSlotSafe(eng3.deferredSlots[i].inst, eng3.deferredSlots[i].name);
  eng3.forceAll(env3.roots.get('doc'), true);
  eng3.validateAll('');
  const errs = env3.diagnostics.filter(d => d.severity === 'error');
  check('one width_match at root cause', errs.length === 1 && errs[0].id === 'Edge.width_match', JSON.stringify(errs));
}

console.log('== guide: end to end ==');
{
  // assemble the guide's decl blocks into one module at runtime
  const md = readFileSync(join(root, 'docs/guide/01_overview_by_example.md'), 'utf8');
  const blocks = [...md.matchAll(/```decl\n([\s\S]*?)```/g)].map(m => m[1]);
  const guideSrc = blocks.join('\n');
  const { decls, errors } = parseSource(guideSrc);
  if (errors.length) throw new Error('guide module has parse errors');
  const env = new Env(); env.load(decls);
  const eng = new Engine(env);
  for (const o of env.outputs) {
    const sc = { inst: null, locals: new Map<string, any>(), rootName: o.name };
    try { env.roots.set(o.name, eng.bind(eng.ev(o.expr, sc), env.resolve(o.type), [o.name], null, sc)); } catch { }
  }
  for (const v of env.roots.values()) eng.forceAll(v, false);
  eng.phase = 2;
  for (let i = 0; i < eng.deferredSlots.length; i++) eng.forceSlotSafe(eng.deferredSlots[i].inst, eng.deferredSlots[i].name);
  for (const v of env.roots.values()) eng.forceAll(v, true);
  eng.validateAll('');
  check('guide evaluates clean', env.diagnostics.length === 0, JSON.stringify(env.diagnostics.slice(0, 3)));
  check('guide endpoint derived', get(eng, ['demo', 'services', 0, 'endpoint']) === 'svc-0:9000');
  check('guide defaults filled', get(eng, ['demo', 'services', 0, 'replicas']) === 1n);
  check('guide quantity default', (get(eng, ['demo', 'services', 0, 'timeout']) as any).value === 0.25);
  check('guide service_count', get(eng, ['demo', 'service_count']) === 3n);
  const inbound = get(eng, ['demo', 'services', 1, 'inbound']);
  check('guide inbound via referrers', isArr(inbound) && inbound.items.length === 2);
  const ser = eng.serialize(env.roots.get('demo'), 'demo');
  check('guide serialized refs relative', ser.includes('"$.links[0]"'), ser.slice(0, 120));
}

console.log('== static checker: corpus stays clean ==');
{
  for (const f of ['docs/examples/01_interconnect.decl', 'docs/examples/02_config.decl', 'docs/examples/03_fixtures.decl']) {
    const { decls, errors } = parseSource(readFileSync(join(root, f), 'utf8'));
    const checks = errors.length ? [] : checkModule(decls);
    check(`${f.split('/').pop()} check-clean`, errors.length === 0 && checks.length === 0, JSON.stringify(checks.slice(0, 3)));
  }
  const md = readFileSync(join(root, 'docs/guide/01_overview_by_example.md'), 'utf8');
  const guideSrc = [...md.matchAll(/```decl\n([\s\S]*?)```/g)].map(m => m[1]).join('\n');
  const checks = checkModule(parseSource(guideSrc).decls);
  check('guide check-clean', checks.length === 0, JSON.stringify(checks.slice(0, 3)));
}

console.log('== match evaluation ==');
{
  const src = `
type Circle = { kind: "circle", r: float }
type Rect = { kind: "rect", w: float, h: float }
type Shape = Circle | Rect
type Proto = "http" | "grpc" | "tcp"
input shape: Shape
input proto: Proto
output area: float = match shape {
    (c: Circle) => 3.0 * c.r * c.r
    (r: Rect) => r.w * r.h
}
output port: int = match proto {
    (p: "http") => 80
    (p: "grpc") => 50051
    (other) => 0
}
`;
  const { decls, errors } = parseSource(src);
  check('match module parses', errors.length === 0);
  check('match module checks clean', checkModule(decls).length === 0, JSON.stringify(checkModule(decls).slice(0, 3)));
  const env = new Env(); env.load(decls);
  const eng = new Engine(env);
  const scS = { inst: null, locals: new Map<string, any>(), rootName: 'shape' };
  env.roots.set('shape', eng.bind(readJson('{"kind":"rect","w":2.0,"h":3.0}'), env.resolve(env.inputs.get('shape')!.type), ['shape'], null, scS));
  const scP = { inst: null, locals: new Map<string, any>(), rootName: 'proto' };
  env.roots.set('proto', eng.bind(readJson('"grpc"'), env.resolve(env.inputs.get('proto')!.type), ['proto'], null, scP));
  for (const o of env.outputs) {
    const sc = { inst: null, locals: new Map<string, any>(), rootName: o.name };
    env.roots.set(o.name, eng.bind(eng.ev(o.expr, sc), env.resolve(o.type), [o.name], null, sc));
  }
  check('record-variant arm selected', env.roots.get('area') === 6);
  check('literal-variant arm selected', env.roots.get('port') === 50051n);
  const src2 = src.replace('"grpc"', '"grpc"');
  const env2 = new Env(); env2.load(parseSource(src2).decls);
  const eng2 = new Engine(env2);
  const sc2 = { inst: null, locals: new Map<string, any>(), rootName: 'proto' };
  env2.roots.set('proto', eng2.bind(readJson('"tcp"'), env2.resolve(env2.inputs.get('proto')!.type), ['proto'], null, sc2));
  const o2 = env2.outputs.find(o => o.name === 'port')!;
  const scO = { inst: null, locals: new Map<string, any>(), rootName: 'port' };
  check('catch-all arm selected', eng2.ev(o2.expr, scO) === 0n);
}

console.log('== generic instantiation (§3.15) ==');
{
  const src = `
type Vec<T, N: int> = T[N]
type Bounded<T, N: 1..1024> = { items: T[0..N], const count = std.array.count(items) }
output q: Vec<int, 3> = [1, 2, 3]
output b: Bounded<string, 4> = { items: ["a", "b"] }
`;
  const { decls, errors } = parseSource(src);
  check('generic module parses', errors.length === 0);
  check('generic module checks clean', checkModule(decls).length === 0, JSON.stringify(checkModule(decls).slice(0, 3)));
  const env = new Env(); env.load(decls);
  const eng = new Engine(env);
  for (const o of env.outputs) {
    const sc = { inst: null, locals: new Map<string, any>(), rootName: o.name };
    try { env.roots.set(o.name, eng.bind(eng.ev(o.expr, sc), env.resolve(o.type), [o.name], null, sc)); } catch { }
  }
  for (const v of env.roots.values()) eng.forceAll(v, true);
  eng.validateAll('');
  check('generic outputs bind clean', env.diagnostics.length === 0, JSON.stringify(env.diagnostics.slice(0, 2)));
  check('Vec<int,3> bound', isArr(env.roots.get('q')) && env.roots.get('q').items.length === 3);
  check('Bounded derived count', get(eng, ['b', 'count']) === 2n);

  // size violation caught at binding
  const env2 = new Env(); env2.load(parseSource(src).decls);
  const eng2 = new Engine(env2);
  const sc2 = { inst: null, locals: new Map<string, any>(), rootName: 'q' };
  const mark = env2.diagnostics.length;
  try { eng2.bind(eng2.ev({ e: 'arr', items: [1n, 2n].map(v => ({ spread: false, expr: { e: 'lit', v } })) } as any, sc2), env2.resolve({ k: 'named', name: 'Vec', args: [{ k: 'prim', name: 'int' }, { k: 'lit', v: 3n }] } as any), ['q'], null, sc2); } catch { }
  check('size violation surfaces', env2.diagnostics.length > mark, JSON.stringify(env2.diagnostics));
}

console.log('== quantity dimension algebra (§3.16) ==');
{
  const src = `
dimension Speed = Length / Time
unit mps: Speed
type Trip = {
    dist: quantity<Length>
    dur: quantity<Time>
    const speed: quantity<Speed> = dist / dur
    const ratio = dist / 1km
}
output trip: Trip = { dist: 3km, dur: 500ms }
`;
  const { decls, errors } = parseSource(src);
  check('quantity module parses', errors.length === 0);
  check('quantity module checks clean', checkModule(decls).length === 0, JSON.stringify(checkModule(decls).slice(0, 3)));
  const env = new Env(); env.load(decls);
  const eng = new Engine(env);
  env.finalizeUnitSpace();
  const o = env.outputs[0];
  const sc = { inst: null, locals: new Map<string, any>(), rootName: o.name };
  env.roots.set(o.name, eng.bind(eng.ev(o.expr, sc), env.resolve(o.type), [o.name], null, sc));
  eng.forceAll(env.roots.get('trip'), true);
  eng.validateAll('');
  check('quantity module binds clean', env.diagnostics.length === 0, JSON.stringify(env.diagnostics.slice(0, 2)));
  const speed = get(eng, ['trip', 'speed']) as any;
  check('dimensions compose', speed.dim === 'Length*Time^-1' && speed.value === 6000);
  check('cancelled vector is a number', get(eng, ['trip', 'ratio']) === 3);
  const ser = eng.serialize(env.roots.get('trip'), 'trip');
  check('derived-unit input normalized to base', ser.includes('"dist":{"value":3000.0,"unit":"m"}'), ser);
  check('composed dimension serializes with its base unit', ser.includes('"unit":"mps"'), ser);
  // interchange form through a derived unit
  const env2 = new Env(); env2.load(parseSource(src).decls);
  const eng2 = new Engine(env2);
  const sc2 = { inst: null, locals: new Map<string, any>(), rootName: 't' };
  const doc = readJson('{"dist":{"value":2,"unit":"km"},"dur":{"value":1,"unit":"s"}}');
  const inst = eng2.bind(doc, env2.resolve({ k: 'named', name: 'Trip', args: [] } as any), ['t'], null, sc2);
  eng2.forceAll(inst, true);
  check('interchange km converts to base', (eng2.resolveSegs as any) && (inst.slots.get('dist')!.value as any).value === 2000);
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
