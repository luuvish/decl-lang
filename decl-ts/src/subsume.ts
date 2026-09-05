// The subsumption judgment ⊑ (spec §3.17) — one normative, total
// judgment behind assignability, narrowing, discrimination, and
// intersection compatibility (D13). Coinductive on recursive records.
import type { Env, RT } from './semantics.ts';
import { Engine } from './engine.ts';

type Assume = Map<RT, Set<RT>>;

export function subsumes(env: Env, a: RT, b: RT, assume: Assume = new Map()): boolean {
  if (a === b) return true;

  // coinductive assumption for recursive records
  if (a.t === 'rec' && b.t === 'rec') {
    const set = assume.get(a);
    if (set?.has(b)) return true;
  }

  // unions / intersections first (structural set rules)
  if (a.t === 'union') return a.arms.every((x: RT) => subsumes(env, x, b, assume));
  if (b.t === 'union') return b.arms.some((x: RT) => subsumes(env, a, x, assume));
  if (a.t === 'isectN') return a.arms.some((x: RT) => subsumes(env, x, b, assume));
  if (b.t === 'isectN') return b.arms.every((x: RT) => subsumes(env, a, x, assume));

  // predicate refinements: T(F') ⊑ T(F) iff F' ⊇ F under identity; T(F) ⊑ T
  if (b.t === 'pred') {
    if (a.t === 'pred') {
      return (
        subsumes(env, a.base, b.base, assume) &&
        b.preds.every((p: any) => a.preds.some((q: any) => predEq(p, q)))
      );
    }
    if (a.t === 'lit') return litSatisfies(env, a.v, b); // decidable by evaluation
    return false;
  }
  if (a.t === 'pred') return subsumes(env, a.base, b, assume);

  switch (b.t) {
    case 'prim':
      if (a.t === 'prim') return a.name === b.name;
      if (a.t === 'lit') return litKind(a.v) === b.name;
      if (a.t === 'range')
        return (
          a.base === b.name ||
          (b.name === 'int' && a.base === 'int') ||
          (b.name === 'float' && a.base === 'float')
        );
      if (a.t === 'pattern') return b.name === 'string';
      return false;
    case 'lit':
      return a.t === 'lit' && valEq(a.v, b.v);
    case 'range': {
      if (a.t === 'lit') return litKind(a.v) === b.base && inRange(a.v, b);
      if (a.t === 'range') {
        if (a.base !== b.base) return false;
        const aHi = a.excl ? dec(a.hi) : a.hi,
          bHi = b.excl ? dec(b.hi) : b.hi;
        return a.lo >= b.lo && aHi <= bHi;
      }
      return false;
    }
    case 'pattern':
      if (a.t === 'lit' && typeof a.v === 'string') return new RegExp(`^(?:${b.src})$`).test(a.v);
      if (a.t === 'pattern') return a.src === b.src; // normalized-text identity only
      return false;
    case 'arr': {
      if (a.t !== 'arr') return false;
      if (!subsumes(env, a.elem, b.elem, assume)) return false;
      const aLo = a.lo ?? 0,
        aHi = a.hi ?? Infinity;
      const bLo = b.lo ?? 0,
        bHi = b.hi ?? Infinity;
      return aLo >= bLo && aHi <= bHi;
    }
    case 'map':
      return (
        a.t === 'map' && subsumes(env, a.key, b.key, assume) && subsumes(env, a.val, b.val, assume)
      );
    case 'quantity':
      return a.t === 'quantity' && a.dim === b.dim;
    case 'ref':
      return a.t === 'ref' && subsumes(env, a.target, b.target, assume);
    case 'func': {
      if (a.t !== 'func' || a.params.length !== b.params.length) return false;
      return (
        b.params.every((bp: RT, i: number) => subsumes(env, bp, a.params[i], assume)) && // contravariant
        subsumes(env, a.ret, b.ret, assume)
      ); // covariant
    }
    case 'rec': {
      if (a.t !== 'rec') return false;
      let set = assume.get(a);
      if (!set) {
        set = new Set();
        assume.set(a, set);
      }
      set.add(b);
      for (const m of b.members) {
        if (m.hidden) continue; // not part of the value: ⊑ never compares it (D34)
        const s = a.members.find((x: any) => x.name === m.name);
        const mTypes: RT[] = m.conj ?? (m.type ? [m.type] : []);
        const sTypes: RT[] = s ? (s.conj ?? (s.type ? [s.type] : [])) : [];
        const typeOk = () =>
          mTypes.length === 0 ||
          sTypes.length === 0 ||
          mTypes.every((mt) => sTypes.some((st) => subsumes(env, st, mt, assume)));
        switch (m.kind) {
          case 'req':
            if (!s || s.kind === 'opt') {
              set.delete(b);
              return false;
            }
            if (!typeOk()) {
              set.delete(b);
              return false;
            }
            break;
          case 'opt':
          case 'dflt':
            if (s && !typeOk()) {
              set.delete(b);
              return false;
            }
            break;
          case 'der':
            if (!s || !typeOk()) {
              set.delete(b);
              return false;
            }
            break;
        }
      }
      return true;
    }
  }
  return false;
}

function litKind(v: any): string {
  if (typeof v === 'bigint') return 'int';
  if (typeof v === 'number') return 'float';
  if (typeof v === 'string') return 'string';
  if (typeof v === 'boolean') return 'bool';
  if (v === null) return 'null';
  return 'unknown';
}
function valEq(a: any, b: any): boolean {
  return typeof a === typeof b && a === b;
}
function inRange(v: any, r: any): boolean {
  const hi = r.excl ? dec(r.hi) : r.hi;
  return v >= r.lo && v <= hi;
}
function dec(v: any): any {
  return typeof v === 'bigint' ? v - 1n : v - 1;
}

// predicate identity: same function reference, equal constant arguments
function predEq(a: any, b: any): boolean {
  if (a?.e === 'name' && b?.e === 'name') return a.name === b.name;
  if (a?.e === 'call' && b?.e === 'call') {
    return (
      predEq(a.fn, b.fn) &&
      a.args.length === b.args.length &&
      a.args.every(
        (x: any, i: number) => x.e === 'lit' && b.args[i].e === 'lit' && valEq(x.v, b.args[i].v),
      )
    );
  }
  return false;
}

// literal ⊑ predicate type: run the predicate on the constant (§3.17)
function litSatisfies(env: Env, v: any, pred: any): boolean {
  try {
    const eng = new Engine(env);
    const sc = { inst: null, locals: new Map<string, any>(), rootName: '' };
    if (!subsumes(env, { t: 'lit', v }, pred.base)) return false;
    for (const p of pred.preds) {
      const fn = eng.ev(p, sc);
      if (eng.call(fn, [v], sc) !== true) return false;
    }
    return true;
  } catch {
    return false;
  }
}

// ---------------- structural emptiness (§3.19, D12) ----------------
export function structurallyEmpty(env: Env, t: RT): boolean {
  if (t.t === 'range') {
    const hi = t.excl ? dec(t.hi) : t.hi;
    return t.lo > hi;
  }
  if (t.t === 'arr') return t.lo !== undefined && t.hi !== undefined && t.lo > t.hi;
  if (t.t === 'isectN') {
    const arms: RT[] = t.arms;
    for (let i = 0; i < arms.length; i++)
      for (let j = i + 1; j < arms.length; j++) if (disjoint(env, arms[i], arms[j])) return true;
    return arms.some((a) => structurallyEmpty(env, a));
  }
  if (t.t === 'union') return t.arms.every((a: RT) => structurallyEmpty(env, a));
  return false;
}
function kindOf(t: RT): string | null {
  if (t.t === 'prim') return t.name;
  if (t.t === 'lit') return litKind(t.v);
  if (t.t === 'range') return t.base;
  if (t.t === 'pattern') return 'string';
  if (t.t === 'arr') return 'array';
  if (t.t === 'rec' || t.t === 'map') return 'object';
  if (t.t === 'quantity') return 'object';
  return null;
}
function disjoint(env: Env, a: RT, b: RT): boolean {
  const ka = kindOf(a),
    kb = kindOf(b);
  if (ka && kb && ka !== kb) return true;
  if (a.t === 'range' && b.t === 'range' && a.base === b.base) {
    const aHi = a.excl ? dec(a.hi) : a.hi,
      bHi = b.excl ? dec(b.hi) : b.hi;
    return a.lo > bHi || b.lo > aHi;
  }
  if (a.t === 'lit' && b.t === 'lit') return !valEq(a.v, b.v);
  if (a.t === 'lit' && b.t === 'range') return !(litKind(a.v) === b.base && inRange(a.v, b));
  if (a.t === 'range' && b.t === 'lit') return disjoint(env, b, a);
  return false;
}
