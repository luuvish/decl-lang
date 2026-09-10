// Query entry points use shared executable programs and the value layer's
// slots, dependency graph, reference rounds, binding, and diagnostics.
import { Engine } from '../engine.ts';
import { sortDiags } from '../semantics.ts';
import type { Env, Diag } from '../semantics.ts';
import type { Expr, TypeAst } from '../ast.ts';

/** Kept for callers that explicitly handle a query capability fallback. */
export class Unsupported extends Error {
  constructor(form: string) {
    super(`qeval: unsupported ${form}`);
  }
}

interface RootSpec {
  name: string;
  expr: Expr;
  type: TypeAst;
  menv: Env;
}
/** a bound input document (§5.5): its raw value and the module that declares it */
interface BoundSpec {
  name: string;
  raw: unknown;
  menv: Env;
}
export interface QReport {
  ok: boolean;
  outputs: { name: string; json: string }[];
  diagnostics: Diag[];
}
/** The settled values and report of the shared reference-round driver. */
export interface RoundsResult {
  report: QReport;
  eng: Engine; // the final round's value layer, for serialization by a caller
}
function evalRounds(
  entry: Env,
  roots: RootSpec[],
  binds: BoundSpec[],
  mods: { env: Env }[],
  serializeOutputs: boolean,
): RoundsResult {
  const eng = Engine.evaluate(
    entry,
    (helper) => {
      for (const m of mods) {
        m.env.constEval = (n: string) => helper.forceConstIn(m.env, n, '');
        m.env.exprEval = (e: Expr) =>
          helper.ev(e, { inst: null, locals: new Map(), rootName: '', menv: m.env });
      }
      for (const b of binds) {
        const decl = b.menv.inputs.get(b.name);
        if (decl)
          helper.bindRoot(
            b.name,
            b.raw,
            b.menv.resolve(decl.type),
            { inst: null, locals: new Map(), rootName: b.name, menv: b.menv },
            false,
          );
      }
      for (const o of roots)
        helper.bindRoot(
          o.name,
          o.expr,
          o.menv.resolve(o.type),
          { inst: null, locals: new Map(), rootName: o.name, menv: o.menv },
          true,
        );
    },
    () => entry.roots.values(),
    false,
    true,
  );
  eng.validateAll('');
  const diagnostics = sortDiags(entry.diagnostics.slice());
  entry.diagnostics.splice(0, entry.diagnostics.length, ...diagnostics);
  const ok = !diagnostics.some((d) => d.severity === 'error');
  const outputs =
    ok && serializeOutputs
      ? roots
          .filter((o) => entry.roots.has(o.name))
          .map((o) => ({ name: o.name, json: eng.serialize(entry.roots.get(o.name), o.name) }))
      : [];
  return { eng, report: { ok, outputs, diagnostics } };
}

/** evaluate a single module's outputs (its own env is the whole universe) */
export function qevaluate(env: Env): QReport {
  return evalRounds(
    env,
    env.outputs.map((o) => ({ ...o, menv: env })),
    [],
    [],
    true,
  ).report;
}

/**
 * Evaluate a whole multi-module universe (§8.8): every module's outputs are
 * roots, each bound and evaluated in its own module scope, into the entry
 * module's universe. Imported names and non-entry-module consts/functions are
 * resolved through the value layer. Returns the report and the final round's
 * value layer (its roots populated and forced) so a caller — the pipeline
 * swap-in — can serialize through it exactly as runUniverse does.
 */
export function qevaluateUniverse(
  mods: { env: Env }[],
  entry: { env: Env },
  binds: { module?: { env: Env }; input: string; raw: unknown }[] = [],
  // Module consumers serialize only the requested roots in their requested
  // format. They need evaluated values and diagnostics, not discarded JSON.
  serializeOutputs = true,
): RoundsResult {
  const roots: RootSpec[] = mods.flatMap((m) =>
    m.env.outputs.map((o) => ({ name: o.name, expr: o.expr, type: o.type, menv: m.env })),
  );
  const bound: BoundSpec[] = binds.map((b) => ({
    name: b.input,
    raw: b.raw,
    menv: (b.module ?? entry).env,
  }));
  return evalRounds(entry.env, roots, bound, mods, serializeOutputs);
}
