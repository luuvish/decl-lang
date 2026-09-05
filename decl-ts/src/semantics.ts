// Value model, environment, type resolution (reference implementation;
// promoted from the Phase 0 spike, adapted to the tree-sitter AST).
import type { Decl, Expr, MemberAst, TypeAst, ElseTail, TemplateParts, Loc } from './ast.ts';
import { subsumes } from './subsume.ts';

// ---------------- values ----------------
export const ABSENT = Symbol('absent');
// a path segment (§7.2): a record member by name, an array index, or a
// map key — kept apart from a member because the canonical text differs:
// a map key is always bracketed, a member is dotted when the dot can
// spell it
export type Seg = string | number | { key: string };
export const mapKey = (k: string): Seg => ({ key: k });
export const segText = (s: Seg): string | number => typeof s === 'object' ? s.key : s;
// dot-spellable (§3.11, §4.3): identifier-shaped and not a literal keyword
export const dotSpellable = (name: string): boolean =>
  /^[_A-Za-z][_A-Za-z0-9]*$/.test(name) && !['true', 'false', 'null'].includes(name);
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
  hidden?: boolean;            // `x$ = e`: computed, never part of the value (D34)
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
export class EvalErr extends Error {
  code?: string;
  constructor(msg: string, code?: string) { super(msg); this.code = code; }
}

export type Diag = { severity: string; id?: string; message: string; path: string; code?: string;
  /** the source range the checker reported at (the declaration, or the expression under inference) */
  loc?: Loc;
  /** the evaluation step that produced it (a slot, a root, an assert): dependency tracking's tag */
  by?: string };

/** §6.7: evaluation- and validation-time diagnostics sort by (path, id), path in canonical order; stable */
export function sortDiags(diags: Diag[]): Diag[] {
  const segsOf = (p: string): Seg[] => { try { return p ? parsePath(p, '') : []; } catch { return [p]; } };
  return diags.map((d, i) => ({ d, i, segs: segsOf(d.path) }))
    .sort((a, b) => cmpPath(a.segs, b.segs) || ((a.d.id ?? '') < (b.d.id ?? '') ? -1 : (a.d.id ?? '') > (b.d.id ?? '') ? 1 : 0) || a.i - b.i)
    .map(x => x.d);
}

// ---------------- dimensions as exponent vectors (§3.16) ----------------
// two dimension expressions are equal iff their base-exponent vectors
// are; the normalized key string is the canonical identity
export type DimVec = Record<string, number>;
export function keyOfVec(v: DimVec): string {
  return Object.entries(v).filter(([, e]) => e !== 0)
    .sort(([a], [b]) => a < b ? -1 : 1)
    .map(([n, e]) => e === 1 ? n : `${n}^${e}`)
    .join('*');   // '' = dimensionless
}
export function vecOfKey(key: string): DimVec {
  const v: DimVec = {};
  if (!key) return v;
  for (const p of key.split('*')) {
    const [n, e] = p.split('^');
    v[n] = (v[n] ?? 0) + (e ? Number(e) : 1);
  }
  return v;
}
export function vecCombine(a: DimVec, b: DimVec, sign: 1 | -1): DimVec {
  const out = { ...a };
  for (const [n, e] of Object.entries(b)) out[n] = (out[n] ?? 0) + sign * e;
  return out;
}

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
  exprEval?: (e: Expr) => any;         // installed by the Engine (unit factors)
  // module linking (§8.3): local name -> the exporting module + original
  // name; namespace name -> that module's export surface
  imports = new Map<string, { env: Env; name: string }>();
  namespaces = new Map<string, { env: Env; exports: Map<string, { env: Env; name: string }> }>();
  onConstDiag?: (d: Diag) => void;     // installed by the checker
  private constDiagSeen = new Set<string>();

  // ---- unit / dimension name spaces (§3.16; separate from values/types) ----
  dimDecls = new Map<string, { terms?: { name: string; exp: number }[] }>();
  dimMemo = new Map<string, DimVec>();
  unitDecls = new Map<string, { dim?: string; factor?: Expr; base?: string }>();
  unitMemo = new Map<string, { key: string; toBase: number }>();
  baseUnitOf = new Map<string, string>();   // dim key -> base unit symbol
  spaceDiags: Diag[] = [];                  // unit/dimension-space findings for the checker

  constructor() {
    // std.units — the full SI catalog as ordinary declarations (D15,
    // §13.10); the prefix generation rule below IS the inventory
    const dim = (name: string, terms?: { name: string; exp: number }[]) => this.dimDecls.set(name, { terms });
    const unit = (sym: string, decl: { dim?: string; factor?: number; base?: string }) => {
      if (this.unitDecls.has(sym)) return;   // base/named symbols win over generated ones
      this.unitDecls.set(sym, decl.dim !== undefined ? { dim: decl.dim }
        : { factor: { e: 'lit', v: decl.factor! }, base: decl.base! });
    };
    const bases: [string, string][] = [
      ['Time', 's'], ['Length', 'm'], ['Mass', 'kg'], ['Current', 'A'],
      ['Temperature', 'K'], ['Amount', 'mol'], ['LuminousIntensity', 'cd'],
    ];
    for (const [d] of bases) dim(d);
    const t = (name: string, exp: number) => ({ name, exp });
    const derived: [string, { name: string; exp: number }[], string][] = [
      ['Frequency', [t('Time', -1)], 'Hz'],
      ['Force', [t('Mass', 1), t('Length', 1), t('Time', -2)], 'N'],
      ['Pressure', [t('Mass', 1), t('Length', -1), t('Time', -2)], 'Pa'],
      ['Energy', [t('Mass', 1), t('Length', 2), t('Time', -2)], 'J'],
      ['Power', [t('Mass', 1), t('Length', 2), t('Time', -3)], 'W'],
      ['Charge', [t('Current', 1), t('Time', 1)], 'C'],
      ['Voltage', [t('Mass', 1), t('Length', 2), t('Time', -3), t('Current', -1)], 'V'],
      ['Resistance', [t('Mass', 1), t('Length', 2), t('Time', -3), t('Current', -2)], 'Ohm'],
      ['Capacitance', [t('Mass', -1), t('Length', -2), t('Time', 4), t('Current', 2)], 'F'],
      ['DataSize', undefined as any, 'bit'],
    ];
    for (const [d, terms] of derived) dim(d, terms);
    for (const [, s] of bases) unit(s, { dim: bases.find(b => b[1] === s)![0] });
    for (const [d, , s] of derived) unit(s, { dim: d });
    unit('B', { factor: 8, base: 'bit' });
    unit('g', { factor: 1e-3, base: 'kg' });
    const PREFIXES: [string, number][] = [
      ['y', 1e-24], ['z', 1e-21], ['a', 1e-18], ['f', 1e-15], ['p', 1e-12],
      ['n', 1e-9], ['u', 1e-6], ['m', 1e-3], ['c', 1e-2], ['d', 1e-1],
      ['da', 1e1], ['h', 1e2], ['k', 1e3], ['M', 1e6], ['G', 1e9],
      ['T', 1e12], ['P', 1e15], ['E', 1e18], ['Z', 1e21], ['Y', 1e24],
    ];
    const prefixable = [...bases.map(([, s]) => s).filter(s => s !== 'kg'),
      ...derived.map(([, , s]) => s).filter(s => s !== 'bit'), 'g'];
    for (const u0 of prefixable) for (const [p, f] of PREFIXES) unit(p + u0, { factor: f, base: u0 });
    for (const u0 of ['bit', 'B']) {
      for (const [p, f] of [['Ki', 1024], ['Mi', 1024 ** 2], ['Gi', 1024 ** 3], ['Ti', 1024 ** 4], ['Pi', 1024 ** 5], ['Ei', 1024 ** 6]] as [string, number][])
        unit(p + u0, { factor: f, base: u0 });
      for (const [p, f] of PREFIXES.filter(([p]) => ['k', 'M', 'G', 'T', 'P', 'E'].includes(p)))
        unit(p + u0, { factor: f, base: u0 });
    }
  }

  resolveDim(name: string, visiting = new Set<string>()): DimVec {
    if (this.dimMemo.has(name)) return this.dimMemo.get(name)!;
    if (visiting.has(name)) throw new Error(`circular dimension ${name}`);
    const d = this.dimDecls.get(name);
    if (!d) throw new Error(`unknown dimension ${name}`);
    let vec: DimVec = {};
    if (!d.terms) vec = { [name]: 1 };
    else {
      visiting.add(name);
      for (const t of d.terms) {
        const sub = this.resolveDim(t.name, visiting);
        for (const [n, e] of Object.entries(sub)) vec[n] = (vec[n] ?? 0) + e * t.exp;
      }
      visiting.delete(name);
    }
    this.dimMemo.set(name, vec);
    return vec;
  }
  unitInfo(sym: string, visiting = new Set<string>()): { key: string; toBase: number } {
    if (this.unitMemo.has(sym)) return this.unitMemo.get(sym)!;
    if (visiting.has(sym)) throw new Error(`circular unit ${sym}`);
    const u = this.unitDecls.get(sym);
    if (!u) throw new Error(`unknown unit ${sym}`);
    let info: { key: string; toBase: number };
    if (u.dim !== undefined) {
      const key = keyOfVec(this.resolveDim(u.dim));
      if (!this.baseUnitOf.has(key)) this.baseUnitOf.set(key, sym);
      info = { key, toBase: 1 };
    } else {
      visiting.add(sym);
      const b = this.unitInfo(u.base!, visiting);
      visiting.delete(sym);
      let f: any = u.factor && (u.factor as any).e === 'lit' ? (u.factor as any).v : undefined;
      if (f === undefined && u.factor && this.exprEval) { try { f = this.exprEval(u.factor); } catch { } }
      if (typeof f === 'bigint') f = Number(f);
      if (typeof f !== 'number') throw new Error(`unit ${sym}: factor is not a numeric constant`);
      info = { key: b.key, toBase: f * b.toBase };
    }
    this.unitMemo.set(sym, info);
    return info;
  }
  // full-space validation for the checker: redeclarations, second base
  // units, unresolvable factors/dimensions (E4073 and friends)
  finalizeUnitSpace(): Diag[] {
    const out: Diag[] = [...this.spaceDiags];
    const baseSeen = new Map<string, string>();
    for (const [sym, u] of this.unitDecls) {
      try {
        const info = this.unitInfo(sym);
        if (u.dim !== undefined) {
          const prev = baseSeen.get(info.key);
          if (prev) out.push({ severity: 'error', code: 'E4073', message: `second base unit ${sym} for dimension ${info.key} (base is ${prev})`, path: '' });
          else baseSeen.set(info.key, sym);
        }
      } catch (e: any) {
        const code = /unknown dimension|circular dimension/.test(e.message) ? 'E3003' : 'E4073';
        out.push({ severity: 'error', code, message: e.message, path: '' });
      }
    }
    return out;
  }

  load(decls: Decl[]) {
    const seen = new Set<string>();
    const claim = (n: string) => { if (seen.has(n)) this.duplicates.push(n); seen.add(n); };
    for (const d of decls) {
      // units and dimensions live in their own name spaces (§3.16)
      if ('name' in d && typeof (d as any).name === 'string' && d.d !== 'unit' && d.d !== 'dimension')
        claim((d as any).name);
      if (d.d === 'dimension') {
        if (this.dimDecls.has(d.name))
          this.spaceDiags.push({ severity: 'error', code: 'E3001', message: `dimension ${d.name} redeclared`, path: '' });
        else this.dimDecls.set(d.name, { terms: d.terms });
      } else if (d.d === 'unit') {
        if (this.unitDecls.has(d.name))
          this.spaceDiags.push({ severity: 'error', code: 'E4073', message: `unit ${d.name} redeclared`, path: '' });
        else this.unitDecls.set(d.name, { dim: d.dim, factor: d.factor, base: d.base });
      }
      if (d.d === 'type') this.typeAsts.set(d.name, { ast: d.type, tail: d.tail, params: (d as any).params });
      else if (d.d === 'const') this.consts.set(d.name, { expr: d.expr, type: d.type, state: 'unforced' });
      else if (d.d === 'func') this.funcs.set(d.name, { params: d.params, ret: d.ret, body: d.body });
      else if (d.d === 'output') this.outputs.push(d);
      else if (d.d === 'input') this.inputs.set(d.name, { type: d.type, fallback: d.fallback });
      else if (d.d === 'diagnostic') this.diags.set(d.name, d as any);
    }
  }

  /** installed by the engine: the evaluation step a report is attributed to */
  tagger?: () => string | undefined;
  report(d: Diag) { const by = this.tagger?.(); if (by !== undefined) d.by = by; this.diagnostics.push(d); }

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
      case 'pattern': {
        const src = this.expandPattern(ast.re);
        const bad = patternError(src);
        if (bad) throw new Error(`malformed pattern /${ast.re}/: ${bad}`);
        return { t: 'pattern', src, re: compilePattern(src) };
      }
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
        if (ast.name === 'quantity')
          return { t: 'quantity', dim: keyOfVec(this.resolveDim((ast.args[0] as any).name)) };
        if (ast.name === 'map' && ast.args.length === 2)
          return { t: 'map', key: this.resolve(ast.args[0]), val: this.resolve(ast.args[1]) };
        if (ast.name === 'ref') return { t: 'ref', target: this.resolve(ast.args[0]) };
        if (['int', 'float', 'bool', 'string'].includes(ast.name) && !ast.args.length && !ast.ext)
          return { t: 'prim', name: ast.name };
        let decl = this.typeAsts.get(ast.name);
        if (!decl) {
          const im = this.imports.get(ast.name);
          if (im) return im.env.resolve({ ...ast, name: im.name }, name);
          if (ast.name.includes('.')) {
            const [ns, ...rest] = ast.name.split('.');
            const ex = this.namespaces.get(ns)?.exports.get(rest.join('.'));
            if (ex) return ex.env.resolve({ ...ast, name: ex.name }, name);
          }
        }
        if (!decl) throw new Error(`unknown type ${ast.name}`);
        let base: RT;
        if (decl.params?.length) base = this.instantiate(ast, decl);
        else if (this.typeMemo.has(ast.name)) base = this.typeMemo.get(ast.name);
        else if (decl.ast.k === 'record') {
          base = { t: 'rec', name: ast.name, members: [], asserts: [], open: decl.ast.open, tail: decl.tail };
          this.typeMemo.set(ast.name, base);
          // a member that fails to resolve must not leave a half-filled
          // record memoized (later lookups would miss its later members)
          try { this.fillRecord(base, decl.ast.members); }
          catch (e) { this.typeMemo.delete(ast.name); throw e; }
        } else if (decl.ast.k === 'named' && decl.ast.ext) {
          // an extension declaration (§3.14) is memoized before its parent
          // resolves: in a recursive family — `type Base = { kids: { [string]:
          // Kid } }`, `type Kid = Base { … }` — the parent's body names this
          // type, and every reference must share the one final record rather
          // than a snapshot of the parent's members taken mid-fill
          base = { t: 'rec', name: ast.name, members: [], asserts: [], tail: decl.tail, filling: true };
          this.typeMemo.set(ast.name, base);
          try {
            const parent = this.resolve({ ...decl.ast, ext: undefined });
            this.extendInto(base, parent, this.resolve(decl.ast.ext));
          } catch (e) { this.typeMemo.delete(ast.name); throw e; }
        } else {
          base = this.resolve(decl.ast, ast.name);
          if (base.t === 'rec' || base.t === 'union') base.name = ast.name;
          base.tail = base.tail ?? decl.tail;
          this.typeMemo.set(ast.name, base);
        }
        if (ast.ext) {
          // an inline extension in a type position: anonymous, never memoized
          const merged: any = { t: 'rec', name: base.name, members: [], asserts: [], filling: true };
          this.extendInto(merged, base, this.resolve(ast.ext));
          return merged;
        }
        return base;
      }
    }
  }
  // §3.14: fill `target` as `base` extended by the override body `ext` —
  // base members copied, overrides replacing or adding, asserts appended,
  // and a context declaration narrowed by the extension replacing the
  // inherited one (§7.3). A base still being filled (the recursive-family
  // case above) defers the merge until it completes; `target` stays marked
  // filling meanwhile, so an extension of an extension waits in turn.
  extendInto(target: any, base: RT, ext: RT) {
    const b: any = base, e: any = ext;
    if (b.t !== 'rec') { Object.assign(target, b, { filling: false }); return; }
    if (b.filling) { (b.pendingExts ??= []).push({ target, ext }); return; }
    target.open = b.open;
    target.tail = b.tail ?? target.tail;
    const ctxDecls = [...(b.ctxDecls ?? [])];
    for (const cd of e.ctxDecls ?? []) {
      const i = ctxDecls.findIndex((x: any) => x.variable === cd.variable);
      if (i >= 0) ctxDecls[i] = cd; else ctxDecls.push(cd);
    }
    target.ctxDecls = ctxDecls.length ? ctxDecls : undefined;
    target.members = b.members.map((m: any) => ({ ...m }));
    for (const om of e.members) {
      const i = target.members.findIndex((m: any) => m.name === om.name);
      if (i >= 0) target.members[i] = om; else target.members.push(om);
    }
    target.asserts = [...b.asserts, ...e.asserts];
    this.completeRecord(target);
  }
  // a record's members are final: extensions that waited on it merge now
  completeRecord(rt: any) {
    rt.filling = false;
    const pending = rt.pendingExts ?? [];
    rt.pendingExts = undefined;
    for (const p of pending) this.extendInto(p.target, rt, p.ext);
  }
  // §3.6: `${T}` inside a pattern splices another type — a string-shaped
  // T (pattern, string literal, union of those) as its regular language,
  // an integer-shaped T (int literal, int range, union) as the decimal
  // representations of its members
  // names being spliced right now, across nested resolutions — a
  // mutually recursive pair (`/x${B}/`, `/y${A}/`) is a cycle, not a stack overflow
  patternVisiting = new Set<string>();
  expandPattern(re: string): string {
    const visiting = this.patternVisiting;
    return re.replace(/\$\{([^}]*)\}/g, (_, inner: string) => {
      const text = inner.trim();
      // the spliced type: a union of string literals, int literals, int
      // ranges, and named types — the type-expression subset that fits
      // inside a pattern token
      const arms = text.split('|').map(a => a.trim());
      const frags = arms.map(arm => {
        let m: RegExpExecArray | null;
        if ((m = /^"((?:[^"\\]|\\.)*)"$/.exec(arm))) return this.patternFragment({ t: 'lit', v: JSON.parse(`"${m[1]}"`) }, text);
        if ((m = /^(-?[0-9]+)\.\.(<?)(-?[0-9]+)$/.exec(arm)))
          return this.patternFragment({ t: 'range', base: 'int', lo: BigInt(m[1]), hi: BigInt(m[3]), excl: m[2] === '<' }, text);
        if (/^-?[0-9]+$/.test(arm)) return this.patternFragment({ t: 'lit', v: BigInt(arm) }, text);
        if (!/^[A-Za-z_][A-Za-z0-9_.]*$/.test(arm)) throw new Error(`pattern interpolation of ${text}: not a type (§3.6)`);
        if (visiting.has(arm)) throw new Error(`pattern interpolation of ${arm} is circular`);
        visiting.add(arm);
        let rt: RT;
        try { rt = this.resolve({ k: 'named', name: arm, args: [] }); }
        catch (e: any) {
          visiting.delete(arm);
          if (/^unknown type/.test(e.message)) throw new Error(`pattern interpolation of ${arm}: unknown type`);
          throw e;
        }
        visiting.delete(arm);
        return this.patternFragment(rt, arm);
      });
      return frags.length === 1 ? frags[0] : `(?:${frags.join('|')})`;
    });
  }
  patternFragment(rt: RT, name: string): string {
    const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\\/]/g, '\\$&');
    const bad = (): never => { throw new Error(`pattern interpolation of ${name}: type is neither string- nor integer-shaped (§3.6)`); };
    switch (rt.t) {
      case 'pattern': return `(?:${rt.src})`;
      case 'lit':
        if (typeof rt.v === 'string') return esc(rt.v);
        if (typeof rt.v === 'bigint') return rt.v.toString();
        return bad();
      case 'range': {
        if (rt.base !== 'int' || typeof rt.lo !== 'bigint' || typeof rt.hi !== 'bigint') return bad();
        const hi = rt.excl ? rt.hi - 1n : rt.hi;
        if (hi - rt.lo >= 65536n) throw new Error(`pattern interpolation of ${name}: range too large (limit 65536 values)`);
        const alts: string[] = [];
        for (let v = rt.lo; v <= hi; v++) alts.push(v.toString());
        return `(?:${alts.join('|')})`;
      }
      case 'union': return `(?:${rt.arms.map((a: RT) => this.patternFragment(a, name)).join('|')})`;
      case 'pred': return this.patternFragment(rt.base, name);
      case 'prim': return rt.name === 'string' ? '.*' : rt.name === 'int' ? '-?[0-9]+' : bad();
      default: return bad();
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
      try { this.fillRecord(rt, body.members); }
      catch (e) { this.typeMemo.delete(key); throw e; }
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
    // member expressions and asserts evaluate in their declaring
    // module's scope (§8.3) — carry it on each entry
    rt.filling = true;
    for (const m of members) {
      if (m.m === 'value')
        rt.members.push({ kind: m.dflt ? 'dflt' : m.opt ? 'opt' : 'req', name: m.name, type: this.resolve(m.type), dflt: m.dflt, menv: this });
      else if (m.m === 'derived')
        rt.members.push({ kind: 'der', name: m.name, type: m.type ? this.resolve(m.type) : undefined, expr: m.expr, menv: this, hidden: m.hidden || undefined });
      else if (m.m === 'assert')
        rt.asserts.push({ kind: 'assert', name: m.name, cond: m.cond, tail: m.tail, origin: rt.name, menv: this });
      else if (m.m === 'when')
        rt.asserts.push({ kind: 'when', cond: m.cond, body: m.body, origin: rt.name, menv: this });
      else if (m.m === 'context')
        (rt.ctxDecls ??= []).push({ variable: m.variable, type: this.resolve(m.type), menv: this });
    }
    this.completeRecord(rt);
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

// ---------------- patterns: the portable core (§3.6) ----------------
// A pattern body is validated against the specification's regular-
// expression core — character literals and escapes, classes, `.`,
// alternation, grouping, the repetition forms `* + ? {m} {m,} {m,n}`, and
// the class escapes `\d \w \s` with their negations — with one fixed set of
// messages, so every implementation reports the same text whatever
// regular-expression engine it runs the accepted patterns on.
// Returns the reason a body is outside the core, or null when it is inside.
const PATTERN_PUNCT = '\\/.*+?()[]{}|^$-';
export function patternError(src: string): string | null {
  const n = src.length;
  let i = 0, depth = 0, canRepeat = false;
  // one escape: `\` at i; returns the code point it stands for (classes
  // need it for ranges) or a reason
  const escape = (): { cp: number } | { err: string } => {
    if (i + 1 >= n) return { err: 'trailing backslash' };
    const e = src[i + 1];
    i += 2;
    if ('dwsDWS'.includes(e)) return { cp: -1 };
    if (e === 'n') return { cp: 10 };
    if (e === 't') return { cp: 9 };
    if (e === 'r') return { cp: 13 };
    if (PATTERN_PUNCT.includes(e)) return { cp: e.codePointAt(0)! };
    if (/[0-9]/.test(e)) return { err: `backreference \\${e} is not supported` };
    return { err: `unsupported escape \\${e}` };
  };
  while (i < n) {
    const c = src[i];
    switch (c) {
      case '\\': {
        const r = escape();
        if ('err' in r) return r.err;
        canRepeat = true;
        break;
      }
      case '[': {
        i++;
        if (src[i] === '^') i++;
        let items = 0;
        const item = (): { cp: number } | { err: string } => {
          if (src[i] === '\\') return escape();
          return { cp: src.codePointAt(i++)! };
        };
        for (;;) {
          if (i >= n) return 'unterminated character class';
          if (src[i] === ']') { i++; break; }
          const lo = item();
          if ('err' in lo) return lo.err;
          if (src[i] === '-' && i + 1 < n && src[i + 1] !== ']') {
            i++;
            const hi = item();
            if ('err' in hi) return hi.err;
            if (lo.cp < 0 || hi.cp < 0 || lo.cp > hi.cp) return 'invalid range in character class';
          }
          items++;
        }
        if (items === 0) return 'empty character class';
        canRepeat = true;
        break;
      }
      case ']': return 'unbalanced bracket';
      case '(':
        i++;
        if (src[i] === '?') {
          if (src[i + 1] === ':') i += 2;
          else return 'unsupported construct (?';
        }
        depth++;
        canRepeat = false;
        break;
      case ')':
        if (depth === 0) return 'unbalanced parenthesis';
        depth--; i++;
        canRepeat = true;
        break;
      case '|': i++; canRepeat = false; break;
      case '*': case '+': case '?':
        if (!canRepeat) return 'nothing to repeat';
        i++;
        canRepeat = false;
        break;
      case '{': {
        if (!canRepeat) return 'nothing to repeat';
        const m = /^\{([0-9]+)(?:(,)([0-9]*))?\}/.exec(src.slice(i));
        if (!m) return 'malformed repetition';
        if (m[3] && BigInt(m[3]) < BigInt(m[1])) return 'malformed repetition';
        i += m[0].length;
        canRepeat = false;
        break;
      }
      case '}': return 'malformed repetition';
      case '^': case '$': i++; canRepeat = false; break;
      default: i++; canRepeat = true;
    }
  }
  return depth > 0 ? 'unbalanced parenthesis' : null;
}
export function compilePattern(src: string): RegExp {
  return new RegExp(`^(?:${src})$`);
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

export function pathStr(segs: Seg[], relRoot?: string): string {
  let out = '';
  segs.forEach((s, i) => {
    if (i === 0) { out += relRoot !== undefined && s === relRoot ? '$' : String(segText(s)); return; }
    if (typeof s === 'number') out += `[${s}]`;
    else if (typeof s === 'object') out += `[${JSON.stringify(s.key)}]`;
    else if (dotSpellable(s)) out += `.${s}`;
    else out += `[${JSON.stringify(s)}]`;
  });
  return out;
}
// a path string from a document: `.name` is a member, `["…"]` a bracketed
// segment (a map key, or a member the dot cannot spell — the canonical
// walk, §7.5, decides which is legal where), `[n]` an index
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
      segs.push(inner.startsWith('"') ? mapKey(JSON.parse(inner)) : Number(inner));
      i = j + 1;
    } else throw new EvalErr(`bad path ${s}`);
  }
  return segs;
}
// canonical path order (§7.2): segment-wise, indices numerically, names
// and keys lexicographically, a prefix first
export function cmpPath(a: Seg[], b: Seg[]): number {
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    const x = segText(a[i]), y = segText(b[i]);
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
      if (s.hidden) continue;                 // a hidden member is not part of the value (D34)
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
    let m = /^-?(?:0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?/.exec(src.slice(i));
    if (!m) throw new EvalErr(`bad JSON at ${i}`);
    i += m[0].length;
    return m[1] || m[2] ? parseFloat(m[0]) : BigInt(m[0]);
  }
  function str(): string {
    if (src[i] !== '"') throw new EvalErr(`bad JSON at ${i}`);
    let j = i + 1, s = '';
    while (src[j] !== '"') {
      if (j >= src.length) throw new EvalErr(`bad JSON at ${j}`);
      if (src[j] === '\\') {
        const e = src[j + 1];
        s += e === 'n' ? '\n' : e === 't' ? '\t' : e === 'u' ? String.fromCharCode(parseInt(src.slice(j + 2, j + 6), 16)) : e;
        j += e === 'u' ? 6 : 2;
      } else { s += src[j]; j++; }
    }
    i = j + 1; return s;
  }
  const v = val(); ws();
  if (i < src.length) throw new EvalErr('bad JSON: trailing characters');
  return v;
}
