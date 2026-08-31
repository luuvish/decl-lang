// Expression-level static analysis: type inference, assignability (§3.18,
// strict S ⊑ T), the absence discipline (§4.10) with its two narrowing
// rules, and the `match` static checks (§4.7). Inference is conservative:
// a form whose type cannot be determined yields `unknown` (rt: null) and
// suppresses downstream judgments rather than guessing.
import type { Expr, TypeAst } from './ast.ts';
import { Env } from './semantics.ts';
import type { RT } from './semantics.ts';
import { subsumes } from './subsume.ts';

export type Ty = { rt: RT | null; abs: boolean };
const UNK: Ty = { rt: null, abs: false };
const PRIM = (name: string): RT => ({ t: 'prim', name });
const BOOL: Ty = { rt: PRIM('bool'), abs: false };

const UNIT_DIMS: Record<string, string> = { s: 'Time', ms: 'Time', us: 'Time', ns: 'Time' };

export interface ICtx {
  env: Env;
  report: (code: string, msg: string) => void;
  vars: Map<string, Ty>;
  present: Set<string>;   // narrowed definitely-present paths
  nonnull: Set<string>;   // narrowed non-null paths
  constMemo: Map<string, Ty>;
}

export function makeCtx(env: Env, report: (code: string, msg: string) => void): ICtx {
  return { env, report, vars: new Map(), present: new Set(), nonnull: new Set(), constMemo: new Map() };
}
const child = (cx: ICtx, vars?: Map<string, Ty>): ICtx =>
  ({ ...cx, vars: vars ?? new Map(cx.vars), present: new Set(cx.present), nonnull: new Set(cx.nonnull) });

// ---------------- type utilities ----------------
const isNullLit = (t: RT) => (t.t === 'lit' && t.v === null) || (t.t === 'prim' && t.name === 'null');
export function hasNull(rt: RT | null): boolean {
  if (!rt) return false;
  if (isNullLit(rt)) return true;
  if (rt.t === 'union') return rt.arms.some(hasNull);
  return false;
}
function stripNull(rt: RT): RT {
  if (rt.t === 'union') {
    const arms = rt.arms.filter((a: RT) => !isNullLit(a));
    return arms.length === 1 ? arms[0] : { t: 'union', arms };
  }
  return rt;
}
function mkUnion(arms: (RT | null)[]): RT | null {
  if (arms.some(a => !a)) return null;
  const flat: RT[] = [];
  for (const a of arms as RT[]) (a.t === 'union' ? flat.push(...a.arms) : flat.push(a));
  const uniq = flat.filter((a, i) => flat.findIndex(b => sameRT(a, b)) === i);
  return uniq.length === 1 ? uniq[0] : { t: 'union', arms: uniq };
}
function sameRT(a: RT, b: RT): boolean {
  if (a === b) return true;
  if (a.t !== b.t) return false;
  if (a.t === 'prim') return a.name === b.name;
  if (a.t === 'lit') return typeof a.v === typeof b.v && a.v === b.v;
  return false;
}
function numKind(rt: RT | null): string | null {
  if (!rt) return null;
  if (rt.t === 'prim') return ['int', 'float', 'string', 'bool'].includes(rt.name) ? rt.name : null;
  if (rt.t === 'lit') {
    if (typeof rt.v === 'bigint') return 'int';
    if (typeof rt.v === 'number') return 'float';
    if (typeof rt.v === 'string') return 'string';
    if (typeof rt.v === 'boolean') return 'bool';
    return null;
  }
  if (rt.t === 'range') return rt.base;
  if (rt.t === 'pattern') return 'string';
  if (rt.t === 'pred') return numKind(rt.base);
  if (rt.t === 'quantity') return 'quantity';
  if (rt.t === 'union') {
    const ks = rt.arms.map(numKind);
    return ks.every((k: any) => k && k === ks[0]) ? ks[0] : null;
  }
  return null;
}
const isBoolish = (rt: RT | null) => !rt || numKind(rt) === 'bool';

// structural view: unwrap ref/pred and select the arm of an intersection
// that has the wanted shape (merged `&` members carry conj arms)
function armOf(rt: RT | null, t: string): RT | null {
  if (!rt) return null;
  if (rt.t === 'ref' && t !== 'ref') return armOf(rt.target, t);
  if (rt.t === 'pred') return armOf(rt.base, t);
  if (rt.t === t) return rt;
  if (rt.t === 'isectN') {
    for (const x of rt.arms) { const v = armOf(x, t); if (v) return v; }
  }
  return null;
}

// ---------------- navigation paths & narrowing ----------------
function pathKey(e: Expr): string | null {
  switch (e.e) {
    case 'name': return e.name;
    case 'ctx': return e.name;
    case 'paren': return pathKey(e.x);
    case 'member': {
      if (e.safe) return null;
      const b = pathKey(e.x); return b ? `${b}.${e.name}` : null;
    }
    case 'index': {
      const b = pathKey(e.x); if (!b) return null;
      if (e.i.e === 'lit') return `${b}[${String(e.i.v)}]`;
      if (e.i.e === 'name') return `${b}[${e.i.name}]`;
      return null;
    }
  }
  return null;
}
type Guards = { present: string[]; nonnull: string[] };
export function guardsOf(e: Expr, polarity: boolean): Guards {
  const none: Guards = { present: [], nonnull: [] };
  switch (e.e) {
    case 'paren': return guardsOf(e.x, polarity);
    case 'un': return e.op === '!' ? guardsOf(e.x, !polarity) : none;
    case 'bin': {
      if (e.op === '&&' && polarity) return merge(guardsOf(e.l, true), guardsOf(e.r, true));
      if (e.op === '||' && !polarity) return merge(guardsOf(e.l, false), guardsOf(e.r, false));
      if (e.op === 'in' && polarity) {
        const b = pathKey(e.r);
        if (!b) return none;
        if (e.l.e === 'lit' && typeof e.l.v === 'string') return { present: [`${b}.${e.l.v}`, `${b}[${e.l.v}]`], nonnull: [] };
        if (e.l.e === 'name') return { present: [`${b}[${e.l.name}]`], nonnull: [] };
        return none;
      }
      const nullSide = e.l.e === 'lit' && e.l.v === null ? e.r : e.r.e === 'lit' && e.r.v === null ? e.l : null;
      if (nullSide) {
        const p = pathKey(nullSide);
        if (p && ((e.op === '!=' && polarity) || (e.op === '==' && !polarity))) return { present: [], nonnull: [p] };
      }
      return none;
    }
  }
  return none;
}
// is a name already taken here? (locals or the module namespace — the
// no-shadowing rule E3019 spans both)
function nameBound(cx: ICtx, n: string): boolean {
  return cx.vars.has(n) || cx.env.consts.has(n) || cx.env.funcs.has(n)
    || cx.env.typeAsts.has(n) || cx.env.inputs.has(n)
    || cx.env.outputs.some(o => o.name === n);
}

const merge = (a: Guards, b: Guards): Guards =>
  ({ present: [...a.present, ...b.present], nonnull: [...a.nonnull, ...b.nonnull] });
export function applyGuards(cx: ICtx, g: Guards): ICtx {
  const c2 = child(cx);
  g.present.forEach(p => c2.present.add(p));
  g.nonnull.forEach(p => c2.nonnull.add(p));
  return c2;
}

// ---------------- stdlib signatures (arity + result) ----------------
const STD: Record<string, { arity: number; ret: RT | null }> = {
  'array.count': { arity: 1, ret: PRIM('int') },
  'array.all': { arity: 2, ret: PRIM('bool') },
  'array.any': { arity: 2, ret: PRIM('bool') },
  'array.filter': { arity: 2, ret: null },
  'array.all_distinct': { arity: 1, ret: PRIM('bool') },
  'array.sum': { arity: 1, ret: null },
  'array.fold': { arity: 3, ret: null },
  'map.keys': { arity: 1, ret: { t: 'arr', elem: PRIM('string') } },
  'map.values': { arity: 1, ret: null },
  'string.length': { arity: 1, ret: PRIM('int') },
  'string.of': { arity: 1, ret: PRIM('string') },
  'string.join': { arity: 2, ret: PRIM('string') },
  'string.starts_with': { arity: 2, ret: PRIM('bool') },
  'ref.path': { arity: 1, ret: PRIM('string') },
  'math.abs': { arity: 1, ret: null },
  'math.min': { arity: 2, ret: null },
  'math.max': { arity: 2, ret: null },
};
function stdPath(e: Expr): string | null {
  if (e.e === 'member' && !e.safe) {
    const b = stdPath(e.x);
    return b !== null ? (b ? `${b}.${e.name}` : e.name) : null;
  }
  return e.e === 'name' && e.name === 'std' ? '' : null;
}

// ---------------- the judgment ----------------
export function tryResolve(env: Env, ast: TypeAst | undefined): RT | null {
  if (!ast) return null;
  try { return env.resolve(ast); } catch { return null; }
}

export function requireVal(cx: ICtx, e: Expr, ty: Ty, what: string): Ty {
  if (ty.abs) {
    const k = pathKey(e);
    if (!k || !cx.present.has(k))
      cx.report('E4050', `maybe-absent expression consumed ${what} (use ?. / ?? or an \`in\` guard)`);
  }
  return ty;
}

export function infer(cx: ICtx, e: Expr): Ty {
  switch (e.e) {
    case 'lit': return { rt: { t: 'lit', v: e.v }, abs: false };
    case 'unitlit': return { rt: { t: 'quantity', dim: UNIT_DIMS[e.unit] ?? '?' }, abs: false };
    case 'template': {
      for (const p of e.parts) if (typeof p !== 'string') requireVal(cx, p, infer(cx, p), 'in a template');
      return { rt: PRIM('string'), abs: false };
    }
    case 'name': {
      if (cx.vars.has(e.name)) return cx.vars.get(e.name)!;
      const { env } = cx;
      if (env.consts.has(e.name)) return constTy(cx, e.name);
      if (env.funcs.has(e.name)) return { rt: funcRT(cx, e.name), abs: false };
      if (e.name === 'std') return UNK;
      const o = env.outputs.find(o => o.name === e.name);
      if (o) return { rt: tryResolve(env, o.type), abs: false };
      if (env.inputs.has(e.name)) return { rt: tryResolve(env, env.inputs.get(e.name)!.type), abs: false };
      if (env.typeAsts.has(e.name)) { cx.report('E3008', `type/namespace name ${e.name} used as a value`); return UNK; }
      cx.report('E3003', `unknown name ${e.name}`);
      return UNK;
    }
    case 'ctx': return cx.vars.get(e.name) ?? UNK;
    case 'referrers': {
      const rt = tryResolve(cx.env, { k: 'named', name: e.type, args: [] });
      if (!rt) cx.report('E4091', `$referrers: unknown record type ${e.type}`);
      else if (rt.t !== 'rec') cx.report('E4091', `$referrers: ${e.type} is not a record type`);
      return { rt: rt && rt.t === 'rec' ? { t: 'arr', elem: { t: 'ref', target: rt } } : null, abs: false };
    }
    case 'obj': {
      for (const en of e.entries) requireVal(cx, en.val, infer(cx, en.val), 'as a construction member');
      return UNK;   // literals are typed by their checked position (§3.18)
    }
    case 'arr': {
      const ts = e.items.map(it => {
        const t = requireVal(cx, it.expr, infer(cx, it.expr), 'as an array element');
        return it.spread ? (t.rt?.t === 'arr' ? t.rt.elem : null) : t.rt;
      });
      const elem = mkUnion(ts);
      return { rt: elem ? { t: 'arr', elem } : null, abs: false };
    }
    case 'comp': case 'mapcomp': {
      let c2 = child(cx);
      for (const cl of e.clauses) {
        const vt = iterVarTy(c2, cl.iter);
        if (nameBound(c2, cl.v)) cx.report('E3019', `comprehension variable ${cl.v} shadows an enclosing name`);
        c2.vars.set(cl.v, vt);
        for (const f of cl.filters) {
          requireVal(c2, f, infer(c2, f), 'as a filter');
          c2 = applyGuards(c2, guardsOf(f, true));
        }
      }
      if (e.e === 'comp') {
        const h = requireVal(c2, e.head, infer(c2, e.head), 'as a comprehension element');
        return { rt: h.rt ? { t: 'arr', elem: h.rt } : null, abs: false };
      }
      const k = requireVal(c2, e.key, infer(c2, e.key), 'as a map key');
      if (k.rt && numKind(k.rt) !== 'string') cx.report('E4001', 'map-comprehension key is not a string');
      const v = requireVal(c2, e.val, infer(c2, e.val), 'as a map value');
      return { rt: v.rt ? { t: 'map', key: PRIM('string'), val: v.rt } : null, abs: false };
    }
    case 'bin': return inferBin(cx, e);
    case 'un': {
      const t = requireVal(cx, e.x, infer(cx, e.x), `as \`${e.op}\` operand`);
      if (e.op === '!') { if (t.rt && !isBoolish(t.rt)) cx.report('E4071', '`!` on a non-bool operand'); return BOOL; }
      if (e.op === '~') { if (t.rt && numKind(t.rt) !== 'int') cx.report('E4071', '`~` on a non-int operand'); return { rt: PRIM('int'), abs: false }; }
      const k = numKind(t.rt);
      if (t.rt && k !== 'int' && k !== 'float' && k !== 'quantity') cx.report('E4071', 'unary `-` on a non-numeric operand');
      return { rt: k === 'int' || k === 'float' ? PRIM(k) : null, abs: false };
    }
    case 'paren': return infer(cx, e.x);
    case 'if': {
      const c = requireVal(cx, e.c, infer(cx, e.c), 'as a condition');
      if (c.rt && !isBoolish(c.rt)) cx.report('E4001', '`if` condition is not bool');
      const t = infer(applyGuards(cx, guardsOf(e.c, true)), e.t);
      const f = infer(applyGuards(cx, guardsOf(e.c, false)), e.f);
      return { rt: mkUnion([t.rt, f.rt]), abs: t.abs || f.abs };
    }
    case 'lambda': {
      const c2 = child(cx);
      for (const p of e.params) {
        if (nameBound(c2, p)) cx.report('E3019', `lambda parameter ${p} shadows an enclosing name`);
        c2.vars.set(p, UNK);
      }
      infer(c2, e.body);
      return UNK;
    }
    case 'call': return inferCall(cx, e);
    case 'member': return inferMember(cx, e);
    case 'index': {
      const b = requireVal(cx, e.x, infer(cx, e.x), 'for indexing');
      return indexCore(cx, b, e);
    }
    case 'with': {
      const b = requireVal(cx, e.base, infer(cx, e.base), 'as `with` base');
      const brt = b.rt && b.rt.t === 'ref' ? b.rt.target : b.rt;
      if (brt && brt.t !== 'rec') { cx.report('E4080', '`with` on a non-record base'); return UNK; }
      if (e.patch.e === 'obj' && brt) {
        for (const en of e.patch.entries) {
          const m = brt.members.find((m: any) => m.name === en.key);
          if (!m && !brt.open) cx.report('E4080', `\`with\` updates unknown member ${en.key}`);
          else if (m?.kind === 'der') cx.report('E4080', `\`with\` updates derived member ${en.key}`);
        }
      }
      if (e.patch.e === 'obj') for (const en of e.patch.entries) requireVal(cx, en.val, infer(cx, en.val), 'as a `with` update');
      else infer(cx, e.patch);
      return { rt: brt, abs: false };
    }
    case 'match': return inferMatch(cx, e, null);
  }
  return UNK;
}

function iterVarTy(cx: ICtx, iter: Expr): Ty {
  const t = requireVal(cx, iter, infer(cx, iter), 'as an iterable');
  if (iter.e === 'bin' && (iter.op === '..' || iter.op === '..<')) {
    const lo = iter.l.e === 'lit' ? iter.l.v : undefined;
    const hi = iter.r.e === 'lit' ? iter.r.v : undefined;
    if (typeof lo === 'number' || typeof hi === 'number') { cx.report('E4115', 'comprehension over a float range'); return UNK; }
    if (lo !== undefined && hi !== undefined)
      return { rt: { t: 'range', base: 'int', lo, hi, excl: iter.op === '..<' }, abs: false };
    return { rt: PRIM('int'), abs: false };
  }
  if (!t.rt) return UNK;
  const asArr = armOf(t.rt, 'arr');
  if (asArr) return { rt: asArr.elem, abs: false };
  cx.report('E4115', `comprehension over a non-iterable ${armOf(t.rt, 'map') ? 'map (use std.map.keys/values)' : 'value'}`);
  return UNK;
}

function inferBin(cx: ICtx, e: Expr & { e: 'bin' }): Ty {
  const { op } = e;
  if (op === '|>') {   // first-argument insertion (§4.9)
    const call: Expr = e.r.e === 'call'
      ? { e: 'call', fn: e.r.fn, args: [e.l, ...e.r.args] }
      : { e: 'call', fn: e.r, args: [e.l] };
    return inferCall(cx, call as any);
  }
  if (op === '??') {
    const l = infer(cx, e.l);   // absence/null on the left is the point
    const r = requireVal(cx, e.r, infer(cx, e.r), 'as `??` fallback');
    return { rt: l.rt && r.rt ? mkUnion([stripNull(l.rt), r.rt]) : null, abs: false };
  }
  if (op === '&&' || op === '||') {
    const l = requireVal(cx, e.l, infer(cx, e.l), `as \`${op}\` operand`);
    if (l.rt && !isBoolish(l.rt)) cx.report('E4071', `\`${op}\` on a non-bool operand`);
    const c2 = applyGuards(cx, guardsOf(e.l, op === '&&'));
    const r = requireVal(c2, e.r, infer(c2, e.r), `as \`${op}\` operand`);
    if (r.rt && !isBoolish(r.rt)) cx.report('E4071', `\`${op}\` on a non-bool operand`);
    return BOOL;
  }
  if (op === 'in') {
    requireVal(cx, e.l, infer(cx, e.l), 'as `in` key');
    const r = requireVal(cx, e.r, infer(cx, e.r), 'as `in` container');
    const rrt = r.rt && r.rt.t === 'ref' ? r.rt.target : r.rt;
    if (rrt && rrt.t === 'rec' && e.l.e === 'lit' && typeof e.l.v === 'string') {
      const m = rrt.members.find((m: any) => m.name === (e.l as any).v);
      if (m && m.kind !== 'opt') cx.report('E4054', `\`in\` on member ${e.l.v}, which is not optional`);
      if (!m && !rrt.open) cx.report('E4054', `\`in\` on undeclared member ${e.l.v} of a closed record`);
    }
    return BOOL;
  }
  if (op === '..' || op === '..<') {
    requireVal(cx, e.l, infer(cx, e.l), 'as a range endpoint');
    requireVal(cx, e.r, infer(cx, e.r), 'as a range endpoint');
    return UNK;   // a range value: iterable / membership container only
  }
  const l = requireVal(cx, e.l, infer(cx, e.l), `as \`${op}\` operand`);
  const r = requireVal(cx, e.r, infer(cx, e.r), `as \`${op}\` operand`);
  if (op === '==' || op === '!=' || op === 'matches') return BOOL;
  const lk = numKind(l.rt), rk = numKind(r.rt);
  const cmp = ['<', '<=', '>', '>='].includes(op);
  if (l.rt && r.rt && lk && rk && lk !== rk && !(lk === 'quantity' && rk === 'quantity'))
    cx.report('E4071', `\`${op}\` mixes ${lk} and ${rk} operands`);
  if (cmp) return BOOL;
  if (['&', '^', '<<', '>>'].includes(op)) {
    if ((l.rt && lk !== 'int') || (r.rt && rk !== 'int')) cx.report('E4071', `\`${op}\` on non-int operands`);
    return { rt: PRIM('int'), abs: false };
  }
  if (op === '|') {   // bitwise on ints (type-level | never reaches expressions)
    if ((l.rt && lk !== 'int') || (r.rt && rk !== 'int')) cx.report('E4071', '`|` on non-int operands');
    return { rt: PRIM('int'), abs: false };
  }
  // + - * / %
  if (op === '+' && lk === 'string' && rk === 'string') return { rt: PRIM('string'), abs: false };
  if (l.rt && r.rt && lk && rk) {
    if (!['int', 'float', 'quantity'].includes(lk)) cx.report('E4071', `\`${op}\` on ${lk} operands`);
    if (lk === 'int' && ['+', '-', '*'].includes(op)) {
      // interval arithmetic keeps range-typed operands range-typed, so
      // `9000 + i` with i: 0..<3 stays assignable where 1..65535 is expected
      const a = asIval(l.rt), b = asIval(r.rt);
      if (a && b) {
        const cands = op === '+' ? [a[0] + b[0], a[1] + b[1]]
          : op === '-' ? [a[0] - b[1], a[1] - b[0]]
          : [a[0] * b[0], a[0] * b[1], a[1] * b[0], a[1] * b[1]];
        const lo = cands.reduce((x, y) => x < y ? x : y);
        const hi = cands.reduce((x, y) => x > y ? x : y);
        return { rt: { t: 'range', base: 'int', lo, hi, excl: false }, abs: false };
      }
    }
    return { rt: lk === 'int' || lk === 'float' ? PRIM(lk) : null, abs: false };
  }
  return UNK;
}
function asIval(rt: RT): [bigint, bigint] | null {
  if (rt.t === 'lit' && typeof rt.v === 'bigint') return [rt.v, rt.v];
  if (rt.t === 'range' && rt.base === 'int' && typeof rt.lo === 'bigint' && typeof rt.hi === 'bigint')
    return [rt.lo, rt.excl ? rt.hi - 1n : rt.hi];
  if (rt.t === 'union') {
    const ivs = rt.arms.map(asIval);
    if (ivs.every((v: any) => v))
      return [ivs.reduce((x: bigint, v: any) => v[0] < x ? v[0] : x, ivs[0]![0]),
              ivs.reduce((x: bigint, v: any) => v[1] > x ? v[1] : x, ivs[0]![1])];
  }
  if (rt.t === 'pred') return asIval(rt.base);
  return null;
}

function indexCore(cx: ICtx, b: Ty, e: Expr & { e: 'index' }): Ty {
  const it = requireVal(cx, e.i, infer(cx, e.i), 'as an index');
  if (!b.rt) return UNK;
  const asArr = armOf(b.rt, 'arr');
  if (asArr) {
    if (it.rt && numKind(it.rt) !== 'int') cx.report('E4071', 'array index is not an int');
    return { rt: asArr.elem, abs: false };
  }
  const asMap = armOf(b.rt, 'map');
  if (asMap) {
    const k = pathKey(e);
    return { rt: asMap.val, abs: !(k && cx.present.has(k)) };
  }
  if (armOf(b.rt, 'rec')) return UNK;   // dynamic member access
  cx.report('E4071', 'indexing a non-collection');
  return UNK;
}

function inferMember(cx: ICtx, e: Expr & { e: 'member' }): Ty {
  if (stdPath(e) !== null) return UNK;   // std.* namespace path (typed at the call)
  const b = infer(cx, e.x);
  const key = pathKey(e.x);
  if (!e.safe) {
    if (b.abs && !(key && cx.present.has(key)))
      cx.report('E4050', `member access on a maybe-absent expression (use ?. or an \`in\` guard)`);
    if (hasNull(b.rt) && !(key && cx.nonnull.has(key)))
      cx.report('E4051', `member .${e.name} on a possibly-null expression without ?.`);
  }
  return memberCore(cx, b, e);
}
function memberCore(cx: ICtx, b: Ty, e: Expr & { e: 'member' }): Ty {
  let brt = b.rt ? stripNull(b.rt) : null;
  if (brt && brt.t === 'ref') brt = brt.target;
  if (brt && brt.t === 'pred') brt = brt.base;
  if (brt && brt.t === 'isectN') brt = armOf(brt, 'rec') ?? armOf(brt, 'map') ?? brt;
  const mkAbs = (t: Ty): Ty => e.safe ? { rt: t.rt, abs: true } : t;
  if (!brt) return mkAbs(UNK);
  if (brt.t === 'rec') {
    const m = brt.members.find((m: any) => m.name === e.name);
    if (!m) {
      if (!brt.open) cx.report('E4003', `member ${e.name} is not declared on ${brt.name ?? 'this record'}`);
      return mkAbs(UNK);
    }
    const rt: RT | null = m.conj ? { t: 'isectN', arms: m.conj } : (m.type ?? null);
    const k = pathKey(e);
    return { rt, abs: e.safe || (m.kind === 'opt' && !(k && cx.present.has(k))) };
  }
  if (brt.t === 'map') {
    const k = pathKey(e);
    return { rt: brt.val, abs: e.safe || !(k && cx.present.has(k)) };
  }
  if (brt.t === 'union') {
    const parts = brt.arms.map((a: RT) => {
      const arm = a.t === 'rec' ? a.members.find((m: any) => m.name === e.name) : null;
      return arm?.type ?? null;
    });
    return mkAbs({ rt: mkUnion(parts), abs: false });
  }
  if (brt.t === 'quantity' && (e.name === 'value' || e.name === 'unit'))
    return mkAbs({ rt: e.name === 'value' ? PRIM('float') : PRIM('string'), abs: false });
  return mkAbs(UNK);
}

function funcRT(cx: ICtx, name: string): RT {
  const f = cx.env.funcs.get(name)!;
  return {
    t: 'func',
    params: f.params.map(p => tryResolve(cx.env, p.type) ?? { t: 'any' }),
    ret: tryResolve(cx.env, f.ret) ?? null,
  };
}
function constTy(cx: ICtx, name: string): Ty {
  if (cx.constMemo.has(name)) return cx.constMemo.get(name)!;
  cx.constMemo.set(name, UNK);   // cycle guard
  const c = cx.env.consts.get(name)!;
  const anno = tryResolve(cx.env, (c as any).type);
  const ty: Ty = anno ? { rt: anno, abs: false }
    : infer(makeCtx(cx.env, () => { }), c.expr);   // silent module-scope inference
  cx.constMemo.set(name, ty);
  return ty;
}

function inferCall(cx: ICtx, e: Expr & { e: 'call' }): Ty {
  const sp = stdPath(e.fn);
  if (sp !== null) {
    const sig = STD[sp];
    if (sig && e.args.length !== sig.arity)
      cx.report('E4062', `std.${sp} expects ${sig.arity} argument(s), got ${e.args.length}`);
    for (const a of e.args) {
      if (a.e === 'lambda') { infer(cx, a); continue; }
      requireVal(cx, a, infer(cx, a), 'as an argument');
    }
    return { rt: sig?.ret ?? null, abs: false };
  }
  const f = infer(cx, e.fn);
  const frt = f.rt && f.rt.t === 'func' ? f.rt : null;
  if (frt && e.args.length !== frt.params.length)
    cx.report('E4062', `call expects ${frt.params.length} argument(s), got ${e.args.length}`);
  e.args.forEach((a, i) => {
    const expected: RT | null = frt && i < frt.params.length && frt.params[i].t !== 'any' ? frt.params[i] : null;
    if (a.e === 'lambda' && expected && expected.t === 'func') { checkLambda(cx, a, expected); return; }
    if (a.e === 'lambda') { infer(cx, a); return; }
    const at = requireVal(cx, a, infer(cx, a), 'as an argument');
    if (at.rt && expected && !subsumes(cx.env, at.rt, expected) && !deferrable(at.rt, expected))
      cx.report('E4001', `argument ${i + 1} is not assignable to its parameter`);
  });
  return { rt: frt?.ret ?? null, abs: false };
}
function checkLambda(cx: ICtx, e: Expr & { e: 'lambda' }, expected: RT) {
  if (e.params.length !== expected.params.length) { cx.report('E4062', 'lambda arity differs from expected function type'); return; }
  const c2 = child(cx);
  e.params.forEach((p, i) => {
    if (nameBound(c2, p)) cx.report('E3019', `lambda parameter ${p} shadows an enclosing name`);
    c2.vars.set(p, { rt: expected.params[i].t === 'any' ? null : expected.params[i], abs: false });
  });
  const b = requireVal(c2, e.body, infer(c2, e.body), 'as a lambda result');
  if (b.rt && expected.ret && !subsumes(cx.env, b.rt, expected.ret) && !deferrable(b.rt, expected.ret))
    cx.report('E4001', 'lambda body is not assignable to the expected result type');
}

// ---------------- match (§4.7) ----------------
function inferMatch(cx: ICtx, e: Expr & { e: 'match' }, expected: RT | null): Ty {
  const s = requireVal(cx, e.subject, infer(cx, e.subject), 'as a match subject');
  let variants: RT[] | null = null;
  if (s.rt) {
    const srt = stripNull(s.rt);
    if (srt.t === 'union') variants = hasNull(s.rt) ? [...srt.arms, { t: 'lit', v: null }] : srt.arms;
    else cx.report('E4103', '`match` subject is not a discriminable union');
  }
  const covered = new Set<number>();
  let catchAlls = 0;
  const results: (RT | null)[] = [];
  for (const arm of e.arms) {
    const c2 = child(cx);
    if (nameBound(c2, arm.v)) cx.report('E3019', `match binding ${arm.v} shadows an enclosing name`);
    let armTy: RT | null = null;
    if (arm.type) {
      armTy = tryResolve(cx.env, arm.type);
      if (arm.type && !armTy) cx.report('E3003', `unknown type in match arm`);
      if (variants && armTy) {
        const sel = variants.map((v, i) => [v, i] as const).filter(([v]) => subsumes(cx.env, v, armTy!));
        for (const [, i] of sel) {
          if (covered.has(i)) cx.report('E4100', `match arms overlap on a variant`);
          covered.add(i);
        }
      }
    } else {
      catchAlls++;
      if (variants) {
        const rest = variants.filter((_, i) => !covered.has(i));
        if (rest.length === 0) cx.report('E4102', 'match catch-all is dead (typed arms are exhaustive)');
        armTy = mkUnion(rest);
      }
    }
    c2.vars.set(arm.v, { rt: armTy, abs: false });
    const b = requireVal(c2, arm.body, expected ? checkExpr(c2, arm.body, expected) : infer(c2, arm.body), 'as a match result');
    results.push(b.rt);
  }
  if (catchAlls > 1) cx.report('E4100', 'more than one match catch-all arm');
  if (variants && catchAlls === 0 && covered.size < variants.length)
    cx.report('E4101', '`match` is not exhaustive over the subject union');
  return { rt: mkUnion(results), abs: false };
}

// ---------------- bidirectional checking (§3.18) ----------------
// a navigation expression in a ref<T> position denotes a place (§7.4):
// the absence discipline does not apply along the spine — whether the
// place holds a value is reference integrity (§7.5), checked at binding
function placeTy(cx: ICtx, e: Expr): Ty {
  switch (e.e) {
    case 'paren': return placeTy(cx, e.x);
    case 'member': return memberCore(cx, placeTy(cx, e.x), e);
    case 'index': return indexCore(cx, placeTy(cx, e.x), e);
    default: return infer(cx, e);
  }
}

export function checkExpr(cx: ICtx, e: Expr, expected: RT | null): Ty {
  if (!expected) return infer(cx, e);
  if (expected.t === 'ref') { placeTy(cx, e); return { rt: expected, abs: false }; }   // place, not value (§7.4)
  if (expected.t === 'pred') return checkExpr(cx, e, expected.base);
  if (expected.t === 'isectN' && (e.e === 'obj' || e.e === 'arr' || e.e === 'comp' || e.e === 'mapcomp')) {
    for (const arm of expected.arms) checkExpr(cx, e, arm);   // a literal must satisfy every arm
    return { rt: expected, abs: false };
  }
  switch (e.e) {
    case 'comp': {
      if (expected.t !== 'arr') break;
      let c2 = child(cx);
      for (const cl of e.clauses) {
        const vt = iterVarTy(c2, cl.iter);
        if (nameBound(c2, cl.v)) cx.report('E3019', `comprehension variable ${cl.v} shadows an enclosing name`);
        c2.vars.set(cl.v, vt);
        for (const f of cl.filters) {
          requireVal(c2, f, infer(c2, f), 'as a filter');
          c2 = applyGuards(c2, guardsOf(f, true));
        }
      }
      checkExpr(c2, e.head, expected.elem);
      return { rt: expected, abs: false };
    }
    case 'mapcomp': {
      if (expected.t !== 'map') break;
      let c2 = child(cx);
      for (const cl of e.clauses) {
        const vt = iterVarTy(c2, cl.iter);
        if (nameBound(c2, cl.v)) cx.report('E3019', `comprehension variable ${cl.v} shadows an enclosing name`);
        c2.vars.set(cl.v, vt);
        for (const f of cl.filters) {
          requireVal(c2, f, infer(c2, f), 'as a filter');
          c2 = applyGuards(c2, guardsOf(f, true));
        }
      }
      const k = requireVal(c2, e.key, infer(c2, e.key), 'as a map key');
      if (k.rt && numKind(k.rt) !== 'string') cx.report('E4001', 'map-comprehension key is not a string');
      checkExpr(c2, e.val, expected.val);
      return { rt: expected, abs: false };
    }
    case 'paren': return checkExpr(cx, e.x, expected);
    case 'if': {
      const c = requireVal(cx, e.c, infer(cx, e.c), 'as a condition');
      if (c.rt && !isBoolish(c.rt)) cx.report('E4001', '`if` condition is not bool');
      checkExpr(applyGuards(cx, guardsOf(e.c, true)), e.t, expected);
      checkExpr(applyGuards(cx, guardsOf(e.c, false)), e.f, expected);
      return { rt: expected, abs: false };
    }
    case 'match': return inferMatch(cx, e, expected);
    case 'obj': {
      if (expected.t === 'rec') {
        // entries see the record's members (siblings + inherited scope chain)
        const cxR = child(cx);
        for (const m of expected.members) {
          const mt: RT | null = m.conj ? { t: 'isectN', arms: m.conj } : (m.type ?? null);
          cxR.vars.set(m.name, { rt: mt, abs: m.kind === 'opt' });
        }
        for (const en of e.entries) {
          const m = expected.members.find((m: any) => m.name === en.key);
          if (!m) {
            if (!expected.open) cx.report('E4003', `member ${en.key} is not declared on ${expected.name ?? 'the record'}`);
            requireVal(cxR, en.val, infer(cxR, en.val), 'as a construction member');
            continue;
          }
          const mt: RT | null = m.conj ? { t: 'isectN', arms: m.conj } : (m.type ?? null);
          requireVal(cxR, en.val, checkExpr(cxR, en.val, mt), 'as a construction member');
        }
        for (const m of expected.members)
          if (m.kind === 'req' && !e.entries.some(en => en.key === m.name))
            cx.report('E4002', `required member ${m.name} missing in the construction`);
        return { rt: expected, abs: false };
      }
      if (expected.t === 'map') {
        for (const en of e.entries) requireVal(cx, en.val, checkExpr(cx, en.val, expected.val), 'as a map value');
        return { rt: expected, abs: false };
      }
      if (expected.t === 'union') { infer(cx, e); return { rt: expected, abs: false }; }   // discriminated at binding
      infer(cx, e);
      cx.report('E4001', `object literal where ${expected.t} is expected`);
      return { rt: expected, abs: false };
    }
    case 'arr': {
      if (expected.t === 'arr') {
        for (const it of e.items) {
          if (it.spread) { requireVal(cx, it.expr, infer(cx, it.expr), 'as a spread'); continue; }
          requireVal(cx, it.expr, checkExpr(cx, it.expr, expected.elem), 'as an array element');
        }
        return { rt: expected, abs: false };
      }
      break;
    }
    case 'lambda':
      if (expected.t === 'func') { checkLambda(cx, e, expected); return { rt: expected, abs: false }; }
      break;
  }
  const ty = requireVal(cx, e, infer(cx, e), 'as a value');
  if (ty.rt && !subsumes(cx.env, ty.rt, expected) && !deferrable(ty.rt, expected))
    cx.report('E4001', `expression type does not satisfy the expected type`);
  return ty;
}

// a same-kind refinement target (pattern, range, literal set) whose
// membership the static type cannot prove is validated at binding, not
// rejected here — the corpus (guide, benchmarks) relies on this split;
// kind-level mismatches still fail statically
function deferrable(s: RT, t: RT): boolean {
  const k = numKind(s);
  if (!k) return false;
  if (t.t === 'pattern') return k === 'string';
  if (t.t === 'range') return k === t.base;
  if (t.t === 'lit') return k === numKind(t);
  if (t.t === 'union') return t.arms.some((a: RT) => deferrable(s, a));
  if (t.t === 'pred') return deferrable(s, t.base);
  return false;
}
