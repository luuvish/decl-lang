// Phase 5: the committable third domain library — synthetic spine-leaf
// fabric documents bound as `input` against examples/fabric/fabric.decl.
// Deterministically generated documents exercise the same structural
// stress points as the proprietary sweep (map containers, type tags,
// parameter bags, cross-references, scale, corruption detection).
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import { initParser, parseSource } from '../src/parse.ts';
import { Env, readJson } from '../src/semantics.ts';
import { Engine } from '../src/engine.ts';
import { checkModule } from '../src/checker.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};

await initParser();

const schemaSrc = readFileSync(join(root, 'examples/fabric/fabric.decl'), 'utf8');
const { decls, errors } = parseSource(schemaSrc);
check('fabric schema parses', errors.length === 0);
check('fabric schema checks clean', checkModule(decls).length === 0, JSON.stringify(checkModule(decls).slice(0, 3)));

// ---- deterministic document generator: an S-spine, L-leaf site ----
function genSite(spines: number, leafs: number): any {
  const eth = (name: string, dir: string, gbps: number, vlan?: number) => ({
    kind: 'fabric.port.eth', name, dir,
    params: { speed: { value: gbps * 1e9, unit: 'bps' }, mtu: 9000, ...(vlan ? { vlan } : {}) },
  });
  const nodes: any = {};
  const edges: any = {};
  for (let s = 0; s < spines; s++) {
    const ports: any = {};
    for (let l = 0; l < leafs; l++) ports[`dn${l}`] = eth(`dn${l}`, 'down', 100);
    nodes[`spine${s}`] = { kind: 'fabric.node.switch.spine', name: `spine${s}`, ports };
  }
  for (let l = 0; l < leafs; l++) {
    const ports: any = {};
    for (let s = 0; s < spines; s++) ports[`up${s}`] = eth(`up${s}`, 'up', 100);
    for (let h = 0; h < 4; h++) ports[`host${h}`] = eth(`host${h}`, 'down', 25, 100 + (l % 5));
    nodes[`leaf${l}`] = {
      kind: 'fabric.node.switch.leaf', name: `leaf${l}`, ports,
      nodes: { [`rack${l}`]: { kind: 'fabric.node.rack', name: `rack${l}` } },
    };
  }
  for (let s = 0; s < spines; s++) for (let l = 0; l < leafs; l++) {
    const n = `sl${s}x${l}`;
    edges[n] = { kind: 'fabric.edge.link', name: n, endpoints: [`spine${s}`, `leaf${l}`] };
  }
  return {
    kind: 'fabric.node.site', name: 'site_a',
    params: {
      uplink: 'wan0', oversubscription: 4,
      subnets: [
        { cidr: '10.0.0.0/16', vlan: 100, gateway: 'wan0' },
        { cidr: '10.1.0.0/16', vlan: 101 },
        { cidr: '172.16.0.0/12', vlan: 200 },
      ],
    },
    ports: { wan0: eth('wan0', 'up', 400) },
    nodes, edges,
  };
}

function validate(doc: any): { errs: any[]; env: Env; eng: Engine } {
  const env = new Env();
  env.load(decls);
  const eng = new Engine(env);
  const sc = { inst: null, locals: new Map<string, any>(), rootName: 'site' };
  try {
    env.roots.set('site', eng.bind(readJson(JSON.stringify(doc)), env.resolve({ k: 'named', name: 'Fabric', args: [] } as any), ['site'], null, sc));
    eng.forceAll(env.roots.get('site'), true);
    eng.validateAll('');
  } catch (e: any) {
    env.diagnostics.push({ severity: 'error', message: `crash: ${e.message}`, path: '' });
  }
  return { errs: env.diagnostics.filter(d => d.severity === 'error'), env, eng };
}

console.log('== synthetic sites validate ==');
{
  const { errs, eng, env } = validate(genSite(2, 4));
  check('2x4 site validates clean', errs.length === 0, JSON.stringify(errs.slice(0, 2)));
  const speed = eng.resolveSegs(['site', 'ports', 'wan0', 'params', 'speed']) as any;
  check('bandwidth normalizes to its base unit', speed && speed.dim === 'DataSize*Time^-1' && speed.value === 4e11);
  const ser1 = eng.serialize(env.roots.get('site'), 'site');
  const { errs: e2, eng: eng2, env: env2 } = validate(JSON.parse(ser1));
  check('serialized site re-validates (round trip)', e2.length === 0, JSON.stringify(e2.slice(0, 2)));
  check('round trip is byte-identical', eng2.serialize(env2.roots.get('site'), 'site') === ser1);

  for (const [s, l] of [[4, 8], [10, 20], [20, 50]] as [number, number][]) {
    const t0 = Date.now();
    const { errs } = validate(genSite(s, l));
    check(`${s}x${l} site (${s * l} links) validates clean in ${Date.now() - t0}ms`, errs.length === 0, JSON.stringify(errs.slice(0, 2)));
  }
}

console.log('== corruption is detected ==');
{
  const cases: [string, (d: any) => void, string][] = [
    ['bad port direction', d => { d.ports.wan0.dir = 'sideways'; }, 'dir'],
    ['zero line rate', d => { d.ports.wan0.params.speed.value = 0; }, 'eth_has_speed'],
    ['dangling edge endpoint', d => { d.edges.sl0x0.endpoints = ['spine0', 'ghost9']; }, 'edge_endpoints_exist'],
    ['dangling uplink', d => { d.params.uplink = 'no_such_port'; }, 'uplink_exists'],
    ['duplicate subnet vlan', d => { d.params.subnets[1].vlan = 100; }, 'subnet_vlans_distinct'],
    ['edge key mismatch', d => { d.edges.sl0x0.name = 'other'; }, 'edge_keys_match'],
  ];
  for (const [name, corrupt, marker] of cases) {
    const doc = genSite(2, 4);
    corrupt(doc);
    const { errs } = validate(doc);
    check(`corruption detected: ${name}`, errs.length > 0
      && errs.some(e => `${e.id ?? ''}${e.message}${e.path}`.includes(marker)), JSON.stringify(errs.slice(0, 2)));
  }
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
