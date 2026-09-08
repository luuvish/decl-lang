// The decl layer of the query engine (qengine/DESIGN.md). Compile expressions to
// query computes and evaluate a module's outputs through the incremental core
// (db.ts). Covered so far:
//   scalars/const, records (slots as queries, cross-slot & nested & member
//   access; required/optional/default/derived/hidden members; restate and
//   missing/undeclared diagnostics; records nested in arrays and maps),
//   references ($this/$parent/$root/$key/$path, deref/navigation, ref<T>
//   navigation members with the mirror rule), references to other roots,
//   arrays & array comprehensions, ranges, indexing, maps & map comprehensions,
//   `in`/`matches`, patterns, string templates, `match`, unions of records,
//   quantities and dimensional arithmetic, std/module-function calls, lambdas,
//   the pipe, object spread, `with`, inputs (fallback on demand), open records,
//   assertions/when-blocks, `$referrers` evaluated in rounds (§7.6), and
//   multi-module universes (§8: imports, namespaces, cross-module roots).
// This covers the whole valid corpus byte-for-byte against the current engine.
// Value semantics — equality, serialization, type binding, iteration, type
// membership, unit resolution, quantity arithmetic, function application,
// assertions — are reused from the current value layer, as the design intends;
// only the evaluation strategy is new. A record value the query graph does not
// build itself (a call result, a `with`, a discriminated union, a
// comprehension) is bound and forced through the value layer and read back
// uniformly by forceMember; assertions run over the forced universe, and a
// value-layer member that reads $referrers is completed in a settle phase once
// the universe is fully built (run). The universe is evaluated in rounds
// (qevaluate), each answering $referrers from the previous round's frozen
// universe, until the queried edges stabilize. (A closure passed to a std
// function has its body run by the value layer for now — pure over its
// parameters and consts, so byte-identical; closures compile into the query
// graph in a later stage.)
import { Db, QueryCycle } from './db.ts';
import { Engine } from '../engine.ts';
import {
  ABSENT,
  EvalErr,
  Taint,
  cmpPath,
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
  sortDiags,
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
  /** read a record member: a query-graph slot through the db, a value-layer one
   * through the reference engine (§4.10) */
  forceMember(rec: Value, name: string): Value;
  /** the value of another evaluation root (another output), or undefined */
  rootValue(name: string): Value | undefined;
  /** the value of an input root, binding its fallback on first demand (§5.6) */
  inputRoot(name: string): Value;
  /** the entries an object-valued expression contributes to a spread (§4.2) */
  spreadEntries(v: Value): [string, Value][];
  /** read a member of a non-record value (a bare map, an unbound literal, …) */
  accessRaw(x: Value, name: string): Value;
  /** materialize a value: deref a reference, flatten a lazy prevalue once (§9.4) */
  matVal(v: Value): Value;
  /** evaluate an expression through the value layer in a given scope, then
   * materialize it — for forms the query graph delegates (e.g. `with`) */
  evValue(expr: Expr, scope: BindScope): Value;
  /** the references pointing at `self` through a type's member (§7.6), answered
   * from the previous round's frozen universe */
  referrers(typeName: string, member: string, self: RecInst): Value;
  /** a module-scoped name (a const/func/import/namespace) resolved by the value
   * layer — used for imported names and non-entry modules (§8) */
  moduleValue(menv: Env, name: string): Value;
  /** read an export of a namespace value (§8.5) */
  nsValue(ns: Value, name: string): Value;
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
/** an evaluation root: a module output (or bound input), with its module scope */
interface RootSpec {
  name: string;
  expr: Expr;
  type: TypeAst;
  menv: Env;
}
/** a compiled expression: a closure over the query context and local bindings */
type Op = (cx: OpCx, locals: Locals) => Value;
/** the lexical context a compile happens in */
interface CCtx {
  self: RecInst | null;
  rootName: string;
  locals: Set<string>; // comprehension loop variables in scope
  resolveType: (t: TypeAst) => RT; // resolve a match arm's type annotation
  menv: Env; // the module environment (funcs, consts, unit table)
  entryEnv: Env; // the universe's entry module (its consts are query-native)
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

// ---- $referrers: the universe is evaluated in rounds until its edges settle ----

/** the maximum rounds a universe may take to stabilize before E5009 (§7.6) */
const ROUNDS = 8;
/** one `$referrers(Type, member)` edge: each referring instance and its refs */
type Edge = Map<string, { path: Seg[]; refs: Seg[][] }>;
/** a frozen round: the universe a later round answers `$referrers` against */
interface Frozen {
  registry: RecInst[];
  edges: Map<string, Edge>;
}

/** the trailing member name of a slot key (`slot:hub.ports["a"].sel$` → `sel$`) */
function memberOfSlotKey(key: string): string {
  const m = /\.([A-Za-z_$][\w$]*)$|\["([^"]*)"\]$/.exec(key);
  return m ? (m[1] ?? m[2]) : key;
}

/** every reference an object value holds, recursively (arrays and maps included) */
function refsIn(v: Value, out: Seg[][]): void {
  if (isRef(v)) out.push(v.segs);
  else if (isArr(v)) for (const x of v.items) refsIn(x, out);
  else if (isMap(v)) for (const x of v.entries.values()) refsIn(x, out);
}
/** a stable string for a set of refs (order-insensitive), for edge comparison */
function refsKey(refs: Seg[][]): string {
  return refs
    .map((r) => pathStr(r))
    .sort()
    .join('\n');
}
/** whether two edges hold the same referrers with the same refs */
function edgeEq(a: Edge, b: Edge): boolean {
  if (a.size !== b.size) return false;
  for (const [k, e] of a) {
    const f = b.get(k);
    if (!f || refsKey(e.refs) !== refsKey(f.refs)) return false;
  }
  return true;
}
/** the path of the first candidate whose refs differ between two edges (§6.7) */
function edgeDiff(a: Edge, b: Edge): string {
  const paths = [...new Set([...a.keys(), ...b.keys()])]
    .map((k) => (a.get(k) ?? b.get(k))!.path)
    .sort(cmpPath);
  for (const p of paths) {
    const k = pathStr(p);
    const e = a.get(k);
    const f = b.get(k);
    if (!e || !f || refsKey(e.refs) !== refsKey(f.refs)) return k;
  }
  return '';
}
/** a stable string for a whole round's edges, for cycle detection */
function edgesKey(edges: Map<string, Edge>): string {
  return [...edges.keys()]
    .sort()
    .map((k) => {
      const e = edges.get(k)!;
      return `${k}=${[...e.keys()]
        .sort()
        .map((pk) => `${pk}:${refsKey(e.get(pk)!.refs)}`)
        .join(';')}`;
    })
    .join('|');
}

/** read member `name` of a value (§4.10, §7.5) */
function access(cx: OpCx, x: Value, name: string): Value {
  // a record is read through the query graph; everything else (a bare map, an
  // unbound literal from the value layer such as std.map.entries records, null,
  // absent) is read by the value layer
  if (isRec(x)) return cx.forceMember(x, name);
  return cx.accessRaw(x, name);
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
      const x = xOp(cx, l) as { __std?: true; __nsref?: true; path?: string[] };
      if (x && x.__std) return { __std: true, path: [...(x.path ?? []), nm] };
      if (x && x.__nsref) return cx.nsValue(x, nm); // a namespace export (a func/value)
      return access(cx, isRef(x) ? cx.deref(x) : x, nm);
    };
  }
  return compile(e, c);
}

/** a place-navigation marker: the location a chain names even past absent steps */
interface Segs {
  __segs: Seg[];
}
const isSegs = (v: unknown): v is Segs => typeof v === 'object' && v !== null && '__segs' in v;

/**
 * Navigate an expression as a place (§7.4): a step past a missing member, key,
 * or index still names the location (accumulated in `__segs`); a present step
 * yields the value there. Mirrors the reference's evNav.
 */
function compileNav(e: Expr, c: CCtx): Op {
  if (e.e === 'if') {
    const cc = compile(e.c, c);
    const t = compileNav(e.t, c);
    const f = compileNav(e.f, c);
    return (cx, l) => (truthy(cc(cx, l)) ? t(cx, l) : f(cx, l));
  }
  if (e.e === 'paren') return compileNav(e.x, c);
  if (e.e === 'member') {
    const nm = e.name;
    const xOp = compileNav(e.x, c);
    return (cx, l) => {
      const x0 = xOp(cx, l);
      if (isSegs(x0)) return { __segs: [...x0.__segs, nm] };
      const x = isRef(x0) ? cx.deref(x0) : x0;
      const v = access(cx, x, nm);
      if (v === ABSENT && isRec(x)) return { __segs: [...x.path, nm] };
      return v;
    };
  }
  if (e.e === 'index') {
    const xOp = compileNav(e.x, c);
    const iOp = compile(e.i, c);
    return (cx, l) => {
      const x0 = xOp(cx, l);
      const i = iOp(cx, l);
      const seg: Seg = typeof i === 'bigint' ? Number(i) : mapKey(i);
      if (isSegs(x0)) return { __segs: [...x0.__segs, seg] };
      const x = isRef(x0) ? cx.deref(x0) : x0;
      if (isArr(x)) return x.items[Number(i)] ?? { __segs: [...x.path, Number(i)] };
      if (isMap(x)) return x.entries.has(i) ? x.entries.get(i) : { __segs: [...x.path, mapKey(i)] };
      if (isRec(x)) {
        const v = access(cx, x, i);
        return v === ABSENT ? { __segs: [...x.path, i] } : v;
      }
      return x;
    };
  }
  return compile(e, c);
}

/** the path segments a ref-position expression denotes, or null if not a place */
function compilePlace(e: Expr, c: CCtx): (cx: OpCx, l: Locals) => Seg[] | null {
  const navOp = compileNav(e, c);
  return (cx, l) => {
    const v = navOp(cx, l);
    if (isSegs(v)) return v.__segs;
    if (isRef(v)) return v.segs as Seg[];
    return isRec(v) || isArr(v) || isMap(v) ? (v.path as Seg[]) : null;
  };
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
          const container = cx.matVal(rc(cx, l));
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
      // a member of the enclosing record — or an ancestor, nearest first (§8
      // scoping, as slotLookup) — shadows module names; read through forceMember
      // so an absent optional yields ABSENT rather than a missing query
      for (let cur = c.self; cur; cur = cur.parent) {
        if (cur.slots.has(nm)) {
          const owner = cur;
          return (cx) => cx.forceMember(owner, nm);
        }
      }
      // a const in the current module scope: the entry module's is query-native
      // (memoized here); another module's is resolved by the value layer (§8)
      if (c.menv.consts.has(nm)) {
        if (c.menv === c.entryEnv) return (cx) => cx.query(`const:${nm}`) as Value;
        const menv = c.menv;
        return (cx) => cx.moduleValue(menv, nm);
      }
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
      // an imported name or a namespace: resolved through the value layer (§8)
      if (c.menv.imports.has(nm) || c.menv.namespaces.has(nm)) {
        const menv = c.menv;
        return (cx) => cx.moduleValue(menv, nm);
      }
      if (c.menv.inputs.has(nm)) return (cx) => cx.inputRoot(nm);
      // otherwise a reference to another evaluation root, resolved at force time
      // (every root is bound before the universe is forced); a form this stage
      // does not resolve stays Unsupported
      return (cx) => {
        const r = cx.rootValue(nm);
        if (r === undefined) throw new Unsupported(`name ${nm}`);
        return r;
      };
    }
    case 'member': {
      const nm = e.name;
      const safe = e.safe;
      const xc = compile(e.x, c);
      return (cx, l) => {
        let x = xc(cx, l);
        if (x && (x as { __nsref?: true }).__nsref) return cx.nsValue(x, nm); // a namespace export
        if (safe && (x === null || x === ABSENT)) return ABSENT;
        if (isRef(x)) x = cx.deref(x);
        return access(cx, x, nm);
      };
    }
    case 'index': {
      const xc = compile(e.x, c);
      const ic = compile(e.i, c);
      return (cx, l) => {
        const x = cx.matVal(xc(cx, l)); // deref + flatten a lazy prevalue (§4.3)
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
    case 'with': {
      // `base with { patch }` (§4.2): merge patch into an object-valued base.
      // Delegated to the value layer, which reads the query-graph base through
      // the compute bridge; the result is a fresh unbound value, materialized.
      const withExpr = e;
      const self = c.self;
      const rootName = c.rootName;
      const menv = c.menv;
      return (cx, l) => cx.evValue(withExpr, { inst: self, locals: l, rootName, menv });
    }
    case 'arr': {
      const parts = e.items.map((it) => ({ spread: it.spread, op: compile(it.expr, c) }));
      return (cx, l) => {
        const items: Value[] = [];
        for (const p of parts) {
          const v = p.op(cx, l);
          if (p.spread) for (const x of iterate(cx.matVal(v))) items.push(x);
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
          for (const el of iterate(cx.matVal(it))) {
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
      // which the type binder validates against `map<K, V>`. A spread entry
      // (§4.2) copies the entries of an object-valued expression in place.
      const parts = e.entries.map((en) =>
        en.val.e === 'spread'
          ? { spread: true as const, op: compile(en.val.expr, c) }
          : { spread: false as const, key: en.key, op: compile(en.val, c) },
      );
      return (cx, l) => {
        const entries = new Map<string, Value>();
        const put = (k: string, v: Value): void => {
          if (entries.has(k)) throw new EvalErr(`duplicate key ${k}`, 'E5004');
          entries.set(k, v);
        };
        for (const p of parts) {
          if (p.spread) {
            let s = p.op(cx, l);
            if (isRef(s)) s = cx.deref(s);
            for (const [k, v] of cx.spreadEntries(s)) put(k, v);
          } else put(p.key, p.op(cx, l));
        }
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
          for (const el of iterate(cx.matVal(it))) {
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
      if (nm === '$path')
        return () => {
          if (!self) throw new EvalErr('$path outside a record instance', 'E4090');
          return pathStr(self.path);
        };
      throw new Unsupported(`ctx ${nm}`);
    }
    case 'referrers': {
      const typeName = e.type;
      const member = e.member;
      const self = c.self;
      return (cx) => {
        if (!self) throw new EvalErr('$referrers outside a record instance', 'E4090');
        return cx.referrers(typeName, member, self);
      };
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
  private readonly prev: Frozen | null; // the previous round's frozen universe
  private readonly queriedEdges = new Set<string>(); // `$referrers` keys asked this round
  private readonly refIndexCache = new Map<string, Map<string, Seg[][]>>();

  constructor(env: Env, prev: Frozen | null = null, mods: { env: Env }[] = []) {
    this.env = env;
    this.prev = prev;
    this.helper = new Engine(env);
    // §4.13 elaboration hooks (a const in a type position, a unit factor) for
    // every module, pointing at this round's value layer — as runUniverse does
    for (const m of mods) {
      m.env.constEval = (n: string) => this.helper.forceConstIn(m.env, n, '');
      m.env.exprEval = (e: Expr) =>
        this.helper.ev(e, { inst: null, locals: new Map(), rootName: '', menv: m.env });
    }
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
      // the value layer may return a lazy prevalue (an unbound record literal, a
      // spread-fold accumulator); it is materialized at the point a query-engine
      // operator consumes it (index/iterate/in) or, in a type position, by the
      // binder — not here, so a record literal keeps its sibling scope (§9.4)
      call: (fn, args) => this.helper.call(fn, args, matchScope),
      matVal: (v) => this.helper.matVal(v),
      forceMember: (rec, name) => this.forceMember(rec, name),
      rootValue: (name) => this.env.roots.get(name),
      inputRoot: (name) => this.demandInputRoot(name),
      spreadEntries: (v) => this.helper.spreadEntries(v),
      accessRaw: (x, name) => this.helper.access(x, name),
      evValue: (expr, scope) => this.helper.matVal(this.helper.ev(expr, scope)),
      referrers: (typeName, member, self) => this.referrers(typeName, member, self),
      moduleValue: (menv, name) => this.helper.matVal(this.helper.moduleValue(menv, name, '')),
      nsValue: (ns, name) => this.helper.matVal(this.helper.nsValue(ns, name, matchScope)),
    };
    this.constCtx = {
      entryEnv: env,
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

  /**
   * Read a record member (§4.10). A query-graph slot is forced through the db
   * (recording a dependency); a value-layer record (built by the value layer for
   * a call, a `with`, or a discriminated union) is forced through the reference
   * engine. An already-forced slot returns its value; an absent optional or a
   * failed member yields ABSENT.
   */
  private forceMember(rec: RecInst, name: string): Value {
    const s = rec.slots.get(name);
    if (!s) {
      if (rec.extras.has(name)) throw new EvalErr(`opaque field ${name} accessed`);
      return ABSENT;
    }
    if (s.state === 'ok') return s.value;
    if (s.state === 'absent' || s.state === 'invalid') return ABSENT;
    if ((rec as unknown as { __qeng?: boolean }).__qeng)
      return this.db.query(`slot:${pathStr(rec.path)}.${name}`) as Value;
    try {
      return this.helper.forceSlot(rec, name);
    } catch {
      return ABSENT; // a tainted (invalid) value-layer member reads as absent
    }
  }

  /**
   * The value of an input root (§5.5, §5.6): with no document supplied, its
   * fallback binds on first demand and is cached in `env.roots`; without a
   * fallback the input is unbound (E5006). Its slots stay lazy — forced on
   * demand through the compute bridge like any query-graph record.
   */
  private demandInputRoot(name: string): Value {
    const cached = this.env.roots.get(name);
    if (cached !== undefined) return cached;
    const decl = this.env.inputs.get(name);
    if (!decl) throw new EvalErr(`unknown name ${name}`);
    if (!decl.fallback) throw new EvalErr(`input ${name} is not bound`, 'E5006');
    const rt = this.env.resolve(decl.type);
    const cctx: CCtx = {
      entryEnv: this.env,
      self: null,
      rootName: name,
      locals: new Set(),
      resolveType: (t) => this.env.resolve(t),
      menv: this.env,
    };
    const sc: BindScope = { inst: null, locals: new Map(), rootName: name, menv: this.env };
    const v = this.bindValue(decl.fallback, rt, [name], null, cctx, sc)(this.cx, NO_LOCALS);
    this.env.roots.set(name, v);
    return v;
  }

  /**
   * `$referrers(Type, member)` for `self` (§7.6): the references that point at
   * `self`, answered from the previous round's frozen universe (empty in round
   * 1). One pass inverts the edge — each referrer filed under every distinct
   * place its member refers to — so the lookup is O(1); the result is sorted by
   * canonical path.
   */
  private referrers(typeName: string, member: string, self: RecInst): Value {
    const key = `${typeName}|${member}`;
    this.queriedEdges.add(key);
    let inv = this.refIndexCache.get(key);
    if (!inv) {
      inv = new Map<string, Seg[][]>();
      const edge = this.edgeOver(this.prev ? this.prev.registry : [], typeName, member);
      for (const e of edge.values()) {
        const seen = new Set<string>();
        for (const r of e.refs) {
          const tk = pathStr(r);
          if (seen.has(tk)) continue;
          seen.add(tk);
          const arr = inv.get(tk);
          if (arr) arr.push(e.path);
          else inv.set(tk, [e.path]);
        }
      }
      this.refIndexCache.set(key, inv);
    }
    const out = (inv.get(pathStr(self.path)) ?? []).slice();
    out.sort(cmpPath);
    return { __arr: true, items: out.map((p) => ({ __ref: true, segs: p })), path: [] };
  }

  /** the edge for (type, member) over a registry: each instance of the type and
   * the references its member holds */
  private edgeOver(registry: RecInst[], typeName: string, member: string): Edge {
    const edge: Edge = new Map();
    for (const cand of registry) {
      if (cand.typeName !== typeName || !cand.slots.has(member)) continue;
      let v: Value;
      try {
        v = this.forceMember(cand, member);
      } catch {
        continue; // a member that could not be forced contributes no ref
      }
      const refs: Seg[][] = [];
      refsIn(v, refs);
      edge.set(pathStr(cand.path), { path: cand.path, refs });
    }
    return edge;
  }

  /** this round's edges for every queried key, over the current registry */
  liveEdges(): Map<string, Edge> {
    const out = new Map<string, Edge>();
    for (const key of this.queriedEdges) {
      const [t, m] = key.split('|');
      out.set(key, this.edgeOver(this.env.registry, t, m));
    }
    return out;
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
        cur = cur.slots.has(s as string) ? this.forceMember(cur, s as string) : undefined;
        if (cur === ABSENT) cur = undefined;
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
    (inst as unknown as { __qeng: boolean }).__qeng = true; // a query-graph record
    this.env.registry.push(inst);

    const cctx: CCtx = {
      entryEnv: this.env,
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

    // Assertions and when-blocks (§6) are validated after the universe is forced
    // (see run → validateAll). Context declarations (§7.3) are a checker
    // obligation (D30), not an evaluator concern — $this/$parent/$root/$key
    // resolve structurally — so they need no handling here. An open record is
    // fine unless a document actually supplies extra members (handled below).

    for (const m of rt.members) {
      const memberPath = [...path, m.name];
      const key = `slot:${instId}.${m.name}`;
      const has = supplied.has(m.name);
      const types: RT[] = m.conj ?? (m.type ? [m.type] : []);

      // a member declared `ref<T>` holds a navigation (§7.4): the expression
      // names a place, checked for reference integrity (§7.5) when it is forced
      if (m.type && m.type.t === 'ref' && !m.conj) {
        const placeExpr: Expr | undefined =
          m.kind === 'der'
            ? m.expr
            : has
              ? supplied.get(m.name)
              : m.kind === 'dflt'
                ? m.dflt
                : undefined;
        if (placeExpr === undefined) {
          if (m.kind === 'opt')
            inst.slots.set(m.name, { kind: 'opt', state: 'absent', deferred: false });
          else {
            inst.slots.set(m.name, { kind: 'req', state: 'invalid', deferred: false });
            this.diagnostics.push({
              severity: 'error',
              message: `required member ${m.name} missing`,
              path: pathStr(memberPath),
              code: 'E4002',
              by: `root:${path[0] as string}`,
            });
          }
          continue;
        }
        const placeOp = compilePlace(placeExpr, cctx);
        this.slotJobs.set(key, (cx, l) => {
          const segs = placeOp(cx, l);
          if (!segs) throw new EvalErr('not a place in ref position');
          if (this.resolveSegs(segs) === undefined)
            throw new EvalErr(`dangling reference ${pathStr(segs)}`, 'E6002');
          return { __ref: true, segs };
        });
        inst.slots.set(m.name, {
          kind: m.kind,
          hidden: m.hidden || undefined,
          state: 'unforced',
          deferred: false,
          compute: () => this.db.query(key),
        });
        continue;
      }

      if (m.kind === 'der') {
        if (has && m.hidden) {
          // a hidden member is never part of the value: supplying it is an error
          this.diagnostics.push({
            severity: 'error',
            message: `hidden member ${m.name} supplied`,
            path: pathStr(memberPath),
            code: 'E4006',
            by: `root:${path[0] as string}`,
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
          compute: () => this.db.query(key),
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
          compute: () => this.db.query(key),
        });
      } else if (m.kind === 'dflt') {
        this.slotJobs.set(key, this.supplyProduce(m.dflt, types, memberPath, inst, cctx, sc));
        inst.slots.set(m.name, {
          kind: 'dflt',
          state: 'unforced',
          deferred: false,
          compute: () => this.db.query(key),
        });
      } else if (m.kind === 'opt') {
        inst.slots.set(m.name, { kind: 'opt', state: 'absent', deferred: false });
      } else {
        inst.slots.set(m.name, { kind: 'req', state: 'invalid', deferred: false });
        this.diagnostics.push({
          severity: 'error',
          message: `required member ${m.name} missing`,
          path: pathStr(memberPath),
          code: 'E4002',
          by: `root:${path[0] as string}`,
        });
      }
    }

    // supplied keys not declared by the type: kept as extras on an open record
    // (their storage/serialization is a later stage), an error on a closed one
    for (const k of supplied.keys()) {
      if (rt.members.some((m: { name: string }) => m.name === k)) continue;
      if (rt.open) throw new Unsupported('open-record extras');
      this.diagnostics.push({
        severity: 'error',
        message: `undeclared member ${k} on closed record${rt.name ? ' ' + rt.name : ''}`,
        path: pathStr([...path, k]),
        code: 'E4003',
        by: `root:${path[0] as string}`,
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
    // a record literal binds in the query graph; a record value from any other
    // expression (a call, a `with`, a reference, a discriminated union) is left
    // to the value layer and forced during materialize
    if (type.t === 'rec' && valExpr.e === 'obj') {
      const entries = valExpr.entries;
      return () => this.bindRecord(entries, type, path, parent);
    }
    // a literal array of record-bearing elements: build each at its own path so
    // a nested record's slots stay query jobs (reachable by access/navigation)
    if (
      type.t === 'arr' &&
      this.hasRec(type.elem, new Set()) &&
      valExpr.e === 'arr' &&
      !valExpr.items.some((it) => it.spread)
    ) {
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
    // a literal map of record-bearing values: likewise, one query record per key
    if (
      type.t === 'map' &&
      this.hasRec(type.val, new Set()) &&
      valExpr.e === 'obj' &&
      !valExpr.entries.some((en) => en.val.e === 'spread')
    ) {
      const val = type.val;
      const entryOps = valExpr.entries.map((en) => ({
        key: en.key,
        op: this.bindValue(en.val, val, [...path, mapKey(en.key)], parent, cctx, sc),
      }));
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
    // a value-layer record (a call/with/union result) forces through the
    // reference engine, which recurses into any query-graph records it holds
    // (their slots delegate to the db)
    if (!(inst as unknown as { __qeng?: boolean }).__qeng) {
      this.helper.forceAll(inst, false);
      return;
    }
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
        const by = `${id}.${m.name}`; // the slot forcing step that produced it (§6.7)
        if (err instanceof EvalErr) {
          s.state = 'invalid';
          const code = (err as { code?: string }).code;
          // the reference tags a diagnostic reported inside the forcing step
          // (a reference or restate failure); a raw evaluation error (E5xxx)
          // propagates and is reported untagged
          const tagged = code === 'E6002' || code === 'E4005';
          this.diagnostics.push({
            severity: 'error',
            message: err.message,
            path: pathStr([...inst.path, m.name]),
            code,
            ...(tagged ? { by } : {}),
          });
        } else if (err instanceof QueryCycle) {
          // a slot that (transitively) reads itself — §7.6/§9.3 dependency cycle
          s.state = 'invalid';
          const ends = err.cycle.map((k) => memberOfSlotKey(k));
          this.diagnostics.push({
            severity: 'error',
            message: `dependency cycle: ${ends[0]} -> ${ends[ends.length - 1]}`,
            path: pathStr([...inst.path, m.name]),
            code: 'E5007',
            by,
          });
        } else if (err instanceof Taint) {
          s.state = 'invalid'; // the value layer already reported the diagnostic
        } else throw err;
      }
    }
  }

  private materializeValue(v: Value, seen: Set<RecInst>): void {
    if (isRec(v)) this.materialize(v, seen);
    else if (isArr(v)) for (const x of v.items) this.materializeValue(x, seen);
    else if (isMap(v)) for (const x of v.entries.values()) this.materializeValue(x, seen);
  }

  run(roots: RootSpec[] = this.env.outputs.map((o) => ({ ...o, menv: this.env }))): QReport {
    // bind every root first, then force the universe (as the source pipeline
    // does), so a reference from one root into another resolves (§9.3). Across
    // modules every module's outputs are roots, each in its own module scope.
    const built: { name: string; v: Value }[] = [];
    for (const o of roots) {
      try {
        const menv = o.menv;
        const rt = menv.resolve(o.type);
        const cctx: CCtx = {
          entryEnv: this.env,
          self: null,
          rootName: o.name,
          locals: new Set(),
          resolveType: (t) => menv.resolve(t),
          menv,
        };
        const sc: BindScope = { inst: null, locals: new Map(), rootName: o.name, menv };
        const v = this.bindValue(o.expr, rt, [o.name], null, cctx, sc)(this.cx, NO_LOCALS);
        this.env.roots.set(o.name, v);
        built.push({ name: o.name, v });
      } catch (err) {
        if (err instanceof EvalErr)
          // a raw evaluation error at an output's root propagates untagged
          this.diagnostics.push({
            severity: 'error',
            message: err.message,
            path: o.name,
            code: (err as { code?: string }).code,
          });
        else if (err instanceof Taint) {
          // the value layer already reported the diagnostic to env.diagnostics
        } else throw err; // Unsupported (or a real bug) bubbles to the caller
      }
    }
    // force the whole universe, not only what the outputs reach: $referrers
    // needs every candidate instance built (e.g. a graph's edges), as the
    // reference's phase-1 forceAll does. The registry grows as records are
    // built, so iterate to its (moving) end; `seen` avoids re-forcing.
    const seen = new Set<RecInst>();
    for (let i = 0; i < this.env.registry.length; i++) this.materialize(this.env.registry[i], seen);
    // A value-layer record (a call/with/union/comprehension result) may carry a
    // member that reads $referrers; the reference's binder marks such members
    // deferred and its forceAll leaves them for phase 2. With the universe now
    // fully built, complete that phase over it, so those members answer against
    // the whole registry (as the reference's settle does).
    const eng = this.helper as unknown as {
      phase: number;
      deferredSlots: { inst: RecInst; name: string }[];
      takeSnapshot(edges: Map<string, unknown>): void;
      forceSlotSafe(inst: RecInst, name: string): void;
    };
    if (eng.deferredSlots.length) {
      eng.phase = 2;
      eng.takeSnapshot(new Map());
      for (const d of eng.deferredSlots.splice(0)) eng.forceSlotSafe(d.inst, d.name);
    }
    const outputs: { name: string; json: string }[] = [];
    for (const { name, v } of built) {
      try {
        this.materializeValue(v, seen);
        outputs.push({ name, json: this.helper.serialize(v, name) });
      } catch (err) {
        if (err instanceof EvalErr)
          this.diagnostics.push({
            severity: 'error',
            message: err.message,
            path: name,
            code: (err as { code?: string }).code,
          });
        else if (err instanceof Taint) {
          // the value layer already reported the diagnostic to env.diagnostics
        } else throw err;
      }
    }
    // validate the forced universe (§6): every instance's assertions and
    // when-blocks, read from the materialized slots by the value layer
    this.helper.validateAll('');
    // a single error anywhere suppresses every output (§9.3, as evaluateSource);
    // assertion diagnostics land on the shared env, the rest on our own list.
    // Sort them in (path, id) order (§6.7), as the source pipeline does.
    const diagnostics = sortDiags([...this.diagnostics, ...this.env.diagnostics]);
    const ok = !diagnostics.some((d) => d.severity === 'error');
    return { ok, outputs: ok ? outputs : [], diagnostics };
  }
}

/**
 * Evaluate a universe in rounds (§7.6): each round runs a fresh QEval that
 * answers `$referrers` from the previous round's frozen universe, until the
 * queried edges settle (E5009 if they never do). A universe that never asks
 * `$referrers` queries no edges and settles in one round — the loop is
 * transparent for it. `mkEval` builds a round's QEval and result over the entry
 * env, which is reset between rounds.
 */
function evalRounds(
  entryEnv: Env,
  mkEval: (prev: Frozen | null) => { report: QReport; live: Map<string, Edge> },
): QReport {
  let prev: Frozen | null = null;
  let prevEdges = new Map<string, Edge>();
  const seen = new Map<string, number>();
  for (let round = 1; ; round++) {
    const { report, live } = mkEval(prev);
    const changed = [...live.keys()]
      .filter((k) => !edgeEq(live.get(k)!, prevEdges.get(k) ?? new Map()))
      .sort();
    const stable = changed.length === 0;
    // a repeated edge set never settles: report at once, not after the budget
    const cycled = !stable && seen.has(edgesKey(live));
    if (stable || cycled || round === ROUNDS) {
      if (stable) return report;
      const e5009 = changed.map((key) => {
        const [t, m] = key.split('|');
        return {
          severity: 'error',
          message: `$referrers(${t}, "${m}") does not stabilize after ${ROUNDS} rounds`,
          path: edgeDiff(live.get(key)!, prevEdges.get(key) ?? new Map()),
          code: 'E5009',
        };
      });
      return { ok: false, outputs: [], diagnostics: sortDiags([...report.diagnostics, ...e5009]) };
    }
    seen.set(edgesKey(live), round);
    // this round becomes the next round's frozen universe; start over
    prev = { registry: entryEnv.registry.slice(), edges: live };
    prevEdges = live;
    entryEnv.roots.clear();
    entryEnv.registry.splice(0);
    entryEnv.diagnostics.length = 0;
  }
}

/** evaluate a single module's outputs (its own env is the whole universe) */
export function qevaluate(env: Env): QReport {
  return evalRounds(env, (prev) => {
    const ev = new QEval(env, prev);
    const report = ev.run();
    return { report, live: ev.liveEdges() };
  });
}

/**
 * Evaluate a whole multi-module universe (§8.8): every module's outputs are
 * roots, each bound and evaluated in its own module scope, into the entry
 * module's universe. Imported names and non-entry-module consts/functions are
 * resolved through the value layer.
 */
export function qevaluateUniverse(mods: { env: Env }[], entry: { env: Env }): QReport {
  const roots: RootSpec[] = mods.flatMap((m) =>
    m.env.outputs.map((o) => ({ name: o.name, expr: o.expr, type: o.type, menv: m.env })),
  );
  return evalRounds(entry.env, (prev) => {
    const ev = new QEval(entry.env, prev, mods);
    const report = ev.run(roots);
    return { report, live: ev.liveEdges() };
  });
}
