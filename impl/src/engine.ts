// Binding, evaluation, validation, serialization (reference implementation;
// promoted from the Phase 0 spike, adapted to the tree-sitter AST).
import {
  ABSENT, DeferSig, Env, EvalErr, Taint,
  cmpPath, isArr, isClo, isMap, isQ, isRange, isRec, isRef, parsePath, pathStr, valueEq,
} from './semantics.ts';
import type { Diag, RecInst, RT, Seg, Slot } from './semantics.ts';
import type { Expr, TemplateParts } from './ast.ts';
import { subsumes } from './subsume.ts';

const UNITS: Record<string, { dim: string; factor: number }> = {
  s: { dim: 'Time', factor: 1 }, ms: { dim: 'Time', factor: 1e-3 },
  us: { dim: 'Time', factor: 1e-6 }, ns: { dim: 'Time', factor: 1e-9 },
};
const BASE_UNIT: Record<string, string> = { Time: 's' };

type Scope = { inst: RecInst | null; locals: Map<string, any>; rootName: string };
type Pre = { __pre: 'obj'; entries: [string, PreVal][] } | { __pre: 'arr'; items: ({ spread: boolean; v: PreVal })[] };
type PreVal = { __expr: Expr; scope: Scope } | any;

export class Engine {
  env: Env;
  deferredSlots: { inst: RecInst; name: string }[] = [];
  noReg = 0;                     // >0: binding for comparison only — do not register instances
  phase = 1;                     // 1: materialization; 2: universe complete, $referrers answers
  constructor(env: Env) { this.env = env; }

  // ---------- expression evaluation ----------
  ev(e: Expr, sc: Scope): any {
    switch (e.e) {
      case 'lit': return e.v;
      case 'unitlit': {
        const u = UNITS[e.unit]; if (!u) throw new EvalErr(`unknown unit ${e.unit}`);
        return { __q: true, dim: u.dim, value: e.num * u.factor };
      }
      case 'paren': return this.ev(e.x, sc);
      case 'mapcomp': {
        const entries: [string, any][] = [];
        const rec = (ci: number, locals: Map<string, any>) => {
          if (ci === e.clauses.length) {
            const k = this.ev(e.key, { ...sc, locals });
            if (typeof k !== 'string') throw new EvalErr('map key must be string');
            if (entries.some(([kk]) => kk === k)) throw new EvalErr(`duplicate key ${k}`);
            entries.push([k, this.ev(e.val, { ...sc, locals })]);
            return;
          }
          const cl = e.clauses[ci];
          for (const el of this.iterate(this.ev(cl.iter, { ...sc, locals }))) {
            const l2 = new Map(locals); l2.set(cl.v, el);
            if (cl.filters.every((f: Expr) => this.truthy(this.ev(f, { ...sc, locals: l2 })))) rec(ci + 1, l2);
          }
        };
        rec(0, sc.locals);
        return { __pre: 'obj', entries };
      }
      case 'template': {
        let s = '';
        for (const p of e.parts) s += typeof p === 'string' ? p : this.toStr(this.ev(p, sc));
        return s;
      }
      case 'name': {
        if (sc.locals.has(e.name)) return sc.locals.get(e.name);
        if (sc.inst) {
          const v = this.slotLookup(sc.inst, e.name);
          if (v !== undefined) return v;
        }
        if (this.env.consts.has(e.name)) return this.forceConst(e.name, sc.rootName);
        if (this.env.funcs.has(e.name)) {
          const f = this.env.funcs.get(e.name)!;
          return { __clo: true, params: f.params.map(p => p.name), body: f.body,
                   scope: { inst: null, locals: new Map(), rootName: sc.rootName } };
        }
        if (e.name === 'std') return { __std: true, path: [] };
        if (this.env.roots.has(e.name)) return this.env.roots.get(e.name);
        throw new EvalErr(`unknown name ${e.name}`);
      }
      case 'ctx': {
        if (e.name === '$this') return sc.inst;
        if (e.name === '$parent') return sc.inst?.parent;
        if (e.name === '$path') return pathStr(sc.inst!.path);
        throw new EvalErr(`unsupported context var ${e.name}`);
      }
      case 'referrers': return this.referrers(e.type, e.member, sc);
      case 'obj': return { __pre: 'obj', entries: e.entries.map(en => [en.key, { __expr: en.val, scope: sc }]) };
      case 'arr': return { __pre: 'arr', items: e.items.map(it => ({ spread: it.spread, v: { __expr: it.expr, scope: sc } })) };
      case 'comp': {
        const items: any[] = [];
        const rec = (ci: number, locals: Map<string, any>) => {
          if (ci === e.clauses.length) { items.push({ spread: false, v: { __expr: e.head, scope: { ...sc, locals } } }); return; }
          const cl = e.clauses[ci];
          const iter = this.ev(cl.iter, { ...sc, locals });
          const elems = this.iterate(iter);
          for (const el of elems) {
            const l2 = new Map(locals); l2.set(cl.v, el);
            if (cl.filters.every(f => this.truthy(this.ev(f, { ...sc, locals: l2 })))) rec(ci + 1, l2);
          }
        };
        rec(0, sc.locals);
        return { __pre: 'arr', items };
      }
      case 'if': return this.truthy(this.ev(e.c, sc)) ? this.ev(e.t, sc) : this.ev(e.f, sc);
      case 'match': {
        const subj = this.deref(this.ev(e.subject, sc));
        const run = (arm: { v: string; body: Expr }) => {
          const l2 = new Map(sc.locals); l2.set(arm.v, subj);
          return this.ev(arm.body, { ...sc, locals: l2 });
        };
        let catchAll: { v: string; body: Expr } | null = null;
        for (const arm of e.arms) {
          if (!arm.type) { catchAll = arm; continue; }
          if (this.memberOf(subj, this.env.resolve(arm.type), sc)) return run(arm);
        }
        if (catchAll) return run(catchAll);
        throw new EvalErr('match: no arm matched');
      }
      case 'lambda': return { __clo: true, params: e.params, body: e.body, scope: sc };
      case 'un': {
        const x = this.ev(e.x, sc);
        if (e.op === '!') return !this.truthy(x);
        if (e.op === '-') return typeof x === 'bigint' ? -x : -x;
        if (e.op === '~') return ~(x as bigint);
        throw new EvalErr('un');
      }
      case 'bin': {
        if (e.op === '|>') {   // first-argument insertion (§4.9)
          const call: Expr = e.r.e === 'call'
            ? { e: 'call', fn: e.r.fn, args: [e.l, ...e.r.args] }
            : { e: 'call', fn: e.r, args: [e.l] };
          return this.ev(call, sc);
        }
        return this.binop(e.op, e.l, e.r, sc);
      }
      case 'member': {
        const x0 = this.ev(e.x, sc);
        if (e.safe && (x0 === null || x0 === ABSENT)) return ABSENT;
        const x = this.deref(x0);
        return this.access(x, e.name);
      }
      case 'index': {
        const x = this.deref(this.ev(e.x, sc));
        const i = this.ev(e.i, sc);
        if (isArr(x)) {
          const n = Number(i);
          if (n < 0 || n >= x.items.length) throw new EvalErr(`index ${n} out of bounds`);
          return x.items[n];
        }
        if (isMap(x)) return x.entries.has(i) ? x.entries.get(i) : ABSENT;
        if (isRec(x)) return this.access(x, i);
        throw new EvalErr('index on non-collection');
      }
      case 'call': {
        const args = e.args.map(a => this.ev(a, sc));
        const fn = this.evCallee(e.fn, sc);
        return this.call(fn, args, sc);
      }
      case 'with': {
        const base = this.deref(this.ev(e.base, sc));
        if (!isRec(base)) throw new EvalErr('with on non-record');
        const patch = this.ev(e.patch, sc);
        const entries: [string, any][] = [];
        for (const n of base.entryOrder) {
          if (base.extras.has(n)) { entries.push([n, base.extras.get(n)]); continue; }
          const s = base.slots.get(n)!;
          if (s.kind === 'der') continue;                    // derived: dropped, recomputed downstream
          if (s.state === 'absent') continue;
          entries.push([n, this.forceSlot(base, n)]);
        }
        for (const m of base.rt.members) {                   // defaulted members not in entryOrder
          if (m.kind === 'dflt' && !entries.some(([k]) => k === m.name) && base.slots.get(m.name)?.state !== 'absent')
            entries.push([m.name, this.forceSlot(base, m.name)]);
        }
        for (const [k, v] of (patch as any).entries) {
          const i = entries.findIndex(([n]) => n === k);
          if (i >= 0) entries[i] = [k, v]; else entries.push([k, v]);
        }
        return { __pre: 'obj', entries };
      }
    }
  }
  // does a value belong to a type? (match arm selection) — bound records
  // answer by their bound type; raw values by a silent trial binding
  memberOf(v: any, rt: RT, sc: Scope): boolean {
    if (isRec(v)) return subsumes(this.env, v.rt, rt);
    const mark = this.env.diagnostics.length;
    this.noReg++;
    try { this.bind(v, rt, ['<match>'], null, sc); return true; }
    catch { return false; }
    finally { this.noReg--; this.env.diagnostics.length = mark; }
  }
  evCallee(e: Expr, sc: Scope): any {
    if (e.e === 'member') {
      const x = this.evCallee(e.x, sc);
      if (x && x.__std) return { __std: true, path: [...x.path, e.name] };
      return this.access(this.deref(x), e.name);
    }
    return this.ev(e, sc);
  }
  iterate(v: any): any[] {
    if (v && v.__pre) return this.matArr(v);
    if (isArr(v)) return v.items;
    if (isRange(v)) {
      const out: any[] = [];
      for (let i = v.lo; i < v.hi + (v.excl ? 0n : 1n); i++) out.push(i);
      return out;
    }
    if (Array.isArray((v as any)?.items)) return (v as any).items;
    throw new EvalErr('not iterable');
  }
  truthy(v: any): boolean {
    if (typeof v === 'boolean') return v;
    throw new EvalErr('non-bool condition');
  }
  binop(op: string, le: Expr, re: Expr, sc: Scope): any {
    if (op === '&&') return this.truthy(this.ev(le, sc)) ? this.truthy(this.ev(re, sc)) : false;
    if (op === '||') return this.truthy(this.ev(le, sc)) ? true : this.truthy(this.ev(re, sc));
    if (op === '??') { const l = this.ev(le, sc); return l === ABSENT || l === null ? this.ev(re, sc) : l; }
    const l = this.ev(le, sc), r = this.ev(re, sc);
    if (op === '..' || op === '..<') return { __range: true, lo: l, hi: r, excl: op === '..<' };
    if (op === '==') return valueEq(l, r);
    if (op === '!=') return !valueEq(l, r);
    if (op === 'in') {
      if (isRange(r)) return l >= r.lo && (r.excl ? l < r.hi : l <= r.hi);
      if (r && r.__pre) return this.matArr(r).some((x: any) => valueEq(l, x));
      if (isArr(r)) return r.items.some((x: any) => valueEq(l, x));
      if (isMap(r)) return r.entries.has(l);
      if (isRec(r)) { const s = r.slots.get(l); return !!s && this.forceState(r, l) !== 'absent'; }
      throw new EvalErr('in: bad container');
    }
    if (l === ABSENT || r === ABSENT) throw new EvalErr('absent consumed');
    const bothI = typeof l === 'bigint' && typeof r === 'bigint';
    const bothF = typeof l === 'number' && typeof r === 'number';
    const bothS = typeof l === 'string' && typeof r === 'string';
    switch (op) {
      case '+': if (bothS) return l + r; if (bothI || bothF) return (l as any) + (r as any); break;
      case '-': if (bothI || bothF) return (l as any) - (r as any); break;
      case '*': if (bothI || bothF) return (l as any) * (r as any); break;
      case '/':
        if (bothI) { if (r === 0n) throw new EvalErr('division by zero'); const q = (l as bigint) / (r as bigint); return q; }
        if (bothF) { if (r === 0) throw new EvalErr('division by zero'); const q = (l as number) / (r as number); if (!isFinite(q)) throw new EvalErr('non-finite'); return q; }
        break;
      case '%': if (bothI) { if (r === 0n) throw new EvalErr('mod zero'); return (l as bigint) % (r as bigint); } break;
      case '<': case '<=': case '>': case '>=': {
        if (bothI || bothF || bothS) {
          if (op === '<') return l < r; if (op === '<=') return l <= r;
          if (op === '>') return l > r; return l >= r;
        }
        if (isQ(l) && isQ(r) && l.dim === r.dim) {
          if (op === '<') return l.value < r.value; if (op === '<=') return l.value <= r.value;
          if (op === '>') return l.value > r.value; return l.value >= r.value;
        }
        break;
      }
      case '&': if (bothI) return (l as bigint) & (r as bigint); break;
      case '|': if (bothI) return (l as bigint) | (r as bigint); break;
      case '^': if (bothI) return (l as bigint) ^ (r as bigint); break;
      case '<<': if (bothI) return (l as bigint) << (r as bigint); break;
      case '>>': if (bothI) return (l as bigint) >> (r as bigint); break;
    }
    throw new EvalErr(`bad operands for ${op}`);
  }
  toStr(v: any): string {
    if (typeof v === 'string') return v;
    if (typeof v === 'bigint') return v.toString();
    if (typeof v === 'number') return String(v);
    if (typeof v === 'boolean') return String(v);
    throw new EvalErr('template: non-convertible');
  }
  deref(v: any): any {
    if (isRef(v)) {
      const target = this.resolveSegs(v.segs);
      if (target === undefined) throw new EvalErr(`dangling reference ${pathStr(v.segs)}`);
      return target;
    }
    return v;
  }
  resolveSegs(segs: Seg[]): any {
    let cur: any = this.env.roots.get(segs[0] as string);
    for (let i = 1; i < segs.length && cur !== undefined; i++) {
      const s = segs[i];
      if (isRec(cur)) cur = this.forceSlot(cur, s as string);
      else if (isArr(cur)) cur = cur.items[s as number];
      else if (isMap(cur)) cur = cur.entries.get(s);
      else cur = undefined;
      if (isRef(cur)) cur = this.deref(cur);
    }
    return cur;
  }
  access(x: any, name: string): any {
    if (isRec(x)) {
      if (x.slots.has(name)) {
        const st = this.forceState(x, name);
        if (st === 'absent') return ABSENT;
        return this.forceSlot(x, name);
      }
      if (x.extras.has(name)) throw new EvalErr(`opaque field ${name} accessed`);
      throw new EvalErr(`no member ${name}`);
    }
    if (x === null) throw new EvalErr('member access on null');
    if (x === ABSENT) return ABSENT;
    throw new EvalErr(`member access on non-record (${name})`);
  }
  slotLookup(inst: RecInst, name: string): any {
    // nearest-enclosing-instance-first: walk the ownership chain
    for (let cur: RecInst | null = inst; cur; cur = cur.parent) {
      if (cur.slots.has(name)) {
        const st = this.forceState(cur, name);
        return st === 'absent' ? ABSENT : this.forceSlot(cur, name);
      }
    }
    return undefined;
  }
  call(fn: any, args: any[], sc: Scope): any {
    if (isClo(fn)) {
      const locals = new Map(fn.scope.locals);
      fn.params.forEach((p: string, i: number) => locals.set(p, args[i]));
      return this.ev(fn.body, { ...fn.scope, locals });
    }
    if (fn && fn.__std) return this.std(fn.path.join('.'), args, sc);
    throw new EvalErr('call of non-function');
  }
  std(name: string, a: any[], sc: Scope): any {
    switch (name) {
      case 'array.count': return BigInt(this.matArr(a[0]).length);
      case 'array.all': return this.matArr(a[0]).every(x => this.truthy(this.call(a[1], [x], sc)));
      case 'array.any': return this.matArr(a[0]).some(x => this.truthy(this.call(a[1], [x], sc)));
      case 'array.filter': return { __arr: true, items: this.matArr(a[0]).filter(x => this.truthy(this.call(a[1], [x], sc))), path: [] };
      case 'array.all_distinct': {
        const items = this.matArr(a[0]);
        for (let i = 0; i < items.length; i++) for (let j = i + 1; j < items.length; j++)
          if (valueEq(items[i], items[j])) return false;
        return true;
      }
      case 'array.sum': {
        const items = this.matArr(a[0]);
        if (items.length === 0) return 0n;
        return items.reduce((acc: any, x: any) => acc + x, typeof items[0] === 'number' ? 0 : 0n);
      }
      case 'array.fold': return this.matArr(a[0]).reduce((acc, x) => this.call(a[2], [acc, x], sc), a[1]);
      case 'map.keys': return { __arr: true, items: [...this.matMap(a[0]).entries.keys()], path: [] };
      case 'map.values': return { __arr: true, items: [...this.matMap(a[0]).entries.values()], path: [] };
      case 'string.length': return BigInt([...a[0]].length);
      case 'string.of': return this.toStr(a[0]);
      case 'string.join': return this.matArr(a[0]).join(a[1]);
      case 'string.starts_with': return (a[0] as string).startsWith(a[1] as string);
      case 'ref.path': {
        if (!isRef(a[0])) throw new EvalErr('ref.path on non-reference');
        return pathStr(a[0].segs);
      }
      case 'math.abs': return typeof a[0] === 'bigint' ? (a[0] < 0n ? -a[0] : a[0]) : Math.abs(a[0]);
      case 'math.min': return a[0] < a[1] ? a[0] : a[1];
      case 'math.max': return a[0] > a[1] ? a[0] : a[1];
      default: throw new EvalErr(`std.${name} not in spike`);
    }
  }
  matArr(v: any): any[] {
    let d = this.deref(v);
    if (d && d.__pre) d = this.materialize(d, [], null, null as any);
    if (isArr(d)) return d.items;
    throw new EvalErr('expected array');
  }
  matMap(v: any): any {
    let d = this.deref(v);
    if (d && d.__pre) d = this.materialize(d, [], null, null as any);
    if (isMap(d)) return d;
    throw new EvalErr('expected map');
  }

  // ---------- referrers ----------
  referrers(typeName: string, member: string, sc: Scope): any {
    if (this.phase < 2) throw new DeferSig();   // universe not fully materialized yet
    const self = sc.inst!;
    const out: RecInst[] = [];
    for (const cand of this.env.registry) {
      if (cand.typeName !== typeName) continue;
      const slot = cand.slots.get(member);
      if (!slot) continue;
      let v: any;
      try { v = this.forceSlot(cand, member); } catch { continue; }   // invalid m: excluded silently
      if (this.containsRefTo(v, self.path)) out.push(cand);
    }
    out.sort((x, y) => cmpPath(x.path, y.path));
    return { __arr: true, items: out.map(c => ({ __ref: true, segs: c.path })), path: [] };
  }
  containsRefTo(v: any, target: Seg[]): boolean {
    if (isRef(v)) return cmpPath(v.segs, target) === 0;
    if (isArr(v)) return v.items.some((x: any) => this.containsRefTo(x, target));
    if (isMap(v)) return [...v.entries.values()].some((x: any) => this.containsRefTo(x, target));
    return false;
  }

  // ---------- binding / checking ----------
  bind(raw: any, rt: RT, path: Seg[], parent: RecInst | null, sc: Scope, forRef = false): any {
    // raw: lexical-JSON value | PreVal | evaluated value
    if (raw && raw.__expr) {
      // the expression's instance scope is the nearest enclosing instance
      const sc2 = { ...raw.scope, inst: parent ?? raw.scope.inst };
      if (rt.t === 'ref') {
        const place = this.evalPlace(raw.__expr, sc2);
        if (!place) throw new EvalErr('not a place in ref position');
        return { __ref: true, segs: place };
      }
      return this.bind(this.ev(raw.__expr, sc2), rt, path, parent, sc);
    }
    const fail = (msg: string, code?: string): never => {
      const tail = rt.tail;
      if (tail && tail.t === 'inline') this.env.report({ severity: 'error', id: rt.name, message: tail.template.filter((p: any) => typeof p === 'string').join(''), path: pathStr(path), code: 'E4001' });
      else this.env.report({ severity: 'error', message: msg, path: pathStr(path), code: code ?? 'E4001' });
      throw new Taint();
    };
    switch (rt.t) {
      case 'prim': {
        if (rt.name === 'int' && typeof raw === 'bigint') return raw;
        if (rt.name === 'float' && typeof raw === 'number') return raw;
        if (rt.name === 'bool' && typeof raw === 'boolean') return raw;
        if (rt.name === 'string' && typeof raw === 'string') return raw;
        if (rt.name === 'null' && raw === null) return raw;
        return fail(`expected ${rt.name}`);
      }
      case 'lit': return valueEq(raw, rt.v) || (typeof rt.v === 'bigint' && typeof raw === 'bigint' && raw === rt.v) || raw === rt.v ? raw : fail(`expected ${JSON.stringify(String(rt.v))}`);
      case 'range': {
        const ok = rt.base === 'int' ? typeof raw === 'bigint' : typeof raw === 'number';
        if (!ok) return fail(`expected ${rt.base} in range`);
        const hi = rt.excl ? raw < rt.hi : raw <= rt.hi;
        return raw >= rt.lo && hi ? raw : fail(`out of range ${rt.lo}..${rt.excl ? '<' : ''}${rt.hi}`);
      }
      case 'pattern':
        return typeof raw === 'string' && rt.re.test(raw) ? raw : fail(`does not match /${rt.src}/`);
      case 'quantity': {
        if (isQ(raw) && raw.dim === rt.dim) return raw;
        if (raw && raw.__jobj) {
          const es = new Map(raw.entries);
          if (es.size === 2 && es.has('value') && es.has('unit')) {
            const u = UNITS[es.get('unit') as string];
            if (!u || u.dim !== rt.dim) return fail(`unit of wrong dimension`, 'E4073');
            const num = es.get('value');
            return { __q: true, dim: rt.dim, value: Number(num) * u.factor };
          }
        }
        return fail('expected quantity');
      }
      case 'ref': {
        if (isRef(raw)) return raw;
        if (typeof raw === 'string') {
          const segs = parsePath(raw, sc.rootName);
          const target = this.resolveSegs(segs);
          if (target === undefined) return fail(`dangling reference ${raw}`, 'E6002');
          return { __ref: true, segs };
        }
        if (isRec(raw) || isArr(raw) || isMap(raw)) return { __ref: true, segs: raw.path };
        return fail('expected reference path');
      }
      case 'arr': {
        let items: any[];
        if (raw && raw.__pre === 'arr') {
          items = [];
          for (const it of raw.items) {
            if (it.spread) { const s = this.deref(this.ev(it.v.__expr, it.v.scope)); items.push(...this.matArr(s).map(x => x)); }
            else items.push(it.v);
          }
        } else if (Array.isArray(raw)) items = raw;
        else if (isArr(raw)) items = raw.items;
        else return fail('expected array');
        if (rt.lo !== undefined && (items.length < rt.lo || items.length > rt.hi)) return fail(`array size ${items.length} outside ${rt.lo}..${rt.hi}`);
        const arr: any = { __arr: true, items: [], path };
        items.forEach((it, i) => {
          try { arr.items.push(this.bind(it, rt.elem, [...path, i], parent, sc)); }
          catch (err) { if (err instanceof Taint) arr.items.push(ABSENT); else throw err; }
        });
        return arr;
      }
      case 'map': {
        const m: any = { __map: true, entries: new Map(), path };
        const es: [string, any][] = raw && raw.__jobj ? raw.entries
          : raw && raw.__pre === 'obj' ? raw.entries
          : isMap(raw) ? [...raw.entries.entries()].map(([k, v]: any) => [k, v])
          : null as any;
        if (!es) return fail('expected map');
        for (const [k, v] of es) {
          try { this.bind(k, rt.key, path, parent, sc); } catch (e) { if (e instanceof Taint) continue; throw e; }
          try { m.entries.set(k, this.bind(v, rt.val, [...path, k], parent, sc)); }
          catch (e) { if (!(e instanceof Taint)) throw e; }
        }
        return m;
      }
      case 'union': {
        // record arms discriminate on shared literal members; others by kind
        const recArms = rt.arms.filter((a: RT) => a.t === 'rec');
        if ((raw && raw.__jobj) || (raw && raw.__pre === 'obj') || isRec(raw)) {
          if (recArms.length > 0) {
            const discNames = recArms[0].members.filter((m: any) =>
              m.type?.t === 'lit' && recArms.every((a: RT) => a.members.some((x: any) => x.name === m.name && x.type?.t === 'lit')))
              .map((m: any) => m.name);
            for (const arm of recArms) {
              const ok = discNames.every((dn: string) => {
                const mv = this.rawEntry(raw, dn);
                const lit = arm.members.find((x: any) => x.name === dn)!.type.v;
                return mv !== undefined && valueEq(this.rawLit(mv), lit);
              });
              if (ok) return this.bind(raw, arm, path, parent, sc);
            }
            return fail(`no union arm matches discriminant`);
          }
        }
        for (const arm of rt.arms) {
          if (this.kindMatches(raw, arm)) return this.bind(raw, arm, path, parent, sc);
        }
        return fail('no union arm matches');
      }
      case 'rec': return this.bindRecord(raw, rt, path, parent, sc);
      case 'pred': {
        const v = this.bind(raw, rt.base, path, parent, sc);
        for (const p of rt.preds) {
          const fn = this.ev(p, { inst: null, locals: new Map(), rootName: sc.rootName });
          let ok: any;
          try { ok = this.call(fn, [v], sc); } catch { ok = false; }
          if (ok !== true) return fail(`predicate ${JSON.stringify(exprName(p))} not satisfied`);
        }
        return v;
      }
      case 'isectN': {
        let v: any = raw;
        for (const arm of rt.arms) v = this.bind(raw, arm, path, parent, sc);
        return v;
      }
      case 'any': return raw;
    }
    throw new Error(`bind: unhandled ${rt.t}`);
  }
  rawEntry(raw: any, name: string): any {
    if (raw && raw.__jobj) return raw.entries.find(([k]: any) => k === name)?.[1];
    if (raw && raw.__pre === 'obj') return raw.entries.find(([k]: any) => k === name)?.[1];
    if (isRec(raw)) return raw.slots.has(name) ? this.forceSlot(raw, name) : undefined;
    return undefined;
  }
  rawLit(v: any): any { return v && v.__expr ? this.ev(v.__expr, v.scope) : v; }
  kindMatches(raw: any, rt: RT): boolean {
    switch (rt.t) {
      case 'prim': return (rt.name === 'int' && typeof raw === 'bigint') || (rt.name === 'float' && typeof raw === 'number')
        || (rt.name === 'bool' && typeof raw === 'boolean') || (rt.name === 'string' && typeof raw === 'string') || (rt.name === 'null' && raw === null);
      case 'lit': return valueEq(this.rawLit(raw), rt.v);
      case 'range': return rt.base === 'int' ? typeof raw === 'bigint' : typeof raw === 'number';
      case 'pattern': return typeof raw === 'string';
      case 'arr': return Array.isArray(raw) || (raw && raw.__pre === 'arr') || isArr(raw);
      default: return true;
    }
  }
  evalPlace(e: Expr, sc: Scope): Seg[] | null {
    // navigation chain -> place; forgiving: evaluate then take path
    try {
      const v = this.evNav(e, sc);
      if (v && v.__segs) return v.__segs;
      const p = isRec(v) || isArr(v) || isMap(v) ? v.path : null;
      return p;
    } catch (err) {
      if (err instanceof EvalErr || err instanceof Taint) throw err;
      throw err;
    }
  }
  evNav(e: Expr, sc: Scope): any {
    if (e.e === 'member') {
      const x = this.deref(this.evNav(e.x, sc));
      const v = this.access(x, e.name);
      if (v === ABSENT && isRec(x)) return { __segs: [...x.path, e.name] };  // place through absent: integrity checks later
      return v;
    }
    if (e.e === 'index') {
      const x = this.deref(this.evNav(e.x, sc));
      const i = this.ev(e.i, sc);
      if (isArr(x)) return x.items[Number(i)] ?? { __segs: [...x.path, Number(i)] };
      if (isMap(x)) return x.entries.get(i) ?? { __segs: [...x.path, i] };
      if (isRec(x)) { const v = this.access(x, i); return v === ABSENT ? { __segs: [...x.path, i] } : v; }
    }
    return this.ev(e, sc);
  }

  bindRecord(raw: any, rt: RT, path: Seg[], parent: RecInst | null, sc: Scope): RecInst {
    let entries: [string, any][];
    if (raw && raw.__jobj) entries = raw.entries;
    else if (raw && raw.__pre === 'obj') entries = raw.entries;
    else if (isRec(raw)) {
      entries = raw.entryOrder.filter(n => !raw.slots.has(n) || raw.slots.get(n)!.kind !== 'der')
        .map(n => [n, raw.extras.has(n) ? raw.extras.get(n) : this.forceSlot(raw, n)]);
    }
    else { this.env.report({ severity: 'error', message: 'expected record', path: pathStr(path), code: 'E4001' }); throw new Taint(); }

    const inst: RecInst = {
      __rec: true, typeName: rt.name, rt, path, parent,
      slots: new Map(), entryOrder: entries.map(([k]) => k), extras: new Map(),
    };
    if (this.noReg === 0) this.env.registry.push(inst);
    const isc: Scope = { inst, locals: new Map(), rootName: sc.rootName };
    const supplied = new Map(entries);

    for (const m of rt.members) {
      const has = supplied.has(m.name);
      const types = m.conj ?? [m.type];
      const mkCheck = (rawV: any) => () => {
        let v: any;
        for (const ty of types) v = this.bind(rawV, ty, [...path, m.name], inst, isc);
        return v;
      };
      if (m.kind === 'der') {
        const slot: Slot = {
          kind: 'der', state: 'unforced', deferred: mentionsReferrersLocal(m.expr),
          compute: () => {
            let v = this.ev(m.expr, isc);
            if (m.type) v = this.bind(v, m.type, [...path, m.name], inst, isc);
            else if (v && (v.__pre || v.__jobj)) v = this.materialize(v, [...path, m.name], inst, isc);
            if (has) {
              this.noReg++;
              let restated: any;
              try { restated = this.bind(supplied.get(m.name), m.type ?? structuralOf(v), [...path, m.name], inst, isc); }
              finally { this.noReg--; }
              if (!valueEq(v, restated)) {
                this.env.report({ severity: 'error', message: `derived member ${m.name} restated with a differing value`, path: pathStr([...path, m.name]), code: 'E4005' });
                throw new Taint();
              }
            }
            return v;
          },
        };
        if (slot.deferred) this.deferredSlots.push({ inst, name: m.name });
        inst.slots.set(m.name, slot);
        continue;
      }
      if (has) {
        inst.slots.set(m.name, { kind: m.kind, state: 'unforced', deferred: false, compute: mkCheck(supplied.get(m.name)) });
      } else if (m.kind === 'dflt') {
        inst.slots.set(m.name, {
          kind: 'dflt', state: 'unforced', deferred: mentionsReferrersLocal(m.dflt),
          compute: () => {
            const v = this.ev(m.dflt, isc);
            let out: any;
            for (const ty of types) out = this.bind(v, ty, [...path, m.name], inst, isc);
            return out;
          },
        });
      } else if (m.kind === 'opt') {
        inst.slots.set(m.name, { kind: 'opt', state: 'absent', deferred: false });
      } else {
        inst.slots.set(m.name, { kind: 'req', state: 'invalid', deferred: false });
        this.env.report({ severity: 'error', message: `required member ${m.name} missing`, path: pathStr([...path, m.name]), code: 'E4002' });
      }
    }
    for (const [k, v] of entries) {
      if (rt.members.some((m: any) => m.name === k)) continue;
      if (rt.open) inst.extras.set(k, v);
      else this.env.report({ severity: 'error', message: `undeclared member ${k} on closed record${rt.name ? ' ' + rt.name : ''}`, path: pathStr([...path, k]), code: 'E4003' });
    }
    return inst;
  }
  materialize(v: any, path: Seg[], parent: RecInst | null, sc: Scope): any {
    // untyped structural value (rare: derived without annotation producing structure)
    if (v && v.__pre === 'arr') {
      const arr: any = { __arr: true, items: [], path };
      let i = 0;
      for (const it of v.items) {
        const x = it.v.__expr ? this.ev(it.v.__expr, it.v.scope) : it.v;
        arr.items.push(this.materialize(x, [...path, i], parent, sc)); i++;
      }
      return arr;
    }
    if (v && v.__pre === 'obj') {
      const m: any = { __map: true, entries: new Map(), path };
      for (const [k, pv] of v.entries) m.entries.set(k, this.materialize(pv.__expr ? this.ev(pv.__expr, pv.scope) : pv, [...path, k], parent, sc));
      return m;
    }
    return v;
  }

  forceState(inst: RecInst, name: string): string {
    this.forceSlotSafe(inst, name);
    return inst.slots.get(name)!.state;
  }
  forceSlotSafe(inst: RecInst, name: string) {
    try { this.forceSlot(inst, name); }
    catch (e) { if (!(e instanceof Taint) && !(e instanceof DeferSig)) throw e; }
  }
  forceSlot(inst: RecInst, name: string): any {
    const s = inst.slots.get(name);
    if (!s) throw new EvalErr(`no member ${name}`);
    if (s.state === 'ok') return s.value;
    if (s.state === 'absent') return ABSENT;
    if (s.state === 'invalid') throw new Taint();
    if (s.state === 'forcing') {
      this.env.report({ severity: 'error', message: `dependency cycle at ${name}`, path: pathStr([...inst.path, name]), code: 'E5007' });
      s.state = 'invalid'; throw new Taint();
    }
    s.state = 'forcing';
    try {
      const v = s.compute!();
      s.state = 'ok'; s.value = v; return v;
    } catch (e) {
      if (e instanceof DeferSig) {
        s.state = 'unforced';                              // retry in phase 2
        this.deferredSlots.push({ inst, name });
        throw e;
      }
      if (s.state === 'forcing') s.state = 'invalid';
      if (e instanceof EvalErr) {
        this.env.report({ severity: 'error', message: e.message, path: pathStr([...inst.path, name]), code: 'E5xxx' });
        throw new Taint();
      }
      throw e;
    }
  }
  forceConst(name: string, rootName: string): any {
    const c = this.env.consts.get(name)!;
    if (c.state === 'ok') return c.value;
    c.state = 'ok';
    c.value = this.ev(c.expr, { inst: null, locals: new Map(), rootName });
    if (c.value && (c.value.__pre || c.value.__jobj)) c.value = this.materialize(c.value, [name], null, { inst: null, locals: new Map(), rootName });
    return c.value;
  }

  // ---------- driving ----------
  forceAll(v: any, _deferredToo: boolean) {
    if (isRec(v)) {
      for (const [n, s] of v.slots) {
        this.forceSlotSafe(v, n);
        if (s.state === 'ok') this.forceAll(s.value, _deferredToo);
      }
    } else if (isArr(v)) v.items.forEach((x: any) => this.forceAll(x, _deferredToo));
    else if (isMap(v)) [...v.entries.values()].forEach((x: any) => this.forceAll(x, _deferredToo));
  }
  validateAll(rootName: string) {
    for (const inst of this.env.registry) this.runAsserts(inst, inst.rt.asserts, rootName);
  }
  runAsserts(inst: RecInst, asserts: any[], rootName: string) {
    const sc: Scope = { inst, locals: new Map(), rootName };
    for (const a of asserts) {
      if (a.kind === 'when') {
        let cond: any;
        try { cond = this.ev(a.cond, sc); } catch (e) { if (e instanceof Taint || e instanceof EvalErr) continue; throw e; }
        if (cond === true) {
          const inner = a.body.map((b: any) => b.m === 'assert'
            ? { kind: 'assert', name: b.name, cond: b.cond, tail: b.tail, origin: a.origin }
            : { kind: 'when', cond: b.cond, body: b.body, origin: a.origin });
          this.runAsserts(inst, inner, rootName);
        }
        continue;
      }
      let ok: any;
      try { ok = this.ev(a.cond, sc); }
      catch (e) {
        if (e instanceof Taint) continue;
        if (e instanceof EvalErr) { this.env.report({ severity: 'error', message: `${a.name}: ${e.message}`, path: pathStr(inst.path), code: 'E5xxx' }); continue; }
        throw e;
      }
      if (ok === true) continue;
      const id = `${a.origin ?? inst.typeName}.${a.name}`;
      if (!a.tail) { this.env.report({ severity: 'error', id, message: `assert ${a.name} failed`, path: pathStr(inst.path), code: 'E6001' }); continue; }
      if (a.tail.t === 'inline') {
        let msg = '';
        for (const p of a.tail.template) msg += typeof p === 'string' ? p : this.toStr(this.ev(p, sc));
        this.env.report({ severity: a.tail.severity, id, message: msg, path: pathStr(inst.path), code: a.tail.severity === 'error' ? 'E6001' : a.tail.severity === 'warn' ? 'W6001' : 'I6001' });
      } else {
        const d = this.env.diags.get(a.tail.name)!;
        const args = a.tail.args.map((x: Expr) => this.ev(x, sc));
        const psc: Scope = { inst: null, locals: new Map(d.params.map((p: any, i: number) => [p.name, args[i]])), rootName };
        let msg = '';
        for (const p of d.template) msg += typeof p === 'string' ? p : this.toStr(this.ev(p, psc));
        this.env.report({ severity: d.severity, id, message: msg, path: pathStr(inst.path), code: d.severity === 'error' ? 'E6001' : 'W6001' });
      }
    }
  }

  // ---------- serialization ----------
  serialize(v: any, rootName: string): string {
    const fmtF = (n: number) => { const s = String(n); return /[.eE]/.test(s) ? s : s + '.0'; };
    const go = (x: any): string | undefined => {
      if (x === ABSENT) return undefined;
      if (x === null) return 'null';
      if (typeof x === 'boolean') return String(x);
      if (typeof x === 'bigint') return x.toString();
      if (typeof x === 'number') return fmtF(x);
      if (typeof x === 'string') return JSON.stringify(x);
      if (isQ(x)) return `{"value":${fmtF(x.value)},"unit":${JSON.stringify(BASE_UNIT[x.dim])}}`;
      if (isRef(x)) return JSON.stringify(pathStr(x.segs, rootName));
      if (isArr(x)) return `[${x.items.map(go).filter((s: any) => s !== undefined).join(',')}]`;
      if (isMap(x)) return `{${[...x.entries.entries()].map(([k, v]: any) => `${JSON.stringify(k)}:${go(v)}`).filter((s: any) => !s.endsWith(':undefined')).join(',')}}`;
      if (isRec(x)) {
        const parts: string[] = [];
        const done = new Set<string>();
        for (const n of x.entryOrder) {
          done.add(n);
          if (x.extras.has(n)) { parts.push(`${JSON.stringify(n)}:${rawJson(x.extras.get(n))}`); continue; }
          const s = x.slots.get(n);
          if (!s || s.state === 'invalid' || s.state === 'absent') continue;
          if (s.kind === 'der') continue;                     // derived appended below in decl order
          const g = go(s.value); if (g !== undefined) parts.push(`${JSON.stringify(n)}:${g}`);
        }
        for (const m of x.rt.members) {
          if (done.has(m.name) && m.kind !== 'der') continue;
          const s = x.slots.get(m.name);
          if (!s || s.state === 'invalid' || s.state === 'absent' || s.state === 'unforced') continue;
          const g = go(s.value); if (g !== undefined) parts.push(`${JSON.stringify(m.name)}:${g}`);
        }
        return `{${parts.join(',')}}`;
      }
      throw new Error('serialize: unexpected value');
    };
    return go(v)!;
  }
}

function rawJson(v: any): string {
  if (v === null) return 'null';
  if (typeof v === 'boolean') return String(v);
  if (typeof v === 'bigint') return v.toString();
  if (typeof v === 'number') { const s = String(v); return /[.eE]/.test(s) ? s : s + '.0'; }
  if (typeof v === 'string') return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(rawJson).join(',')}]`;
  if (v && v.__jobj) return `{${v.entries.map(([k, x]: any) => `${JSON.stringify(k)}:${rawJson(x)}`).join(',')}}`;
  throw new Error('rawJson');
}

function exprName(e: any): string {
  if (e?.e === 'name') return e.name;
  if (e?.e === 'call') return exprName(e.fn);
  return '<predicate>';
}
function mentionsReferrersLocal(e: any): boolean {
  if (!e || typeof e !== 'object') return false;
  if (e.e === 'referrers') return true;
  return Object.values(e).some(v => Array.isArray(v) ? v.some(mentionsReferrersLocal) : mentionsReferrersLocal(v));
}
function structuralOf(v: any): RT {
  // shape-of-computed type for restating unannotated derived members
  if (typeof v === 'bigint') return { t: 'prim', name: 'int' };
  if (typeof v === 'number') return { t: 'prim', name: 'float' };
  if (typeof v === 'string') return { t: 'prim', name: 'string' };
  if (typeof v === 'boolean') return { t: 'prim', name: 'bool' };
  if (v === null) return { t: 'prim', name: 'null' };
  if (isRef(v)) return { t: 'ref', target: { t: 'any' } };
  if (isArr(v)) return { t: 'arr', elem: v.items.length ? structuralOf(v.items[0]) : { t: 'any' } };
  if (isQ(v)) return { t: 'quantity', dim: v.dim };
  return { t: 'any' };
}

