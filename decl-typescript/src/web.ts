// The browser entry of the reference implementation — what the website's
// playground runs. Single-module only (no file system): parse, static
// checks, evaluation with the same pipeline the conformance runner uses,
// canonical serialization, and the formatter.
import { initParser, parseSource } from './parse.ts';
import { checkModule } from './checker.ts';
import { Env } from './semantics.ts';
import type { Diag } from './semantics.ts';
import { Engine } from './engine.ts';
import { format, initFormatter } from './fmt.ts';

export type Report = {
  /** the phase that decided the report */
  phase: 'parse' | 'check' | 'evaluate';
  ok: boolean;
  parseErrors: { row: number; col: number }[];
  /** static-checker diagnostics (E1xxx/E2xxx bands and friends) */
  checks: Diag[];
  /** binding / evaluation / assertion diagnostics */
  diagnostics: Diag[];
  /** every `output`, serialized as canonical JSON text */
  outputs: { name: string; json: string }[];
  /** input roots declared by the module (not bound in the playground) */
  inputs: string[];
};

/** `base` is the URL directory holding tree-sitter-decl.wasm and tree-sitter.wasm */
export async function init(base: string): Promise<void> {
  const dir = base.endsWith('/') ? base : `${base}/`;
  await initParser({ grammar: `${dir}tree-sitter-decl.wasm`, runtime: `${dir}tree-sitter.wasm` });
  await initFormatter();
}

export function run(source: string): Report {
  const { decls, errors } = parseSource(source);
  const inputs = decls.filter((d: any) => d.d === 'input').map((d: any) => d.name as string);
  if (errors.length) return { phase: 'parse', ok: false, parseErrors: errors, checks: [], diagnostics: [], outputs: [], inputs };
  const checks = checkModule(decls);
  if (checks.some(d => d.severity === 'error')) {
    return { phase: 'check', ok: false, parseErrors: [], checks, diagnostics: [], outputs: [], inputs };
  }
  // the single-module pipeline of conformance.ts, keeping the engine to serialize
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
  const diagnostics = env.diagnostics;
  const ok = !diagnostics.some(d => d.severity === 'error');
  const outputs = ok
    ? env.outputs.filter(o => env.roots.has(o.name)).map(o => ({ name: o.name, json: eng.serialize(env.roots.get(o.name), o.name) }))
    : [];
  return { phase: 'evaluate', ok, parseErrors: [], checks, diagnostics, outputs, inputs };
}

/** canonical formatting; throws on syntax errors */
export function fmt(source: string): string {
  return format(source);
}
