// The single-module pipeline (platform-neutral): bind and evaluate every
// output of one module's declarations — the judgment the conformance
// runner applies and the playground runs — and the source-level report
// that front-ends (the website, embedders) consume.
import { parseSource } from './parse.ts';
import { checkModule } from './checker.ts';
import { Env, sortDiags } from './semantics.ts';
import type { Diag } from './semantics.ts';
import { Engine } from './engine.ts';
import type { Decl } from './ast.ts';
import { qevaluate, Unsupported } from './qengine/qeval.ts';

/** the incremental query engine may be selected in place of the tree walker
 *  (qengine/DESIGN.md); it is a drop-in, byte-identical evaluator, off unless
 *  DECL_QENGINE is set — while it is validated across the whole corpus */
const qEnv = (k: string): boolean =>
  !!(globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env[k];
const useQEngine = (): boolean => qEnv('DECL_QENGINE');
/** strict mode: an unhandled form is an error, not a silent fall back to the
 *  tree walker — used to prove the query engine covers a corpus end to end */
export const strictQEngine = (): boolean => qEnv('DECL_QENGINE_STRICT');

export type Pipeline = { env: Env; eng: Engine; diags: Diag[] };

export function runPipeline(decls: Decl[]): Pipeline {
  const env = new Env();
  env.load(decls);
  const eng = Engine.evaluate(
    env,
    (eng) => {
      for (const o of env.outputs) {
        const sc = { inst: null, locals: new Map<string, any>(), rootName: o.name };
        eng.bindRoot(o.name, o.expr, env.resolve(o.type), sc, true);
      }
    },
    () => env.roots.values(),
  );
  eng.validateAll('');
  env.diagnostics.splice(0, env.diagnostics.length, ...sortDiags(env.diagnostics)); // §6.7
  return { env, eng, diags: env.diagnostics };
}

export type Report = {
  /** the phase that decided the report */
  phase: 'parse' | 'check' | 'evaluate';
  ok: boolean;
  parseErrors: { row: number; col: number }[];
  /** static-checker diagnostics */
  checks: Diag[];
  /** binding / evaluation / assertion diagnostics */
  diagnostics: Diag[];
  /** every `output`, serialized as canonical JSON text */
  outputs: { name: string; json: string }[];
  /** input roots declared by the module (not bound here) */
  inputs: string[];
};

/** parse, check, and evaluate one module given as source text */
export function evaluateSource(source: string): Report {
  const { decls, errors } = parseSource(source);
  const inputs = decls.filter((d: any) => d.d === 'input').map((d: any) => d.name as string);
  if (errors.length)
    return {
      phase: 'parse',
      ok: false,
      parseErrors: errors,
      checks: [],
      diagnostics: [],
      outputs: [],
      inputs,
    };
  const checks = checkModule(decls);
  if (checks.some((d) => d.severity === 'error')) {
    return {
      phase: 'check',
      ok: false,
      parseErrors: [],
      checks,
      diagnostics: [],
      outputs: [],
      inputs,
    };
  }
  if (useQEngine()) {
    try {
      const env = new Env();
      env.load(decls);
      const qr = qevaluate(env);
      return {
        phase: 'evaluate',
        ok: qr.ok,
        parseErrors: [],
        checks,
        diagnostics: qr.diagnostics,
        outputs: qr.outputs,
        inputs,
      };
    } catch (e) {
      if (!(e instanceof Unsupported) || strictQEngine()) throw e; // else fall back
    }
  }
  const { env, eng, diags } = runPipeline(decls);
  const ok = !diags.some((d) => d.severity === 'error');
  const outputs = ok
    ? env.outputs
        .filter((o) => env.roots.has(o.name))
        .map((o) => ({ name: o.name, json: eng.serialize(env.roots.get(o.name), o.name) }))
    : [];
  return { phase: 'evaluate', ok, parseErrors: [], checks, diagnostics: diags, outputs, inputs };
}
