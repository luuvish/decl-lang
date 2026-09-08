// The decl layer of the query engine (qengine/DESIGN.md). Compile expressions to
// query computes and evaluate a module's outputs through the incremental core
// (db.ts). Covered so far:
//   scalars/const, records (slots as queries, cross-slot & nested & member
//   access), references ($this/$parent/$root/$key, deref/navigation),
//   arrays & array comprehensions, ranges, indexing, maps & map comprehensions,
//   `in`/`matches`, patterns, string templates, and `match` over literal unions.
// Value semantics — equality, serialization, type binding, iteration, type
// membership — are reused from the current value layer, as the design intends;
// only the evaluation strategy is new. Quantities/units, std calls, `ref<T>`
// navigation members, context declarations, unions of records in collection
// positions, `$referrers`, and the rest come in later stages.
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
}
/** local bindings in scope (comprehension loop variables) */
type Locals = Map<string, Value>;
const NO_LOCALS: Locals = new Map();
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
  if (isQ(l) || isQ(r)) throw new Unsupported('quantity arithmetic'); // later stage
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

/** read member `name` of a record value through the query graph (§4.10, §7.5) */
function access(cx: OpCx, x: Value, name: string): Value {
  if (isRec(x)) {
    if (x.slots.has(name)) return cx.query(`slot:${pathStr(x.path)}.${name}`) as Value;
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

function compile(e: Expr, c: CCtx): Op {
  switch (e.e) {
    case 'lit': {
      const v = e.v;
      return () => v;
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
      return (cx, l) => applyBin(op, lc(cx, l), rc(cx, l));
    }
    case 'name': {
      if (c.locals.has(e.name)) {
        const n = e.name;
        return (_cx, l) => l.get(n);
      }
      const key = c.names(e.name);
      if (key === undefined) throw new Unsupported(`name ${e.name}`);
      return (cx) => cx.query(key) as Value;
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

  constructor(env: Env) {
    this.env = env;
    this.helper = new Engine(env);
    this.db = new Db({
      eq: valueEq,
      resolve: (key) => {
        const op = key.startsWith('const:')
          ? this.constOps.get(key.slice(6))
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
    };
    const constCtx: CCtx = {
      names: (n) => (env.consts.has(n) ? `const:${n}` : undefined),
      self: null,
      rootName: '',
      locals: new Set(),
      resolveType: (t) => env.resolve(t),
    };
    for (const [name, con] of env.consts) this.constOps.set(name, compile(con.expr, constCtx));
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
      if (isRec(cur))
        cur = cur.slots.has(s as string)
          ? this.cx.query(`slot:${pathStr(cur.path)}.${s}`)
          : undefined;
      else if (isArr(cur)) cur = cur.items[s as number];
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

    const memberNames = new Set<string>(rt.members.map((m: { name: string }) => m.name));
    const cctx: CCtx = {
      names: (n) =>
        memberNames.has(n)
          ? `slot:${instId}.${n}`
          : this.env.consts.has(n)
            ? `const:${n}`
            : undefined,
      self: inst,
      rootName: path[0] as string,
      locals: new Set(),
      resolveType: (t) => this.env.resolve(t),
    };
    const sc = {
      inst,
      locals: new Map<string, unknown>(),
      rootName: path[0] as string,
      menv: this.env,
    };

    for (const m of rt.members) {
      if (m.kind === 'opt' || m.kind === 'dflt') throw new Unsupported(`member kind ${m.kind}`);
      if (m.type && m.type.t === 'ref') throw new Unsupported('ref-typed member');
      const memberPath = [...path, m.name];
      const valExpr: Expr | undefined = m.kind === 'der' ? m.expr : supplied.get(m.name);
      if (valExpr === undefined) throw new Unsupported(`missing member ${m.name}`);
      let produce: Op;
      if (m.type && m.type.t === 'rec') {
        if (valExpr.e !== 'obj') throw new Unsupported('record member not an object literal');
        const objEntries = valExpr.entries;
        const memberRt = m.type;
        produce = () => this.bindRecord(objEntries, memberRt, memberPath, inst);
      } else {
        const op = compile(valExpr, cctx);
        const memberRt = m.type;
        produce = (cx, l) => {
          const v = op(cx, l);
          return memberRt ? this.helper.bind(v, memberRt, memberPath, inst, sc) : v;
        };
      }
      this.slotJobs.set(`slot:${instId}.${m.name}`, produce);
      inst.slots.set(m.name, {
        kind: m.kind,
        hidden: m.hidden || undefined,
        state: 'unforced',
        deferred: false,
      });
    }
    return inst;
  }

  /** force every slot into the RecInst tree so serialization can read it */
  private materialize(inst: RecInst, seen: Set<RecInst>): void {
    if (seen.has(inst)) return;
    seen.add(inst);
    const id = pathStr(inst.path);
    for (const m of inst.rt.members) {
      const s = inst.slots.get(m.name);
      if (!s) continue;
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
    const outputs: { name: string; json: string }[] = [];
    let ok = true;
    for (const o of this.env.outputs) {
      try {
        const rt = this.env.resolve(o.type);
        let v: Value;
        if (rt.t === 'rec' && o.expr.e === 'obj') {
          v = this.bindRecord(o.expr.entries, rt, [o.name], null);
          this.env.roots.set(o.name, v); // set before materialize so references resolve
          this.materialize(v, new Set());
        } else {
          const cctx: CCtx = {
            names: (n) => (this.env.consts.has(n) ? `const:${n}` : undefined),
            self: null,
            rootName: o.name,
            locals: new Set(),
            resolveType: (t) => this.env.resolve(t),
          };
          const raw = compile(o.expr, cctx)(this.cx, NO_LOCALS);
          const sc = {
            inst: null,
            locals: new Map<string, unknown>(),
            rootName: o.name,
            menv: this.env,
          };
          v = this.helper.bind(raw, rt, [o.name], null, sc);
          this.env.roots.set(o.name, v);
        }
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
