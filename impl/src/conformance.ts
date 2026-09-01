// Conformance runner (ROADMAP Phase 2): judges every fixture under
// tests/validation by its declared phase.
//   valid/*                          -> parse clean + static checks clean
//   invalid @expect-phase: parsing   -> must fail to parse
//   invalid @expect-phase: checking  -> parses; static checks report @expect-error
//   invalid @expect-phase: binding   -> parses; the pipeline reports @expect-error
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initParser, parseSource } from './parse.ts';
import { Env, readJson } from './semantics.ts';
import { Engine } from './engine.ts';
import { checkModule } from './checker.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');

function* walk(dir: string): Generator<string> {
  for (const e of readdirSync(dir)) {
    const p = join(dir, e);
    if (statSync(p).isDirectory()) yield* walk(p);
    else if (p.endsWith('.decl')) yield p;
  }
}

function runPipeline(decls: any[]) {
  const env = new Env();
  env.load(decls);
  const eng = new Engine(env);
  for (const o of env.outputs) {
    const sc = { inst: null, locals: new Map<string, any>(), rootName: o.name };
    try { env.roots.set(o.name, eng.bind(eng.ev(o.expr, sc), env.resolve(o.type), [o.name], null, sc)); }
    catch { }
  }
  for (const v of env.roots.values()) eng.forceAll(v, false);
  eng.phase = 2;
  for (let i = 0; i < eng.deferredSlots.length; i++) eng.forceSlotSafe(eng.deferredSlots[i].inst, eng.deferredSlots[i].name);
  for (const v of env.roots.values()) eng.forceAll(v, true);
  eng.validateAll('');
  return env.diagnostics;
}

await initParser();
let ok = 0, fail = 0;
for (const file of walk(join(root, 'tests/validation'))) {
  const rel = file.slice(root.length + 1);
  const src = readFileSync(file, 'utf8');
  const meta = Object.fromEntries(
    [...src.matchAll(/\/\/ @([a-z-]+):\s*(.+)/g)].map(m => [m[1], m[2].trim()]),
  );
  const isValid = rel.includes('/valid/');
  const phase = meta['expect-phase'];
  const wantCode = meta['expect-error'];
  const wantMsg = meta['expect-message'];

  let verdict = false, detail = '';
  const { decls, errors } = parseSource(src);
  if (isValid) {
    // a valid fixture must parse, check clean, AND evaluate its outputs
    // without error-severity diagnostics
    const checks = errors.length === 0 ? checkModule(decls) : [];
    const evalErrs = errors.length === 0 && checks.length === 0
      ? runPipeline(decls).filter(d => d.severity === 'error') : [];
    verdict = errors.length === 0 && checks.length === 0 && evalErrs.length === 0;
    detail = errors.length ? `${errors.length} parse errors` : JSON.stringify([...checks, ...evalErrs]);
  } else if (phase === 'parsing') {
    verdict = errors.length > 0;
    detail = 'expected parse errors, got none';
  } else if (phase === 'checking') {
    const checks = errors.length === 0 ? checkModule(decls) : [];
    verdict = checks.some(d => d.code === wantCode)
      && (!wantMsg || checks.some(d => d.message.includes(wantMsg)));
    detail = JSON.stringify(checks);
  } else if (phase === 'binding') {
    const diags = errors.length === 0 ? runPipeline(decls) : [];
    verdict = diags.some(d => d.code === wantCode)
      && (!wantMsg || diags.some(d => d.message.includes(wantMsg)));
    detail = JSON.stringify(diags);
  } else {
    detail = `unknown phase ${phase}`;
  }
  if (verdict) { ok++; console.log(`  ok   ${rel}`); }
  else { fail++; console.log(`  FAIL ${rel} ${detail}`); }
}
console.log(`\n${ok} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
