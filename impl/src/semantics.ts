// Value model, environment, type resolution (reference implementation;
// promoted from the Phase 0 spike, adapted to the tree-sitter AST).
import type { Decl, Expr, MemberAst, TypeAst, ElseTail, TemplateParts } from './ast.ts';
import { subsumes } from './subsume.ts';

// ---------------- values ----------------
export const ABSENT = Symbol('absent');
export type Seg = string | number;
export type Value = any; // bigint | number | string | boolean | null | QuantityV | RefV | RecInst | ArrV | MapV | RangeV | ClosureV

export const isQ = (v: any) => v && v.__q === true;
export const isRef = (v: any) => v && v.__ref === true;
export const isRec = (v: any): v is RecInst => v && v.__rec === true;
export const isArr = (v: any) => v && v.__arr === true;
export const isMap = (v: any) => v && v.__map === true;
export const isRange = (v: any) => v && v.__range === true;
export const isClo = (v: any) => v && v.__clo === true;

export type Slot = {
  kind: 'req' | 'opt' | 'dflt' | 'der';
  state: 'unforced' | 'forcing' | 'ok' | 'invalid' | 'absent';
  value?: Value;
  deferred: boolean;           // expression mentions $referrers
  compute?: () => Value;       // throws Taint/EvalError
  suppliedDerived?: any;       // raw restatement to verify
};
export type RecInst = {
  __rec: true;
  typeName?: string;
  rt: any;                     // resolved record type
  path: Seg[];
  parent: RecInst | null;
  slots: Map<string, Slot>;
  entryOrder: string[];        // supplied order (document/literal), incl. open extras
  extras: Map<string, any>;    // open pass-through (opaque)
};

export class Taint extends Error { constructor() { super('taint'); } }
export class DeferSig extends Error { constructor() { super('defer'); } }
export class EvalErr extends Error { constructor(msg: string) { super(msg); } }

export type Diag = { severity: string; id?: string; message: string; path: string; code?: string };

// ---------------- units (SI subset) ----------------
const UNITS: Record<string, { dim: string; factor: number }> = {
  s: { dim: 'Time', factor: 1 }, ms: { dim: 'Time', factor: 1e-3 },
  us: { dim: 'Time', factor: 1e-6 }, ns: { dim: 'Time', factor: 1e-9 },
};
const BASE_UNIT: Record<string, string> = { Time: 's' };

// ---------------- resolved types ----------------
export type RT = any;

export class Env {
  typeAsts = new Map<string, { ast: TypeAst; tail?: ElseTail; params?: any[] }>();
  typeMemo = new Map<string, RT>();
  consts = new Map<string, { expr: Expr; type?: TypeAst; value?: Value; state: string }>();
  funcs = new Map<string, { params: { name: string; type?: TypeAst }[]; ret?: TypeAst; body: Expr }>();
  duplicates: string[] = [];
  outputs: { name: string; type: TypeAst; expr: Expr }[] = [];
  inputs = new Map<string, { type: TypeAst; fallback?: Expr }>();
  diags = new Map<string, { params: { name: string }[]; severity: string; template: TemplateParts }>();
  registry: RecInst[] = [];
  roots = new Map<string, Value>();
  diagnostics: Diag[] = [];
  constEval?: (name: string) => any;   // installed by the Engine (§4.13)
  onConstDiag?: (d: Diag) => void;     // installed by the checker
  private constDiagSeen = new Set<string>();

  load(decls: Decl[]) {
    const seen = new Set<string>();
    const claim = (n: string) => { if (seen.has(n)) this.duplicates.push(n); seen.add(n); };
    for (const d of decls) {
      if ('name' in d && typeof (d as any).name === 'string') claim((d as any).name);
      if (d.d === 'type') this.typeAsts.set(d.name, { ast: d.type, tail: d.tail, params: (d as any).params });
      else if (d.d === 'const') this.consts.set(d.name, { expr: d.expr, type: d.type, state: 'unforced' });
      else if (d.d === 'func') this.funcs.set(d.name, { params: d.params, ret: d.ret, body: d.body });
      else if (d.d === 'output') this.outputs.push(d);
      else if (d.d === 'input') this.inputs.set(d.name, { type: d.type, fallback: d.fallback });
      else if (d.d === 'diagnostic') this.diags.set(d.name, d as any);
    }
  }

  report(d: Diag) { this.diagnostics.push(d); }

  // §4.13: a named endpoint in a constant position evaluates at
  // elaboration time; an erroring constant is a compile-time diagnostic
  constNum(v: any): any {
    if (typeof v !== 'string' || !this.constEval || !this.consts.has(v)) return v;
    const diag = (code: string, message: string) => {
      if (this.constDiagSeen.has(v + code)) return;
      this.constDiagSeen.add(v + code);
      const d: Diag = { severity: 'error', code, message, path: '' };
      this.onConstDiag ? this.onConstDiag(d) : this.report(d);
    };
    try {
      const r = this.constEval(v);
      if (typeof r === 'bigint' || typeof r === 'number') return r;
      if (r !== undefined) diag('E4021', `constant ${v} is not numeric in a constant position`);
      return v;
    } catch (e: any) {
      if (e instanceof EvalErr) {
        const code = /zero/.test(e.message) ? 'E5001' : /NaN|Infinity/.test(e.message) ? 'E5002' : 'E5001';
        diag(code, `evaluating constant ${v}: ${e.message}`);
      }
      return v;
    }
  }

  resolve(ast: TypeAst, name?: string): RT {
    switch (ast.k) {
      case 'prim': return { t: 'prim', name: ast.name };
      case 'lit': return { t: 'lit', v: ast.v };
      case 'range': {
        const lo = this.constNum(ast.lo), hi = this.constNum(ast.hi);
        const isF = typeof lo === 'number' || typeof hi === 'number';
        return { t: 'range', lo, hi, excl: ast.excl, base: isF ? 'float' : 'int' };
      }
      case 'pattern': return { t: 'pattern', src: ast.re, re: new RegExp(`^(?:${ast.re})$`) };
      case 'map': return { t: 'map', key: this.resolve(ast.key), val: this.resolve(ast.val) };
      case 'array': {
        const lo = this.constNum(ast.lo), hi0 = this.constNum(ast.hi);
        const hi = ast.excl && typeof hi0 !== 'string' && hi0 !== undefined ? Number(hi0) - 1 : hi0;
        return { t: 'arr', elem: this.resolve(ast.elem), lo: typeof lo === 'bigint' ? Number(lo) : lo, hi: typeof hi === 'bigint' ? Number(hi) : hi };
      }
      case 'union': return { t: 'union', arms: ast.arms.map(a => this.resolve(a)) };
      case 'isect': {
        const arms = ast.arms.map(a => this.resolve(a));
        if (arms.every(a => a.t === 'rec')) return this.mergeIsect(arms, name);
        return { t: 'isectN', arms };
      }
      case 'record': {
        const rt: any = { t: 'rec', name, members: [], asserts: [], open: ast.open, tail: undefined };
        this.fillRecord(rt, ast.members);
        return rt;
      }
      case 'named': {
        if ((ast as any).preds && (ast as any).preds.length) {
          const base = this.resolve({ ...(ast as any), preds: undefined }, name);
          return { t: 'pred', base, preds: (ast as any).preds };
        }
        if (ast.name === 'quantity') return { t: 'quantity', dim: (ast.args[0] as any).name };
        if (ast.name === 'map' && ast.args.length === 2)
          return { t: 'map', key: this.resolve(ast.args[0]), val: this.resolve(ast.args[1]) };
        if (ast.name === 'ref') return { t: 'ref', target: this.resolve(ast.args[0]) };
        if (['int', 'float', 'bool', 'string'].includes(ast.name) && !ast.args.length && !ast.ext)
          return { t: 'prim', name: ast.name };
        const decl = this.typeAsts.get(ast.name);
        if (!decl) throw new Error(`unknown type ${ast.name}`);
        let base: RT;
        if (decl.params?.length) base = this.instantiate(ast, decl);
        else if (this.typeMemo.has(ast.name)) base = this.typeMemo.get(ast.name);
        else {
          if (decl.ast.k === 'record') {
            base = { t: 'rec', name: ast.name, members: [], asserts: [], open: decl.ast.open, tail: decl.tail };
            this.typeMemo.set(ast.name, base);
            this.fillRecord(base, decl.ast.members);
          } else {
            base = this.resolve(decl.ast, ast.name);
            if (base.t === 'rec' || base.t === 'union') base.name = ast.name;
            base.tail = base.tail ?? decl.tail;
            this.typeMemo.set(ast.name, base);
          }
        }
        if (ast.ext) {
          const ext = this.resolve(ast.ext) as any;      // anonymous record of overrides
          const merged: any = { t: 'rec', name: base.name, open: base.open, tail: base.tail,
            members: base.members.map((m: any) => ({ ...m })), asserts: [...base.asserts] };
          for (const om of ext.members) {
            const i = merged.members.findIndex((m: any) => m.name === om.name);
            if (i >= 0) merged.members[i] = om; else merged.members.push(om);
          }
          merged.asserts.push(...ext.asserts);
          return merged;
        }
        return base;
      }
    }
  }
  // §3.15: substitute arguments, check value arguments against their
  // parameter types (the parameter's type IS its constraint, D14), and
  // resolve the substituted body — structural after substitution
  instantiate(ast: TypeAst & { k: 'named' }, decl: { ast: TypeAst; tail?: ElseTail; params?: any[] }): RT {
    const ps = decl.params!;
    if (ast.args.length !== ps.length)
      throw new Error(`generic arity: ${ast.name} expects ${ps.length} argument(s), got ${ast.args.length}`);
    const types = new Map<string, TypeAst>();
    const values = new Map<string, any>();
    const label: string[] = [];
    for (let i = 0; i < ps.length; i++) {
      const p = ps[i], a = ast.args[i];
      if (p.type) {                       // value parameter
        let v: any;
        if (a.k === 'lit') v = a.v;
        else if (a.k === 'named' && !a.args.length && !a.ext && !a.preds) {
          v = this.constNum(a.name);
          if (typeof v === 'string')
            throw new Error(`non-constant value argument ${a.name} for ${p.name} of ${ast.name}`);
        } else throw new Error(`generic arity: parameter ${p.name} of ${ast.name} takes a constant value`);
        const bound = this.resolve(this.substType(p.type, types, values));
        if (!subsumes(this, { t: 'lit', v }, bound))
          throw new Error(`value argument ${v} outside parameter ${p.name}'s type in ${ast.name}`);
        values.set(p.name, v);
        label.push(String(v));
      } else {
        types.set(p.name, a);
        label.push(a.k === 'named' ? a.name : a.k === 'prim' ? (a as any).name : a.k);
      }
    }
    const bigStr = (_k: string, v: any) => typeof v === 'bigint' ? `${v}n` : v;
    const key = `${ast.name}<${JSON.stringify(ast.args, bigStr)}>`;
    if (this.typeMemo.has(key)) return this.typeMemo.get(key);
    const shown = `${ast.name}<${label.join(', ')}>`;
    const body = this.substType(decl.ast, types, values);
    let rt: RT;
    if (body.k === 'record') {
      rt = { t: 'rec', name: shown, members: [], asserts: [], open: body.open, tail: decl.tail };
      this.typeMemo.set(key, rt);
      this.fillRecord(rt, body.members);
    } else {
      rt = this.resolve(body, shown);
      if (rt.t === 'rec' || rt.t === 'union') rt.name = shown;
      rt.tail = rt.tail ?? decl.tail;
      this.typeMemo.set(key, rt);
    }
    return rt;
  }
  substType(ast: TypeAst, types: Map<string, TypeAst>, values: Map<string, any>): TypeAst {
    const t = (a: TypeAst): TypeAst => this.substType(a, types, values);
    switch (ast.k) {
      case 'named': {
        const plain = !ast.args.length && !ast.ext && !ast.preds;
        if (plain && types.has(ast.name)) return types.get(ast.name)!;
        if (plain && values.has(ast.name)) return { k: 'lit', v: values.get(ast.name) };
        return { ...ast, args: ast.args.map(t), ext: ast.ext ? t(ast.ext) : undefined,
                 preds: ast.preds?.map(p => substExpr(p, values)) };
      }
      case 'range': {
        const sub = (v: any) => typeof v === 'string' && values.has(v) ? values.get(v) : v;
        return { ...ast, lo: sub(ast.lo), hi: sub(ast.hi) };
      }
      case 'array': {
        const sub = (v: any) => typeof v === 'string' && values.has(v) ? Number(values.get(v)) : v;
        return { ...ast, elem: t(ast.elem), lo: sub(ast.lo), hi: sub(ast.hi) };
      }
      case 'record': return { ...ast, members: ast.members.map(m => this.substMember(m, types, values)) };
      case 'map': return { ...ast, key: t(ast.key), val: t(ast.val) };
      case 'union': case 'isect': return { ...ast, arms: ast.arms.map(t) };
      case 'func': return { ...ast, params: ast.params.map(t), ret: t(ast.ret) };
      default: return ast;
    }
  }
  substMember(m: MemberAst, types: Map<string, TypeAst>, values: Map<string, any>): MemberAst {
    const t = (a: TypeAst): TypeAst => this.substType(a, types, values);
    switch (m.m) {
      case 'value': return { ...m, type: t(m.type), dflt: m.dflt ? substExpr(m.dflt, values) : undefined };
      case 'derived': return { ...m, type: m.type ? t(m.type) : undefined, expr: substExpr(m.expr, values) };
      case 'context': return { ...m, type: t(m.type) };
      case 'assert': return { ...m, cond: substExpr(m.cond, values) };
      case 'when': return { ...m, cond: substExpr(m.cond, values), body: m.body.map(b => this.substMember(b, types, values)) };
    }
  }

  fillRecord(rt: any, members: MemberAst[]) {
    for (const m of members) {
      if (m.m === 'value')
        rt.members.push({ kind: m.dflt ? 'dflt' : m.opt ? 'opt' : 'req', name: m.name, type: this.resolve(m.type), dflt: m.dflt });
      else if (m.m === 'derived')
        rt.members.push({ kind: 'der', name: m.name, type: m.type ? this.resolve(m.type) : undefined, expr: m.expr });
      else if (m.m === 'assert')
        rt.asserts.push({ kind: 'assert', name: m.name, cond: m.cond, tail: m.tail, origin: rt.name });
      else if (m.m === 'when')
        rt.asserts.push({ kind: 'when', cond: m.cond, body: m.body, origin: rt.name });
      else if (m.m === 'context') { /* checked statically (D30); no runtime slot */ }
    }
  }
  mergeIsect(arms: RT[], name?: string): RT {
    const recs = arms.filter(a => a.t === 'rec');
    if (recs.length !== arms.length) return { t: 'isectN', arms };
    const merged: any = { t: 'rec', name, open: recs.every(r => r.open), tail: undefined, members: [], asserts: [] };
    for (const r of recs) {
      for (const m of r.members) {
        const i = merged.members.findIndex((x: any) => x.name === m.name);
        if (i >= 0) merged.members[i] = { ...merged.members[i], conj: [...(merged.members[i].conj ?? [merged.members[i].type]), m.type], kind: m.kind === 'req' ? 'req' : merged.members[i].kind };
        else merged.members.push({ ...m });
      }
      merged.asserts.push(...r.asserts.map((a: any) => ({ ...a, origin: a.origin ?? r.name })));
    }
    return merged;
  }
}

// ---------------- helpers ----------------
// deep-copy an expression substituting generic value parameters (§3.15)
export function substExpr(e: Expr, values: Map<string, any>): Expr {
  if (!e || typeof e !== 'object') return e;
  if ((e as any).e === 'name' && values.has((e as any).name))
    return { e: 'lit', v: values.get((e as any).name) };
  const out: any = {};
  for (const [k, v] of Object.entries(e)) {
    if (Array.isArray(v)) out[k] = v.map(x => x && typeof x === 'object' ? substExpr(x, values) : x);
    else if (v && typeof v === 'object') out[k] = substExpr(v as any, values);
    else out[k] = v;
  }
  return out;
}

const idRe = /^[_A-Za-z][_A-Za-z0-9]*$/;
export function pathStr(segs: Seg[], relRoot?: string): string {
  let out = '';
  segs.forEach((s, i) => {
    if (i === 0) { out += relRoot !== undefined && s === relRoot ? '$' : String(s); return; }
    if (typeof s === 'number') out += `[${s}]`;
    else if (idRe.test(s)) out += `.${s}`;
    else out += `[${JSON.stringify(s)}]`;
  });
  return out;
}
export function parsePath(s: string, rootName: string): Seg[] {
  const segs: Seg[] = [];
  let i = 0;
  if (s[0] === '$') { segs.push(rootName); i = 1; }
  else { let m = /^[_A-Za-z][_A-Za-z0-9]*/.exec(s); if (!m) throw new EvalErr(`bad path ${s}`); segs.push(m[0]); i = m[0].length; }
  while (i < s.length) {
    if (s[i] === '.') {
      const m = /^[_A-Za-z][_A-Za-z0-9]*/.exec(s.slice(i + 1));
      if (!m) throw new EvalErr(`bad path ${s}`);
      segs.push(m[0]); i += 1 + m[0].length;
    } else if (s[i] === '[') {
      const j = s.indexOf(']', i);
      const inner = s.slice(i + 1, j);
      segs.push(inner.startsWith('"') ? JSON.parse(inner) : Number(inner));
      i = j + 1;
    } else throw new EvalErr(`bad path ${s}`);
  }
  return segs;
}
export function cmpPath(a: Seg[], b: Seg[]): number {
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    const x = a[i], y = b[i];
    if (typeof x === 'number' && typeof y === 'number') { if (x !== y) return x - y; }
    else if (String(x) !== String(y)) return String(x) < String(y) ? -1 : 1;
  }
  return a.length - b.length;
}

function placeOf(v: any): Seg[] | null {
  if (isRef(v)) return v.segs;
  if (isRec(v) || isArr(v) || isMap(v)) return v.path;
  return null;
}
export function valueEq(a: any, b: any): boolean {
  const pa = placeOf(a), pb = placeOf(b);
  if ((isRef(a) || isRef(b)) && pa && pb) return cmpPath(pa, pb) === 0;
  if (typeof a === 'bigint' && typeof b === 'bigint') return a === b;
  if (typeof a === 'number' && typeof b === 'number') return a === b;
  if (isQ(a) && isQ(b)) return a.dim === b.dim && a.value === b.value;
  if (isArr(a) && isArr(b)) return a.items.length === b.items.length && a.items.every((x: any, i: number) => valueEq(x, b.items[i]));
  if (isMap(a) && isMap(b)) {
    if (a.entries.size !== b.entries.size) return false;
    for (const [k, v] of a.entries) { if (!b.entries.has(k) || !valueEq(v, b.entries.get(k))) return false; }
    return true;
  }
  if (isRec(a) && isRec(b)) {
    for (const [n, s] of a.slots) {
      const s2 = b.slots.get(n);
      const v1 = s.state === 'absent' ? ABSENT : s.value;
      const v2 = !s2 || s2.state === 'absent' ? ABSENT : s2.value;
      if (v1 === ABSENT && v2 === ABSENT) continue;
      if (v1 === ABSENT || v2 === ABSENT) return false;
      if (!valueEq(v1, v2)) return false;
    }
    return true;
  }
  return a === b;
}

const mentionsReferrers = (e: any): boolean => {
  if (!e || typeof e !== 'object') return false;
  if (e.e === 'referrers') return true;
  return Object.values(e).some(v => Array.isArray(v) ? v.some(mentionsReferrers) : mentionsReferrers(v));
};

// ---------------- lexical JSON (int/float by lexeme) ----------------
export function readJson(src: string): any {
  let i = 0;
  const ws = () => { while (/[\s]/.test(src[i])) i++; };
  function val(): any {
    ws();
    const c = src[i];
    if (c === '{') {
      i++; const o: any = { __jobj: true, entries: [] as [string, any][] };
      ws();
      if (src[i] === '}') { i++; return o; }
      for (;;) {
        ws(); const k = str(); ws(); i++; // ':'
        o.entries.push([k, val()]); ws();
        if (src[i] === ',') { i++; continue; }
        i++; return o;
      }
    }
    if (c === '[') {
      i++; const a: any[] = []; ws();
      if (src[i] === ']') { i++; return a; }
      for (;;) { a.push(val()); ws(); if (src[i] === ',') { i++; continue; } i++; return a; }
    }
    if (c === '"') return str();
    if (src.startsWith('true', i)) { i += 4; return true; }
    if (src.startsWith('false', i)) { i += 5; return false; }
    if (src.startsWith('null', i)) { i += 4; return null; }
    let m = /^-?(?:0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?/.exec(src.slice(i))!;
    i += m[0].length;
    return m[1] || m[2] ? parseFloat(m[0]) : BigInt(m[0]);
  }
  function str(): string {
    let j = i + 1, s = '';
    while (src[j] !== '"') {
      if (src[j] === '\\') {
        const e = src[j + 1];
        s += e === 'n' ? '\n' : e === 't' ? '\t' : e === 'u' ? String.fromCharCode(parseInt(src.slice(j + 2, j + 6), 16)) : e;
        j += e === 'u' ? 6 : 2;
      } else { s += src[j]; j++; }
    }
    i = j + 1; return s;
  }
  const v = val(); ws();
  return v;
}
