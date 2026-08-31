// Spike driver: run the benchmark cases end to end. Throwaway (ROADMAP §0.6).
import { readFileSync } from 'node:fs';
import { parseModule } from './syntax.ts';
import { ABSENT, Env, isArr, isRec, pathStr, readJson } from './semantics.ts';
import { Engine } from './engine.ts';

let pass = 0, failCnt = 0;
function check(name: string, cond: boolean, detail = '') {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { failCnt++; console.log(`  FAIL ${name} ${detail}`); }
}

function runModule(file: string, binds: { input: string; json: string; as?: string }[] = []) {
  const src = readFileSync(file, 'utf8');
  const env = new Env();
  env.load(parseModule(src));
  const eng = new Engine(env);
  for (const o of env.outputs) {
    const sc = { inst: null, locals: new Map<string, any>(), rootName: o.name };
    const rt = env.resolve(o.type);
    const pre = eng.ev(o.expr, sc);
    try { env.roots.set(o.name, eng.bind(pre, rt, [o.name], null, sc)); }
    catch { /* root invalid: diagnostics already reported */ }
  }
  for (const b of binds) {
    const decl = env.inputs.get(b.input);
    const name = b.as ?? b.input;
    const sc = { inst: null, locals: new Map<string, any>(), rootName: name };
    const rt = env.resolve(decl!.type);
    try { env.roots.set(name, eng.bind(readJson(b.json), rt, [name], null, sc)); }
    catch { }
  }
  for (const v of env.roots.values()) eng.forceAll(v, false);
  eng.phase = 2;
  for (let i = 0; i < eng.deferredSlots.length; i++) {
    const d = eng.deferredSlots[i];
    eng.forceSlotSafe(d.inst, d.name);
  }
  for (const v of env.roots.values()) eng.forceAll(v, true);
  eng.validateAll('');
  env.diagnostics.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : (a.id ?? '') < (b.id ?? '') ? -1 : 1);
  return { env, eng };
}

const get = (eng: Engine, segs: (string | number)[]) => eng.resolveSegs(segs);
const diagsOf = (env: Env, sev?: string) => env.diagnostics.filter(d => !sev || d.severity === sev);

// ---------------- case 3: fixtures ----------------
console.log('\n== case 3: fixture generation ==');
{
  const { env, eng } = runModule('docs/examples/03_fixtures.decl');
  for (const d of env.diagnostics) console.log('  diag:', d.severity, d.id ?? d.code, d.path, '-', d.message);
  check('no diagnostics', env.diagnostics.length === 0, JSON.stringify(env.diagnostics));
  const sweep = env.roots.get('sweep');
  check('32 cases', isArr(sweep) && sweep.items.length === 32);
  check('16 packets each', get(eng, ['sweep', 0, 'packet_count']) === 16n);
  check('total_bytes of s64 grid', get(eng, ['sweep', 0, 'total_bytes']) === 1024n);
  check('derived label', get(eng, ['sweep', 0, 'packets', 3, 'label']) === 'pkt-3-p0');
  check('smoke alternates sizes', get(eng, ['smoke', 'packets', 1, 'size_bytes']) === 512n);

  const ser = eng.serialize(env.roots.get('sweep'), 'sweep');
  const env2 = new Env(); env2.load(parseModule(readFileSync('docs/examples/03_fixtures.decl', 'utf8')));
  const eng2 = new Engine(env2);
  const sc2 = { inst: null, locals: new Map<string, any>(), rootName: 'again' };
  const rt2 = env2.resolve({ k: 'array', elem: { k: 'named', name: 'TestCase', args: [] } } as any);
  env2.roots.set('again', eng2.bind(readJson(ser), rt2, ['again'], null, sc2));
  eng2.forceAll(env2.roots.get('again'), false);
  eng2.phase = 2;
  for (let i = 0; i < eng2.deferredSlots.length; i++) eng2.forceSlotSafe(eng2.deferredSlots[i].inst, eng2.deferredSlots[i].name);
  eng2.forceAll(env2.roots.get('again'), true);
  eng2.validateAll('');
  check('round-trip validates', env2.diagnostics.length === 0, JSON.stringify(env2.diagnostics.slice(0, 3)));
  check('round-trip byte-identical', eng2.serialize(env2.roots.get('again'), 'again') === ser);
}

// ---------------- case 2: config ----------------
console.log('\n== case 2: config schema ==');
{
  const { env, eng } = runModule('docs/examples/02_config.decl');
  check('no diagnostics on outputs', env.diagnostics.length === 0, JSON.stringify(env.diagnostics));
  check('base fills defaults', get(eng, ['base', 'workers']) === 4n);
  check('base derived insecure', get(eng, ['base', 'insecure']) === true);
  check('prod layering keeps base host', get(eng, ['prod', 'host']) === 'api.internal');
  check('prod override', get(eng, ['prod', 'workers']) === 32n);
  check('prod nested with', get(eng, ['prod', 'tls', 'cert_path']) === '/etc/ssl/api.pem');
  check('prod derived recomputed', get(eng, ['prod', 'insecure']) === false);
  check('dev port', get(eng, ['dev', 'port']) === 8081n);

  // valid external doc, with an unknown flag (open record passthrough)
  const valid = `{"host":"10.0.0.5","tls":{"enabled":true,"cert_path":"/tmp/c.pem"},"experimental_flag":true,"rate_limits":{"search":50}}`;
  const r1 = runModule('docs/examples/02_config.decl', [{ input: 'deployed', json: valid }]);
  check('valid doc: no diagnostics', r1.env.diagnostics.length === 0, JSON.stringify(r1.env.diagnostics));
  const ser = r1.eng.serialize(r1.env.roots.get('deployed'), 'deployed');
  check('opaque passthrough survives', ser.includes('"experimental_flag":true'), ser);

  // invalid doc: tls without cert (when-guarded assert), huge workers (warn), bad port (range)
  const invalid = `{"host":"x","port":70000,"workers":100,"tls":{"enabled":true}}`;
  const r2 = runModule('docs/examples/02_config.decl', [{ input: 'deployed', json: invalid }]);
  for (const d of r2.env.diagnostics) console.log('  diag:', d.severity, d.id ?? d.code, d.path, '-', d.message);
  const errs = diagsOf(r2.env, 'error'), warns = diagsOf(r2.env, 'warn');
  check('two errors (port range, cert missing)', errs.length === 2, JSON.stringify(errs));
  check('cert assert id', errs.some(d => d.id === 'TlsConfig.cert_present'));
  check('one warning', warns.length === 1 && warns[0].id === 'ServerConfig.sane_workers');
  check('warned value preserved', get(r2.eng, ['deployed', 'workers']) === 100n);
}

// ---------------- case 1: interconnect ----------------
console.log('\n== case 1: interconnect (oic 2x2 crossbar) ==');
{
  const { env, eng } = runModule('docs/examples/01_interconnect.decl');
  for (const d of env.diagnostics) console.log('  diag:', d.severity, d.id ?? d.code, d.path, '-', d.message);
  check('xbar validates clean', env.diagnostics.length === 0);
  check('width propagates to master si', get(eng, ['xbar', 'nodes', 'dom0', 'nodes', 'mst0', 'ports', 'si', 'width']) === 64n);
  check('width propagates through decoder', get(eng, ['xbar', 'nodes', 'dom0', 'nodes', 'dec0', 'ports', 'outs', 'mi1', 'width']) === 64n);
  check('arbiter takes max of inputs', get(eng, ['xbar', 'nodes', 'dom0', 'nodes', 'arb0', 'ports', 'mi', 'width']) === 64n);
  check('slave output reaches boundary width', get(eng, ['xbar', 'nodes', 'dom0', 'nodes', 'slv1', 'ports', 'mi', 'width']) === 64n);

  const ser = eng.serialize(env.roots.get('xbar'), 'xbar');
  check('refs serialize document-relative', ser.includes('"$.nodes.dom0.ports.si0"'), ser.slice(0, 200));

  // round-trip into the input slot, under a different root name
  const src = readFileSync('docs/examples/01_interconnect.decl', 'utf8');
  const env2 = new Env(); env2.load(parseModule(src));
  const eng2 = new Engine(env2);
  const sc2 = { inst: null, locals: new Map<string, any>(), rootName: 'doc' };
  env2.roots.set('doc', eng2.bind(readJson(ser), env2.resolve(env2.inputs.get('doc')!.type), ['doc'], null, sc2));
  eng2.forceAll(env2.roots.get('doc'), false);
  eng2.phase = 2;
  for (let i = 0; i < eng2.deferredSlots.length; i++) eng2.forceSlotSafe(eng2.deferredSlots[i].inst, eng2.deferredSlots[i].name);
  eng2.forceAll(env2.roots.get('doc'), true);
  eng2.validateAll('');
  for (const d of env2.diagnostics) console.log('  rt diag:', d.severity, d.id ?? d.code, d.path, '-', d.message);
  check('round-trip validates under new root', env2.diagnostics.length === 0);
  check('round-trip byte-identical', eng2.serialize(env2.roots.get('doc'), 'doc') === ser);

  // broken document: shrink one boundary port width -> exactly one width_match
  const doc = readJson(ser);
  const ports = doc.entries.find(([k]: any) => k === 'ports')[1];
  const mi1 = ports.entries.find(([k]: any) => k === 'mi1')[1];
  mi1.entries.find(([k]: any) => k === 'width')[1] = 32n;
  const env3 = new Env(); env3.load(parseModule(src));
  const eng3 = new Engine(env3);
  const sc3 = { inst: null, locals: new Map<string, any>(), rootName: 'doc' };
  env3.roots.set('doc', eng3.bind(doc, env3.resolve(env3.inputs.get('doc')!.type), ['doc'], null, sc3));
  eng3.forceAll(env3.roots.get('doc'), false);
  eng3.phase = 2;
  for (let i = 0; i < eng3.deferredSlots.length; i++) eng3.forceSlotSafe(eng3.deferredSlots[i].inst, eng3.deferredSlots[i].name);
  eng3.forceAll(env3.roots.get('doc'), true);
  eng3.validateAll('');
  for (const d of env3.diagnostics) console.log('  broken diag:', d.severity, d.id ?? d.code, d.path, '-', d.message);
  const errs = diagsOf(env3.env ?? env3, 'error');
  check('exactly one width_match error, at the root cause', errs.length === 1 && errs[0].id === 'Edge.width_match', JSON.stringify(errs));
}

console.log(`\n${pass} ok, ${failCnt} failed`);
process.exitCode = failCnt > 0 ? 1 : 0;
