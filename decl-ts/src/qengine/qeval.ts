// The decl layer of the query engine (qengine/DESIGN.md), stages 2a–2b: compile
// expressions to query computes and evaluate a module's outputs through the
// incremental core (db.ts).
//   2a — scalar/const computation: literals, unary/binary operators, `if`,
//        `name` -> const.
//   2b — records: an object literal bound to a record type becomes a RecInst
//        whose members are slot QUERIES; a member that names a sibling reads it
//        through the query graph (`slot b = a + 1` queries slot a), so the graph
//        does the cross-slot dependency work. Required and derived scalar
//        members are covered; other member kinds, nesting, and refs come later.
// Value semantics — equality, serialization, and type binding — are reused from
// the current value layer, as the design intends; only the evaluation strategy
// is new.
import { Db } from './db.ts';
import { Engine } from '../engine.ts';
import { ABSENT, EvalErr, pathStr, valueEq } from '../semantics.ts';
import type { Env, RecInst, Value, Diag, Seg, RT } from '../semantics.ts';
import type { Expr } from '../ast.ts';

/** an expression or member form this stage does not compile yet */
export class Unsupported extends Error {
  constructor(form: string) {
    super(`qeval: unsupported ${form}`);
  }
}

/** a compiled expression: a closure over the query context */
type Op = (cx: { query(key: string): unknown }) => Value;
/** resolve a name to the query key it reads, or undefined if it is not in scope */
type Names = (name: string) => string | undefined;

// ---- value-level operators (§4.4–4.5), mirroring the reference exactly ----

function truthy(v: Value): boolean {
  if (typeof v === 'boolean') return v;
  throw new EvalErr('non-bool condition');
}

function applyUn(op: string, x: Value): Value {
  if (op === '!') return !truthy(x);
  if (op === '-') return typeof x === 'bigint' ? -x : -x;
  if (op === '~') return ~x;
  throw new EvalErr('un');
}

function applyBin(op: string, l: Value, r: Value): Value {
  if (op === '==') return valueEq(l, r);
  if (op === '!=') return !valueEq(l, r);
  if (l === ABSENT || r === ABSENT) throw new EvalErr('absent consumed');
  const bothI = typeof l === 'bigint' && typeof r === 'bigint';
  const bothF = typeof l === 'number' && typeof r === 'number';
  const bothS = typeof l === 'string' && typeof r === 'string';
  // bind operands before the same-kind guards narrow them, so the ops stay
  // well typed (the guards ensure a matching kind at runtime)
  const a = l;
  const b = r;
  switch (op) {
    case '+':
      if (bothS || bothI || bothF) return a + b;
      break;
    case '-':
      if (bothI || bothF) return a - b;
      break;
    case '*':
      if (bothI || bothF) return a * b;
      break;
    case '/':
      if (bothI) {
        if (r === 0n) throw new EvalErr('division by zero', 'E5001');
        return a / b;
      }
      if (bothF) {
        if (r === 0) throw new EvalErr('division by zero', 'E5001');
        const q = a / b;
        if (!isFinite(q)) throw new EvalErr('non-finite', 'E5002');
        return q;
      }
      break;
    case '%':
      if (bothI) {
        if (r === 0n) throw new EvalErr('mod zero', 'E5001');
        return a % b;
      }
      break;
    case '<':
    case '<=':
    case '>':
    case '>=':
      if (bothI || bothF || bothS) {
        if (op === '<') return a < b;
        if (op === '<=') return a <= b;
        if (op === '>') return a > b;
        return a >= b;
      }
      break;
    case '&':
      if (bothI) return a & b;
      break;
    case '|':
      if (bothI) return a | b;
      break;
    case '^':
      if (bothI) return a ^ b;
      break;
    case '<<':
      if (bothI) return a << b;
      break;
    case '>>':
      if (bothI) return a >> b;
      break;
  }
  throw new EvalErr(`bad operands for ${op}`);
}

// ---- compile: an expression AST becomes a closure over the query context ----

function compile(e: Expr, names: Names): Op {
  switch (e.e) {
    case 'lit': {
      const v = e.v;
      return () => v;
    }
    case 'paren':
      return compile(e.x, names);
    case 'un': {
      const op = e.op;
      const xc = compile(e.x, names);
      return (cx) => applyUn(op, xc(cx));
    }
    case 'if': {
      const c = compile(e.c, names);
      const t = compile(e.t, names);
      const f = compile(e.f, names);
      return (cx) => (truthy(c(cx)) ? t(cx) : f(cx));
    }
    case 'bin': {
      const op = e.op;
      const lc = compile(e.l, names);
      const rc = compile(e.r, names);
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
      const key = names(e.name);
      if (key === undefined) throw new Unsupported(`name ${e.name}`);
      return (cx) => cx.query(key) as Value;
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

interface SlotJob {
  op: Op;
  type: RT | undefined; // resolved member type
  path: Seg[];
  inst: RecInst;
}

class QEval {
  private readonly env: Env;
  private readonly helper: Engine; // value layer only: type binding + serialization
  private readonly db: Db;
  private readonly constOps = new Map<string, Op>();
  private readonly slotJobs = new Map<string, SlotJob>();
  private readonly diagnostics: Diag[] = [];

  constructor(env: Env) {
    this.env = env;
    this.helper = new Engine(env);
    const constNames: Names = (n) => (env.consts.has(n) ? `const:${n}` : undefined);
    for (const [name, c] of env.consts) this.constOps.set(name, compile(c.expr, constNames));
    this.db = new Db({
      eq: valueEq,
      resolve: (key) => {
        if (key.startsWith('const:')) {
          const op = this.constOps.get(key.slice(6));
          return op ? (cx) => op(cx) : undefined;
        }
        if (key.startsWith('slot:')) {
          const job = this.slotJobs.get(key);
          return job ? () => this.runSlot(job) : undefined;
        }
        return undefined;
      },
    });
  }

  private runSlot(job: SlotJob): Value {
    let v = job.op({ query: (k) => this.db.query(k) });
    if (job.type) {
      const sc = {
        inst: job.inst,
        locals: new Map<string, unknown>(),
        rootName: job.path[0] as string,
        menv: this.env,
      };
      v = this.helper.bind(v, job.type, job.path, job.inst, sc);
    }
    return v;
  }

  /** bind an object literal to a record type: a RecInst whose members are slot queries */
  private bindRecord(entries: { key: string; val: Expr }[], rt: RT, path: Seg[]): RecInst {
    const supplied = new Map(entries.map((e) => [e.key, e.val]));
    const instId = pathStr(path);
    const inst: RecInst = {
      __rec: true,
      typeName: rt.name,
      rt,
      path,
      parent: null,
      slots: new Map(),
      entryOrder: entries.map((e) => e.key),
      extras: new Map(),
    };
    (inst as unknown as { menv: Env }).menv = this.env;
    (inst as unknown as { eng: Engine }).eng = this.helper;
    this.env.registry.push(inst);

    const memberNames = new Set<string>(rt.members.map((m: { name: string }) => m.name));
    const names: Names = (n) =>
      memberNames.has(n)
        ? `slot:${instId}.${n}`
        : this.env.consts.has(n)
          ? `const:${n}`
          : undefined;

    for (const m of rt.members) {
      if (m.kind === 'opt' || m.kind === 'dflt') throw new Unsupported(`member kind ${m.kind}`);
      if (m.type && m.type.t === 'rec') throw new Unsupported('nested record member');
      let op: Op;
      if (m.kind === 'der') op = compile(m.expr, names);
      else {
        const val = supplied.get(m.name);
        if (val === undefined) throw new Unsupported(`missing member ${m.name}`);
        op = compile(val, names);
      }
      this.slotJobs.set(`slot:${instId}.${m.name}`, {
        op,
        type: m.type,
        path: [...path, m.name],
        inst,
      });
      inst.slots.set(m.name, {
        kind: m.kind,
        hidden: m.hidden || undefined,
        state: 'unforced',
        deferred: false,
      });
    }

    // materialize each slot into the RecInst so serialization can read it
    for (const m of rt.members) {
      const s = inst.slots.get(m.name)!;
      try {
        s.value = this.db.query(`slot:${instId}.${m.name}`) as Value;
        s.state = 'ok';
      } catch (err) {
        if (err instanceof EvalErr) {
          s.state = 'invalid';
          this.diagnostics.push({
            severity: 'error',
            message: err.message,
            path: pathStr([...path, m.name]),
            code: (err as { code?: string }).code,
          });
        } else throw err;
      }
    }
    return inst;
  }

  run(): QReport {
    const outputs: { name: string; json: string }[] = [];
    let ok = true;
    for (const o of this.env.outputs) {
      try {
        const rt = this.env.resolve(o.type);
        let v: Value;
        if (rt.t === 'rec' && o.expr.e === 'obj') {
          v = this.bindRecord(o.expr.entries, rt, [o.name]);
        } else {
          const constNames: Names = (n) => (this.env.consts.has(n) ? `const:${n}` : undefined);
          const raw = compile(o.expr, constNames)({ query: (k) => this.db.query(k) });
          const sc = {
            inst: null,
            locals: new Map<string, unknown>(),
            rootName: o.name,
            menv: this.env,
          };
          v = this.helper.bind(raw, rt, [o.name], null, sc);
        }
        this.env.roots.set(o.name, v);
        outputs.push({ name: o.name, json: this.helper.serialize(v, o.name) });
      } catch (err) {
        if (err instanceof EvalErr) {
          ok = false;
          this.diagnostics.push({
            severity: 'error',
            message: err.message,
            path: o.name,
            code: (err as { code?: string }).code,
          });
        } else throw err; // Unsupported (or a real bug) bubbles to the caller
      }
    }
    if (this.diagnostics.some((d) => d.severity === 'error')) ok = false;
    return { ok, outputs, diagnostics: this.diagnostics };
  }
}

export function qevaluate(env: Env): QReport {
  return new QEval(env).run();
}
