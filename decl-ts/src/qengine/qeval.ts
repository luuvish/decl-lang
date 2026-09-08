// The decl layer of the query engine (qengine/DESIGN.md). Compile expressions to
// query computes and evaluate a module's outputs through the incremental core
// (db.ts). Covered so far:
//   scalars/const, records (slots as queries, cross-slot & nested & member
//   access; required/optional/default/derived/hidden members; restate and
//   missing/undeclared diagnostics), references ($this/$parent/$root/$key,
//   deref/navigation),
//   arrays & array comprehensions, ranges, indexing, maps & map comprehensions,
//   `in`/`matches`, patterns, string templates, `match` over literal unions,
//   quantities and dimensional arithmetic, std/module-function calls, lambdas,
//   and the pipe.
// Value semantics — equality, serialization, type binding, iteration, type
// membership, unit resolution, quantity arithmetic, function application,
// assertions/when-blocks — are reused from the current value layer, as the
// design intends; only the evaluation strategy is new. Assertions run over the
// forced universe (run → validateAll). `ref<T>` navigation members, context
// declarations, record-bearing unions, non-literal record values, `$referrers`,
// and the rest come in later stages. (A closure passed to a std function has its body run by the
// value layer for now — pure over its parameters and consts, so byte-identical;
// closures compile into the query graph in a later stage.)
import { Db } from './db.ts';
import { Engine } from '../engine.ts';
import {
  ABSENT,
  EvalErr,
  compilePattern,
  isArr,
  isMap,
  isQ,
  isRange,
  isRec,
  isRef,
  mapKey,
  patternError,
  pathStr,
  segText,
  valueEq,
} from '../semantics.ts';
import type { Env, RecInst, Value, Diag, Seg, RT } from '../semantics.ts';
import type { Expr, TypeAst } from '../ast.ts';

/** an expression or member form this stage does not compile yet */
export class Unsupported extends Error {
  constructor(form: string) {
    super(`qeval: unsupported ${form}`);
  }
}

/** the runtime context a compiled expression reads through */
interface OpCx {
  /** read a query's value (recording it as a dependency) */
  query(key: string): unknown;
  /** resolve a reference to the value it denotes (§7.4) */
  deref(ref: Value): Value;
  /** does a value inhabit a type? (§4.11 match arm selection) */
  memberOf(v: Value, rt: RT): boolean;
  /** a value's string form for template interpolation (§4.8) */
  toStr(v: Value): string;
  /** resolve a unit symbol to its dimension key and base factor (§3.16) */
  unitInfo(sym: string): { key: string; toBase: number };
  /** quantity-aware arithmetic and comparison (§4.6) */
  qArith(op: string, l: Value, r: Value): Value;
  /** apply a closure, native, or std function to evaluated arguments (§4.9, §13) */
  call(fn: Value, args: Value[]): Value;
}
/** local bindings in scope (comprehension loop variables) */
type Locals = Map<string, Value>;
const NO_LOCALS: Locals = new Map();
/** the scope object the value layer's `bind`/`serialize` expect */
type BindScope = {
  inst: RecInst | null;
  locals: Map<string, unknown>;
  rootName: string;
  menv: Env;
};
/** a compiled expression: a closure over the query context and local bindings */
type Op = (cx: OpCx, locals: Locals) => Value;
/** resolve a name to the query key it reads, or undefined if it is not in scope */
type Names = (name: string) => string | undefined;
/** the lexical context a compile happens in */
interface CCtx {
  names: Names;
  self: RecInst | null;
  rootName: string;
  locals: Set<string>; // comprehension loop variables in scope
  resolveType: (t: TypeAst) => RT; // resolve a match arm's type annotation
  menv: Env; // the module environment (funcs, consts, unit table)
}

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

/** the binary operators applyBin evaluates directly (others compile specially) */
const VALUE_OPS = new Set([
  '==',
  '!=',
  '+',
  '-',
  '*',
  '/',
  '%',
  '<',
  '<=',
  '>',
  '>=',
  '&',
  '|',
  '^',
  '<<',
  '>>',
]);

/** the operators quantity operands take through qArith (§4.6) */
const QOPS = new Set(['+', '-', '*', '/', '<', '<=', '>', '>=']);

/** read member `name` of a record value through the query graph (§4.10, §7.5) */
function access(cx: OpCx, x: Value, name: string): Value {
  if (isRec(x)) {
    const s = x.slots.get(name);
    if (s) {
      // an absent optional or a failed member yields ABSENT (§4.10); a live slot
      // is forced through the query graph (memoized)
      if (s.state === 'absent' || s.state === 'invalid') return ABSENT;
      return cx.query(`slot:${pathStr(x.path)}.${name}`) as Value;
    }
    if (x.extras.has(name)) throw new EvalErr(`opaque field ${name} accessed`);
    return ABSENT; // a member admitted only through an optional bound is absent
  }
  if (x === null) throw new EvalErr('member access on null');
  if (x === ABSENT) return ABSENT;
  throw new Unsupported('member access on a non-record value');
}

/** the elements of an iterable (an array or a range) */
function iterate(v: Value): Value[] {
  if (isArr(v)) return v.items;
  if (isRange(v)) {
    const out: Value[] = [];
    for (let i = v.lo; i < v.hi + (v.excl ? 0n : 1n); i++) out.push(i);
    return out;
  }
  throw new EvalErr('not iterable');
}

// ---- compile: an expression AST becomes a closure over the query context ----

/**
 * Compile a call's callee (§4.9): a member chain over `std` builds a std path
 * (`std.math.min`) rather than a data access; anything else compiles normally
 * (a name may resolve to a module function's closure).
 */
function compileCallee(e: Expr, c: CCtx): Op {
  if (e.e === 'member') {
    const nm = e.name;
    const xOp = compileCallee(e.x, c);
    return (cx, l) => {
      const x = xOp(cx, l) as { __std?: true; path?: string[] };
      if (x && x.__std) return { __std: true, path: [...(x.path ?? []), nm] };
      return access(cx, isRef(x) ? cx.deref(x) : x, nm);
    };
  }
  return compile(e, c);
}

function compile(e: Expr, c: CCtx): Op {
  switch (e.e) {
    case 'lit': {
      const v = e.v;
      return () => v;
    }
    case 'unitlit': {
      const num = e.num;
      const unit = e.unit;
      return (cx) => {
        const u = cx.unitInfo(unit);
        return { __q: true, dim: u.key, value: num * u.toBase };
      };
    }
    case 'paren':
      return compile(e.x, c);
    case 'un': {
      const op = e.op;
      const xc = compile(e.x, c);
      return (cx, l) => applyUn(op, xc(cx, l));
    }
    case 'if': {
      const cc = compile(e.c, c);
      const t = compile(e.t, c);
      const f = compile(e.f, c);
      return (cx, l) => (truthy(cc(cx, l)) ? t(cx, l) : f(cx, l));
    }
    case 'bin': {
      const op = e.op;
      if (op === '|>') {
        // first-argument insertion (§4.9): `l |> f(a)` is `f(l, a)`
        const call: Expr =
          e.r.e === 'call'
            ? { e: 'call', fn: e.r.fn, args: [e.l, ...e.r.args] }
            : { e: 'call', fn: e.r, args: [e.l] };
        return compile(call, c);
      }
      const lc = compile(e.l, c);
      const rc = compile(e.r, c);
      if (op === '&&') return (cx, l) => (truthy(lc(cx, l)) ? truthy(rc(cx, l)) : false);
      if (op === '||') return (cx, l) => (truthy(lc(cx, l)) ? true : truthy(rc(cx, l)));
      if (op === '??')
        return (cx, l) => {
          const v = lc(cx, l);
          return v === ABSENT || v === null ? rc(cx, l) : v;
        };
      if (op === '..' || op === '..<') {
        const excl = op === '..<';
        return (cx, l) => ({ __range: true, lo: lc(cx, l), hi: rc(cx, l), excl });
      }
      if (op === 'matches')
        return (cx, l) => {
          const s = lc(cx, l);
          const p = rc(cx, l) as { __pat?: true; re: string };
          if (typeof s !== 'string' || !p || !p.__pat)
            throw new EvalErr('matches needs a string and a pattern');
          const bad = patternError(p.re);
          if (bad) throw new EvalErr(`malformed pattern /${p.re}/: ${bad}`, 'E4119');
          return compilePattern(p.re).test(s);
        };
      if (op === 'in')
        return (cx, l) => {
          const x = lc(cx, l);
          let container = rc(cx, l);
          if (isRef(container)) container = cx.deref(container);
          if (isRange(container))
            return x >= container.lo && (container.excl ? x < container.hi : x <= container.hi);
          if (isArr(container)) return container.items.some((y: Value) => valueEq(x, y));
          if (isMap(container)) return container.entries.has(x);
          if (isRec(container)) throw new Unsupported('`in` over a record'); // needs slot forcing
          throw new EvalErr('in: bad container');
        };
      if (!VALUE_OPS.has(op)) throw new Unsupported(`operator ${op}`);
      return (cx, l) => {
        const lv = lc(cx, l);
        const rv = rc(cx, l);
        // quantity operands take dimension-aware arithmetic/comparison (§4.6)
        if ((isQ(lv) || isQ(rv)) && QOPS.has(op)) return cx.qArith(op, lv, rv);
        return applyBin(op, lv, rv);
      };
    }
    case 'name': {
      const nm = e.name;
      if (c.locals.has(nm)) return (_cx, l) => l.get(nm);
      // a member of the enclosing record shadows module names; read it through
      // access so an absent optional yields ABSENT rather than a missing query
      const self = c.self;
      if (self && self.slots.has(nm)) return (cx) => access(cx, self, nm);
      const key = c.names(nm);
      if (key !== undefined) return (cx) => cx.query(key) as Value;
      if (nm === 'std') return () => ({ __std: true, path: [] });
      const fn = c.menv.funcs.get(nm);
      if (fn) {
        // a module function is a closure over the module scope; its body runs
        // through the value layer, which is pure over its parameters and consts
        const params = fn.params.map((p) => p.name);
        const body = fn.body;
        const menv = c.menv;
        const rootName = c.rootName;
        return () => ({
          __clo: true,
          params,
          body,
          scope: { inst: null, locals: new Map(), rootName, menv },
        });
      }
      throw new Unsupported(`name ${e.name}`);
    }
    case 'member': {
      const nm = e.name;
      const safe = e.safe;
      const xc = compile(e.x, c);
      return (cx, l) => {
        let x = xc(cx, l);
        if (safe && (x === null || x === ABSENT)) return ABSENT;
        if (isRef(x)) x = cx.deref(x);
        return access(cx, x, nm);
      };
    }
    case 'index': {
      const xc = compile(e.x, c);
      const ic = compile(e.i, c);
      return (cx, l) => {
        let x = xc(cx, l);
        if (isRef(x)) x = cx.deref(x);
        const i = ic(cx, l);
        if (isArr(x)) {
          const n = Number(i);
          if (n < 0 || n >= x.items.length) throw new EvalErr(`index ${n} out of bounds`, 'E5005');
          return x.items[n];
        }
        if (isMap(x)) return x.entries.has(i) ? x.entries.get(i) : ABSENT;
        if (isRec(x)) return access(cx, x, i as string);
        throw new EvalErr('index on non-collection');
      };
    }
    case 'call': {
      const fnOp = compileCallee(e.fn, c);
      const argOps = e.args.map((a) => compile(a, c));
      return (cx, l) =>
        cx.call(
          fnOp(cx, l),
          argOps.map((op) => op(cx, l)),
        );
    }
    case 'lambda': {
      const params = e.params;
      const body = e.body;
      const menv = c.menv;
      const rootName = c.rootName;
      const self = c.self;
      // a closure over the current locals; its body runs through the value layer
      return (_cx, l) => ({
        __clo: true,
        params,
        body,
        scope: { inst: self, locals: new Map(l), rootName, menv },
      });
    }
    case 'arr': {
      const parts = e.items.map((it) => ({ spread: it.spread, op: compile(it.expr, c) }));
      return (cx, l) => {
        const items: Value[] = [];
        for (const p of parts) {
          const v = p.op(cx, l);
          if (p.spread) for (const x of iterate(isRef(v) ? cx.deref(v) : v)) items.push(x);
          else items.push(v);
        }
        return { __arr: true, items, path: [] };
      };
    }
    case 'comp': {
      const allVars = e.clauses.map((cl) => cl.v);
      const headOp = compile(e.head, { ...c, locals: new Set([...c.locals, ...allVars]) });
      const clauses = e.clauses.map((cl, i) => {
        const prior = new Set([...c.locals, ...allVars.slice(0, i)]);
        const withThis = new Set([...prior, cl.v]);
        return {
          v: cl.v,
          iterOp: compile(cl.iter, { ...c, locals: prior }),
          filterOps: cl.filters.map((f) => compile(f, { ...c, locals: withThis })),
        };
      });
      return (cx, l) => {
        const items: Value[] = [];
        const rec = (i: number, loc: Locals): void => {
          if (i === clauses.length) {
            items.push(headOp(cx, loc));
            return;
          }
          const cl = clauses[i];
          const it = cl.iterOp(cx, loc);
          for (const el of iterate(isRef(it) ? cx.deref(it) : it)) {
            const loc2 = new Map(loc);
            loc2.set(cl.v, el);
            if (cl.filterOps.every((f) => truthy(f(cx, loc2)))) rec(i + 1, loc2);
          }
        };
        rec(0, l);
        return { __arr: true, items, path: [] };
      };
    }
    case 'obj': {
      // an object literal reaches compile only in a map-typed position (a
      // record-typed one is dispatched to bindRecord); build a map value,
      // which the type binder validates against `map<K, V>`
      const parts = e.entries.map((en) => {
        if (en.val.e === 'spread') throw new Unsupported('spread in a map literal');
        return { key: en.key, op: compile(en.val, c) };
      });
      return (cx, l) => {
        const entries = new Map<string, Value>();
        for (const p of parts) entries.set(p.key, p.op(cx, l));
        return { __map: true, entries, path: [] };
      };
    }
    case 'mapcomp': {
      const allVars = e.clauses.map((cl) => cl.v);
      const scope = new Set([...c.locals, ...allVars]);
      const keyOp = compile(e.key, { ...c, locals: scope });
      const valOp = compile(e.val, { ...c, locals: scope });
      const clauses = e.clauses.map((cl, i) => {
        const prior = new Set([...c.locals, ...allVars.slice(0, i)]);
        const withThis = new Set([...prior, cl.v]);
        return {
          v: cl.v,
          iterOp: compile(cl.iter, { ...c, locals: prior }),
          filterOps: cl.filters.map((f) => compile(f, { ...c, locals: withThis })),
        };
      });
      return (cx, l) => {
        const entries = new Map<string, Value>();
        const rec = (i: number, loc: Locals): void => {
          if (i === clauses.length) {
            const k = keyOp(cx, loc);
            if (typeof k !== 'string') throw new EvalErr('map key must be string');
            if (entries.has(k)) throw new EvalErr(`duplicate key ${k}`, 'E5004');
            entries.set(k, valOp(cx, loc));
            return;
          }
          const cl = clauses[i];
          const it = cl.iterOp(cx, loc);
          for (const el of iterate(isRef(it) ? cx.deref(it) : it)) {
            const loc2 = new Map(loc);
            loc2.set(cl.v, el);
            if (cl.filterOps.every((f) => truthy(f(cx, loc2)))) rec(i + 1, loc2);
          }
        };
        rec(0, l);
        return { __map: true, entries, path: [] };
      };
    }
    case 'template': {
      const parts = e.parts.map((p) => (typeof p === 'string' ? p : compile(p, c)));
      return (cx, l) => {
        let s = '';
        for (const p of parts) s += typeof p === 'string' ? p : cx.toStr(p(cx, l));
        return s;
      };
    }
    case 'pattern': {
      const re = e.re;
      return () => ({ __pat: true, re });
    }
    case 'match': {
      const subjOp = compile(e.subject, c);
      const arms = e.arms.map((a) => ({
        v: a.v,
        rt: a.type ? c.resolveType(a.type) : null,
        body: compile(a.body, { ...c, locals: new Set([...c.locals, a.v]) }),
      }));
      return (cx, l) => {
        let subj = subjOp(cx, l);
        if (isRef(subj)) subj = cx.deref(subj);
        let catchAll: (typeof arms)[number] | null = null;
        for (const arm of arms) {
          if (!arm.rt) {
            catchAll = arm;
            continue;
          }
          if (cx.memberOf(subj, arm.rt)) {
            const l2 = new Map(l);
            l2.set(arm.v, subj);
            return arm.body(cx, l2);
          }
        }
        if (catchAll) {
          const l2 = new Map(l);
          l2.set(catchAll.v, subj);
          return catchAll.body(cx, l2);
        }
        throw new EvalErr('match: no arm matched');
      };
    }
    case 'ctx': {
      const nm = e.name;
      const self = c.self;
      const rootName = c.rootName;
      if (nm === '$this')
        return () => {
          if (!self) throw new EvalErr('$this outside a record instance', 'E4090');
          return { __ref: true, segs: self.path };
        };
      if (nm === '$parent')
        return () => {
          if (!self || !self.parent)
            throw new EvalErr('$parent: the evaluation root has no owner', 'E4090');
          return { __ref: true, segs: self.parent.path };
        };
      if (nm === '$root') return () => ({ __ref: true, segs: [rootName] });
      if (nm === '$key')
        return () => {
          if (!self || !self.parent || self.path.length < self.parent.path.length + 2)
            throw new EvalErr('$key: the instance is not a collection element', 'E4090');
          const k = segText(self.path[self.path.length - 1]);
          return typeof k === 'number' ? BigInt(k) : k;
        };
      throw new Unsupported(`ctx ${nm}`);
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

class QEval {
  private readonly env: Env;
  private readonly helper: Engine; // value layer only: type binding + serialization
  private readonly db: Db;
  private readonly cx: OpCx;
  private readonly constOps = new Map<string, Op>();
  // each slot's compute: it reads siblings/consts/references through the context
  private readonly slotJobs = new Map<string, Op>();
  private readonly diagnostics: Diag[] = [];
  private readonly constCtx: CCtx;

  constructor(env: Env) {
    this.env = env;
    this.helper = new Engine(env);
    this.db = new Db({
      eq: valueEq,
      resolve: (key) => {
        const op = key.startsWith('const:')
          ? this.constOp(key.slice(6))
          : key.startsWith('slot:')
            ? this.slotJobs.get(key)
            : undefined;
        // run each compute with our own context (query + deref); the Db still
        // records dependencies, since our `query` calls back into it
        return op ? () => op(this.cx, NO_LOCALS) : undefined;
      },
    });
    const matchScope = { inst: null, locals: new Map<string, unknown>(), rootName: '', menv: env };
    this.cx = {
      query: (k) => this.db.query(k),
      deref: (r) => this.deref(r),
      memberOf: (v, rt) => this.helper.memberOf(v, rt, matchScope),
      toStr: (v) => this.helper.toStr(v),
      unitInfo: (sym) => {
        try {
          return this.env.unitInfo(sym);
        } catch (err) {
          throw new EvalErr((err as Error).message);
        }
      },
      qArith: (op, l, r) => this.helper.qArith(op, l, r),
      // the value layer may return a lazy prevalue (e.g. a fold accumulating an
      // array by spread); materialize it once at the boundary so the query
      // engine's own operators see a concrete value (§9.4)
      call: (fn, args) => this.helper.matVal(this.helper.call(fn, args, matchScope)),
    };
    this.constCtx = {
      names: (n) => (env.consts.has(n) ? `const:${n}` : undefined),
      self: null,
      rootName: '',
      locals: new Set(),
      resolveType: (t) => env.resolve(t),
      menv: env,
    };
  }

  /**
   * The compute for a module const, compiled on first demand and cached. Lazy
   * compilation matters: a const that names an input or a form this stage does
   * not yet handle stays uncompiled (and does not raise Unsupported) unless
   * something actually reads it — matching the value layer, which forces consts
   * on demand.
   */
  private constOp(name: string): Op | undefined {
    const cached = this.constOps.get(name);
    if (cached) return cached;
    const con = this.env.consts.get(name);
    if (!con) return undefined;
    const op = compile(con.expr, this.constCtx);
    this.constOps.set(name, op);
    return op;
  }

  /** the value a reference denotes: walk its path from the root (§7.4, §7.5) */
  private deref(ref: Value): Value {
    const segs = (ref as { segs: Seg[] }).segs;
    const target = this.resolveSegs(segs);
    if (target === undefined) throw new EvalErr(`dangling reference ${pathStr(segs)}`, 'E6002');
    return target;
  }

  private resolveSegs(segs: Seg[]): Value {
    let cur: Value = this.env.roots.get(segs[0] as string);
    for (let i = 1; i < segs.length && cur !== undefined; i++) {
      const s = segText(segs[i]);
      if (isRec(cur)) {
        const slot = cur.slots.get(s as string);
        cur =
          slot && slot.state !== 'absent' && slot.state !== 'invalid'
            ? this.cx.query(`slot:${pathStr(cur.path)}.${s}`)
            : undefined;
      } else if (isArr(cur)) cur = cur.items[s as number];
      else if (isMap(cur)) cur = cur.entries.get(s);
      else cur = undefined;
      if (cur === ABSENT) cur = undefined;
      if (isRef(cur)) cur = this.deref(cur);
    }
    return cur;
  }

  /**
   * Bind an object literal to a record type: a RecInst whose members are slot
   * queries. A record-typed member binds recursively; a scalar/array member
   * compiles and type-binds its value. Slots are registered but not yet forced —
   * `materialize` fills them for serialization.
   */
  private bindRecord(
    entries: { key: string; val: Expr }[],
    rt: RT,
    path: Seg[],
    parent: RecInst | null,
  ): RecInst {
    const supplied = new Map(entries.map((e) => [e.key, e.val]));
    const instId = pathStr(path);
    const inst: RecInst = {
      __rec: true,
      typeName: rt.name,
      rt,
      path,
      parent,
      slots: new Map(),
      entryOrder: entries.map((e) => e.key),
      extras: new Map(),
    };
    (inst as unknown as { menv: Env }).menv = this.env;
    (inst as unknown as { eng: Engine }).eng = this.helper;
    this.env.registry.push(inst);

    const cctx: CCtx = {
      names: (n) => (this.env.consts.has(n) ? `const:${n}` : undefined),
      self: inst,
      rootName: path[0] as string,
      locals: new Set(),
      resolveType: (t) => this.env.resolve(t),
      menv: this.env,
    };
    const sc = {
      inst,
      locals: new Map<string, unknown>(),
      rootName: path[0] as string,
      menv: this.env,
    };

    // context declarations (§7.3) and open-record tails come in later stages; a
    // record that uses them skips for now. Assertions and when-blocks (§6) are
    // validated after the universe is forced (see run → validateAll).
    if (rt.ctxDecls && rt.ctxDecls.length) throw new Unsupported('context declarations');
    if (rt.open) throw new Unsupported('open record');

    for (const m of rt.members) {
      const memberPath = [...path, m.name];
      const key = `slot:${instId}.${m.name}`;
      const has = supplied.has(m.name);
      const types: RT[] = m.conj ?? (m.type ? [m.type] : []);
      // a member declared `ref<T>` holds a navigation (§7.4) — a later stage
      if (m.type && m.type.t === 'ref') throw new Unsupported('ref-typed member');

      if (m.kind === 'der') {
        if (has && m.hidden) {
          // a hidden member is never part of the value: supplying it is an error
          this.diagnostics.push({
            severity: 'error',
            message: `hidden member ${m.name} supplied`,
            path: pathStr(memberPath),
            code: 'E4006',
          });
          inst.slots.set(m.name, { kind: 'der', hidden: true, state: 'invalid', deferred: false });
          continue;
        }
        const memberRt: RT | undefined = m.type;
        const recBearing = memberRt ? this.hasRec(memberRt, new Set()) : false;
        // a restated derived record/collection is a later stage
        if (has && recBearing) throw new Unsupported('restated derived record');
        const valueOp: Op = memberRt
          ? this.bindValue(m.expr, memberRt, memberPath, inst, cctx, sc)
          : compile(m.expr, cctx);
        const restateOp = has ? compile(supplied.get(m.name)!, cctx) : undefined;
        const produce: Op = (cx, l) => {
          const v = valueOp(cx, l);
          if (restateOp) {
            // a derived member also supplied must be restated identically (§5.4)
            const rawR = restateOp(cx, l);
            const restated = memberRt
              ? this.helper.bind(rawR, memberRt, memberPath, inst, sc)
              : rawR;
            if (!valueEq(v, restated))
              throw new EvalErr(
                `derived member ${m.name} restated with a differing value`,
                'E4005',
              );
          }
          return v;
        };
        this.slotJobs.set(key, produce);
        inst.slots.set(m.name, {
          kind: 'der',
          hidden: m.hidden || undefined,
          state: 'unforced',
          deferred: false,
        });
        continue;
      }

      if (has) {
        this.slotJobs.set(
          key,
          this.supplyProduce(supplied.get(m.name)!, types, memberPath, inst, cctx, sc),
        );
        inst.slots.set(m.name, {
          kind: m.kind,
          hidden: m.hidden || undefined,
          state: 'unforced',
          deferred: false,
        });
      } else if (m.kind === 'dflt') {
        this.slotJobs.set(key, this.supplyProduce(m.dflt, types, memberPath, inst, cctx, sc));
        inst.slots.set(m.name, { kind: 'dflt', state: 'unforced', deferred: false });
      } else if (m.kind === 'opt') {
        inst.slots.set(m.name, { kind: 'opt', state: 'absent', deferred: false });
      } else {
        inst.slots.set(m.name, { kind: 'req', state: 'invalid', deferred: false });
        this.diagnostics.push({
          severity: 'error',
          message: `required member ${m.name} missing`,
          path: pathStr(memberPath),
          code: 'E4002',
        });
      }
    }

    // supplied keys not declared by the type: an error on a closed record (§3.9)
    for (const k of supplied.keys()) {
      if (rt.members.some((m: { name: string }) => m.name === k)) continue;
      this.diagnostics.push({
        severity: 'error',
        message: `undeclared member ${k} on closed record${rt.name ? ' ' + rt.name : ''}`,
        path: pathStr([...path, k]),
        code: 'E4003',
      });
    }
    return inst;
  }

  /**
   * Build the compute for a supplied or default member value. A single type is
   * bound through {@link bindValue} (which keeps records — nested, or inside
   * arrays and maps — in the query graph); conjunction types (§3.11) bind
   * through each conjunct in turn through the value layer.
   */
  private supplyProduce(
    valExpr: Expr,
    types: RT[],
    memberPath: Seg[],
    inst: RecInst,
    cctx: CCtx,
    sc: BindScope,
  ): Op {
    if (types.length === 1) return this.bindValue(valExpr, types[0], memberPath, inst, cctx, sc);
    const op = compile(valExpr, cctx);
    return (cx, l) => {
      const raw = op(cx, l);
      let v: Value = raw;
      for (const ty of types) v = this.helper.bind(raw, ty, memberPath, inst, sc);
      return types.length ? v : raw;
    };
  }

  /** does a type contain a record anywhere (so its values live in the query graph)? */
  private hasRec(t: RT, seen: Set<RT>): boolean {
    if (seen.has(t)) return false;
    seen.add(t);
    switch (t.t) {
      case 'rec':
        return true;
      case 'arr':
        return this.hasRec(t.elem, seen);
      case 'map':
        return this.hasRec(t.val, seen);
      case 'union':
        return t.arms.some((a: RT) => this.hasRec(a, seen));
      default:
        return false;
    }
  }

  /**
   * Bind a value expression to a type, keeping records in the query graph. A
   * record literal recurses into {@link bindRecord}; an array or map whose
   * elements contain records builds each element at its own path (so a nested
   * record's slots are query jobs, reachable by access and navigation); anything
   * else (scalars, scalar collections, literal unions) compiles and type-binds
   * through the value layer. Record-bearing unions and non-literal record-bearing
   * collections are later stages.
   */
  private bindValue(
    valExpr: Expr,
    type: RT,
    path: Seg[],
    parent: RecInst | null,
    cctx: CCtx,
    sc: BindScope,
  ): Op {
    if (type.t === 'rec') {
      if (valExpr.e !== 'obj') throw new Unsupported('non-literal record value');
      const entries = valExpr.entries;
      return () => this.bindRecord(entries, type, path, parent);
    }
    if (type.t === 'union' && this.hasRec(type, new Set()))
      throw new Unsupported('union with records');
    if (type.t === 'arr' && this.hasRec(type.elem, new Set())) {
      if (valExpr.e !== 'arr') throw new Unsupported('record array not a literal');
      if (valExpr.items.some((it) => it.spread)) throw new Unsupported('spread in a record array');
      const elem = type.elem;
      const lo = type.lo;
      const hi = type.hi;
      const elemOps = valExpr.items.map((it, i) =>
        this.bindValue(it.expr, elem, [...path, i], parent, cctx, sc),
      );
      return (cx, l) => {
        const items = elemOps.map((op) => op(cx, l));
        if (lo !== undefined && (items.length < lo || items.length > hi))
          throw new EvalErr(`array size ${items.length} outside ${lo}..${hi}`);
        return { __arr: true, items, path };
      };
    }
    if (type.t === 'map' && this.hasRec(type.val, new Set())) {
      if (valExpr.e !== 'obj') throw new Unsupported('record map not a literal');
      const val = type.val;
      const entryOps = valExpr.entries.map((en) => {
        if (en.val.e === 'spread') throw new Unsupported('spread in a record map');
        return {
          key: en.key,
          op: this.bindValue(en.val, val, [...path, mapKey(en.key)], parent, cctx, sc),
        };
      });
      return (cx, l) => {
        const entries = new Map<string, Value>();
        for (const e of entryOps) entries.set(e.key, e.op(cx, l));
        return { __map: true, entries, path };
      };
    }
    const op = compile(valExpr, cctx);
    return (cx, l) => this.helper.bind(op(cx, l), type, path, parent, sc);
  }

  /** force every slot into the RecInst tree so serialization can read it */
  private materialize(inst: RecInst, seen: Set<RecInst>): void {
    if (seen.has(inst)) return;
    seen.add(inst);
    const id = pathStr(inst.path);
    for (const m of inst.rt.members) {
      const s = inst.slots.get(m.name);
      if (!s || s.state !== 'unforced') continue; // absent/invalid slots have no compute
      try {
        const v = this.db.query(`slot:${id}.${m.name}`) as Value;
        s.value = v;
        s.state = 'ok';
        this.materializeValue(v, seen);
      } catch (err) {
        if (err instanceof EvalErr) {
          s.state = 'invalid';
          this.diagnostics.push({
            severity: 'error',
            message: err.message,
            path: pathStr([...inst.path, m.name]),
            code: (err as { code?: string }).code,
          });
        } else throw err;
      }
    }
  }

  private materializeValue(v: Value, seen: Set<RecInst>): void {
    if (isRec(v)) this.materialize(v, seen);
    else if (isArr(v)) for (const x of v.items) this.materializeValue(x, seen);
    else if (isMap(v)) for (const x of v.entries.values()) this.materializeValue(x, seen);
  }

  run(): QReport {
    // bind every root first, then force the universe (as the source pipeline
    // does), so a reference from one root into another resolves (§9.3)
    const built: { name: string; v: Value }[] = [];
    for (const o of this.env.outputs) {
      try {
        const rt = this.env.resolve(o.type);
        const cctx: CCtx = {
          names: (n) => (this.env.consts.has(n) ? `const:${n}` : undefined),
          self: null,
          rootName: o.name,
          locals: new Set(),
          resolveType: (t) => this.env.resolve(t),
          menv: this.env,
        };
        const sc: BindScope = { inst: null, locals: new Map(), rootName: o.name, menv: this.env };
        const v = this.bindValue(o.expr, rt, [o.name], null, cctx, sc)(this.cx, NO_LOCALS);
        this.env.roots.set(o.name, v);
        built.push({ name: o.name, v });
      } catch (err) {
        if (err instanceof EvalErr)
          this.diagnostics.push({
            severity: 'error',
            message: err.message,
            path: o.name,
            code: (err as { code?: string }).code,
          });
        else throw err; // Unsupported (or a real bug) bubbles to the caller
      }
    }
    const outputs: { name: string; json: string }[] = [];
    for (const { name, v } of built) {
      try {
        this.materializeValue(v, new Set());
        outputs.push({ name, json: this.helper.serialize(v, name) });
      } catch (err) {
        if (err instanceof EvalErr)
          this.diagnostics.push({
            severity: 'error',
            message: err.message,
            path: name,
            code: (err as { code?: string }).code,
          });
        else throw err;
      }
    }
    // validate the forced universe (§6): every instance's assertions and
    // when-blocks, read from the materialized slots by the value layer
    this.helper.validateAll('');
    // a single error anywhere suppresses every output (§9.3, as evaluateSource);
    // assertion diagnostics land on the shared env, the rest on our own list
    const diagnostics = [...this.diagnostics, ...this.env.diagnostics];
    const ok = !diagnostics.some((d) => d.severity === 'error');
    return { ok, outputs: ok ? outputs : [], diagnostics };
  }
}

export function qevaluate(env: Env): QReport {
  return new QEval(env).run();
}
