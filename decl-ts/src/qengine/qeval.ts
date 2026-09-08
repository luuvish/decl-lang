// The decl layer of the query engine (qengine/DESIGN.md), stage 2a: compile
// expressions to query computes and evaluate them through the incremental core
// (db.ts). This stage covers scalar/const computation — literals, unary and
// binary operators, `if`, and `name` references to a `const` — enough to drive
// the whole path (compile -> memoized query -> value) end to end and diff it
// against the current engine. Records, refs, comprehensions, `$referrers`, and
// the rest arrive in later stages. Value semantics (equality, serialization,
// type binding) are reused from the current value layer, as the design intends;
// only the *evaluation strategy* is new here.
import { Db } from './db.ts';
import { Engine } from '../engine.ts';
import { ABSENT, EvalErr, valueEq } from '../semantics.ts';
import type { Env, Value, Diag } from '../semantics.ts';
import type { Expr } from '../ast.ts';

/** an expression form this stage does not compile yet */
export class Unsupported extends Error {
  constructor(form: string) {
    super(`qeval: unsupported ${form}`);
  }
}

type Op = (cx: { query(key: string): unknown }) => Value;

// ---- value-level operators (§4.4–4.5), mirroring the reference exactly ----

function truthy(v: Value): boolean {
  if (typeof v === 'boolean') return v;
  throw new EvalErr('non-bool condition');
}

function applyUn(op: string, x: Value): Value {
  if (op === '!') return !truthy(x);
  if (op === '-') return typeof x === 'bigint' ? -x : -(x as number);
  if (op === '~') return ~(x as bigint);
  throw new EvalErr('un');
}

function applyBin(op: string, l: Value, r: Value): Value {
  if (op === '==') return valueEq(l, r);
  if (op === '!=') return !valueEq(l, r);
  if (l === ABSENT || r === ABSENT) throw new EvalErr('absent consumed');
  const bothI = typeof l === 'bigint' && typeof r === 'bigint';
  const bothF = typeof l === 'number' && typeof r === 'number';
  const bothS = typeof l === 'string' && typeof r === 'string';
  switch (op) {
    case '+':
      if (bothS) return l + r;
      if (bothI || bothF) return (l as number) + (r as number);
      break;
    case '-':
      if (bothI || bothF) return (l as number) - (r as number);
      break;
    case '*':
      if (bothI || bothF) return (l as number) * (r as number);
      break;
    case '/':
      if (bothI) {
        if (r === 0n) throw new EvalErr('division by zero', 'E5001');
        return l / r;
      }
      if (bothF) {
        if (r === 0) throw new EvalErr('division by zero', 'E5001');
        const q = l / r;
        if (!isFinite(q)) throw new EvalErr('non-finite', 'E5002');
        return q;
      }
      break;
    case '%':
      if (bothI) {
        if (r === 0n) throw new EvalErr('mod zero', 'E5001');
        return l % r;
      }
      break;
    case '<':
    case '<=':
    case '>':
    case '>=':
      if (bothI || bothF || bothS) {
        if (op === '<') return (l as number) < (r as number);
        if (op === '<=') return (l as number) <= (r as number);
        if (op === '>') return (l as number) > (r as number);
        return (l as number) >= (r as number);
      }
      break;
    case '&':
      if (bothI) return l & r;
      break;
    case '|':
      if (bothI) return l | r;
      break;
    case '^':
      if (bothI) return l ^ r;
      break;
    case '<<':
      if (bothI) return l << r;
      break;
    case '>>':
      if (bothI) return l >> r;
      break;
  }
  throw new EvalErr(`bad operands for ${op}`);
}

// ---- compile: an expression AST becomes a closure over the query context ----

function compile(e: Expr): Op {
  switch (e.e) {
    case 'lit': {
      const v = e.v;
      return () => v;
    }
    case 'paren':
      return compile(e.x);
    case 'un': {
      const op = e.op;
      const xc = compile(e.x);
      return (cx) => applyUn(op, xc(cx));
    }
    case 'if': {
      const c = compile(e.c);
      const t = compile(e.t);
      const f = compile(e.f);
      return (cx) => (truthy(c(cx)) ? t(cx) : f(cx));
    }
    case 'bin': {
      const op = e.op;
      const lc = compile(e.l);
      const rc = compile(e.r);
      if (op === '&&') return (cx) => (truthy(lc(cx)) ? truthy(rc(cx)) : false);
      if (op === '||') return (cx) => (truthy(lc(cx)) ? true : truthy(rc(cx)));
      if (op === '??')
        return (cx) => {
          const l = lc(cx);
          return l === ABSENT || l === null ? rc(cx) : l;
        };
      return (cx) => applyBin(op, lc(cx), rc(cx));
    }
    case 'name': {
      const n = e.name;
      return (cx) => cx.query(`const:${n}`) as Value;
    }
    default:
      throw new Unsupported(e.e);
  }
}

export interface QReport {
  ok: boolean;
  outputs: { name: string; json: string }[];
  diagnostics: Diag[];
}

/**
 * Evaluate a module's outputs through the query engine (stage 2a subset). The
 * consts are derived queries (memoized, dependency-tracked); each output is
 * compiled, evaluated, bound to its declared type, and serialized. Throws
 * `Unsupported` if the module uses a form this stage does not compile yet.
 */
export function qevaluate(env: Env): QReport {
  const helper = new Engine(env); // value layer only: type binding + serialization
  const constOps = new Map<string, Op>();
  for (const [name, c] of env.consts) constOps.set(name, compile(c.expr));
  const outputs: { name: string; op: Op }[] = env.outputs.map((o) => ({
    name: o.name,
    op: compile(o.expr),
  }));

  const db = new Db({
    eq: valueEq,
    resolve: (key) => {
      if (key.startsWith('const:')) {
        const op = constOps.get(key.slice(6));
        return op ? (cx) => op(cx) : undefined;
      }
      return undefined;
    },
  });

  const diagnostics: Diag[] = [];
  const out: { name: string; json: string }[] = [];
  let ok = true;
  for (const o of env.outputs) {
    const op = outputs.find((x) => x.name === o.name)!.op;
    try {
      const raw = op({ query: (k) => db.query(k) });
      const rt = env.resolve(o.type);
      const sc = { inst: null, locals: new Map<string, unknown>(), rootName: o.name, menv: env };
      const v = helper.bind(raw, rt, [o.name], null, sc);
      env.roots.set(o.name, v);
      out.push({ name: o.name, json: helper.serialize(v, o.name) });
    } catch (err) {
      if (err instanceof EvalErr) {
        ok = false;
        diagnostics.push({
          severity: 'error',
          message: err.message,
          path: o.name,
          code: (err as { code?: string }).code,
        });
      } else {
        throw err; // Unsupported (or a real bug) bubbles to the caller
      }
    }
  }
  return { ok, outputs: out, diagnostics };
}
