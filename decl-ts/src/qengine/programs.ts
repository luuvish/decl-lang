// Executable expressions are shared by syntax identity. Operations receive the
// engine and lexical frame at execution time; none capture a record instance.
import type { Expr } from '../ast.ts';
import type { Engine, Scope } from '../engine.ts';
import { ABSENT, EvalErr, isArr, isMap, isRec, mapKey } from '../semantics.ts';
import type { Value, RT } from '../semantics.ts';

export type Op = (eng: Engine, scope: Scope) => Value;

export interface MemberPlan {
  member: RT;
  types: RT[];
  expression?: Op;
  fallback?: Op;
  deferred: boolean;
}
interface Schema {
  source: RT[];
  members: MemberPlan[];
  names: Set<string>;
}

export class Programs {
  private readonly expressions = new WeakMap<Expr, Op>();
  private readonly callees = new WeakMap<Expr, Op>();
  private readonly navigations = new WeakMap<Expr, Op>();
  private readonly schemas = new WeakMap<RT, Schema>();
  compiled = 0;
  compiledSchemas = 0;

  schema(rt: RT, mentions: (e: Expr) => boolean): Schema {
    const hit = this.schemas.get(rt);
    if (hit && hit.source === rt.members && hit.members.length === rt.members.length) return hit;
    const members = rt.members.map((m: RT): MemberPlan => ({
      member: m,
      types: m.conj ?? [m.type],
      expression: m.expr ? this.get(m.expr) : undefined,
      fallback: m.dflt ? this.get(m.dflt) : undefined,
      deferred: mentions(m.kind === 'der' ? m.expr : m.dflt),
    }));
    const schema = {
      source: rt.members,
      members,
      names: new Set<string>(members.map((m: MemberPlan) => m.member.name)),
    };
    this.schemas.set(rt, schema);
    this.compiledSchemas++;
    return schema;
  }

  get(e: Expr): Op {
    let op = this.expressions.get(e);
    if (!op) {
      op = this.compile(e);
      this.expressions.set(e, op);
      this.compiled++;
    }
    return op;
  }

  callee(e: Expr): Op {
    let op = this.callees.get(e);
    if (!op) {
      if (e.e === 'member') {
        const base = this.callee(e.x),
          name = e.name;
        op = (eng, sc) => {
          const x = base(eng, sc);
          if (x?.__std) return { __std: true, path: [...x.path, name] };
          if (x?.__nsref) return eng.nsValue(x, name, sc);
          return eng.access(eng.deref(x), name);
        };
      } else op = this.get(e);
      this.callees.set(e, op);
    }
    return op;
  }

  nav(e: Expr): Op {
    let op = this.navigations.get(e);
    if (op) return op;
    if (e.e === 'paren') op = this.nav(e.x);
    else if (e.e === 'if') {
      const c = this.get(e.c),
        t = this.nav(e.t),
        f = this.nav(e.f);
      op = (eng, sc) => (eng.truthy(c(eng, sc)) ? t(eng, sc) : f(eng, sc));
    } else if (e.e === 'member') {
      const base = this.nav(e.x),
        name = e.name;
      op = (eng, sc) => {
        const raw = base(eng, sc);
        if (raw?.__segs) return { __segs: [...raw.__segs, name] };
        const x = eng.deref(raw),
          v = eng.access(x, name);
        return v === ABSENT && isRec(x) ? { __segs: [...x.path, name] } : v;
      };
    } else if (e.e === 'index') {
      const base = this.nav(e.x),
        index = this.get(e.i),
        ordinary = this.get(e);
      op = (eng, sc) => {
        const raw = base(eng, sc),
          i = index(eng, sc);
        const seg = typeof i === 'bigint' ? Number(i) : mapKey(i);
        if (raw?.__segs) return { __segs: [...raw.__segs, seg] };
        const x = eng.deref(raw);
        if (isArr(x)) return x.items[Number(i)] ?? { __segs: [...x.path, Number(i)] };
        if (isMap(x)) return x.entries.get(i) ?? { __segs: [...x.path, mapKey(i)] };
        if (isRec(x)) {
          const v = eng.access(x, i);
          return v === ABSENT ? { __segs: [...x.path, i] } : v;
        }
        return ordinary(eng, sc);
      };
    } else op = this.get(e);
    this.navigations.set(e, op);
    return op;
  }

  private compile(e: Expr): Op {
    switch (e.e) {
      case 'lit':
        return () => e.v;
      case 'pattern':
        return () => ({ __pat: true, re: e.re });
      case 'unitlit':
        return (eng) => {
          let u: { key: string; toBase: number };
          try {
            u = eng.env.unitInfo(e.unit);
          } catch (error) {
            throw new EvalErr((error as Error).message);
          }
          return { __q: true, dim: u.key, value: e.num * u.toBase };
        };
      case 'paren':
        return this.get(e.x);
      case 'name':
        return (eng, sc) => eng.nameValue(e.name, sc);
      case 'ctx':
        return (eng, sc) => eng.contextValue(e.name, sc);
      case 'referrers':
        return (eng, sc) => eng.referrers(e.type, e.member, sc);
      case 'obj': {
        const entries = e.entries;
        return (_eng, sc) => ({
          __pre: 'obj',
          entries: entries.map((en) => [en.key, { __expr: en.val, scope: sc }]),
        });
      }
      case 'arr': {
        const items = e.items;
        return (_eng, sc) => ({
          __pre: 'arr',
          items: items.map((it) => ({ spread: it.spread, v: { __expr: it.expr, scope: sc } })),
        });
      }
      case 'spread':
        return () => {
          throw new EvalErr('spread outside an object literal');
        };
      case 'template': {
        const parts = e.parts.map((p) => (typeof p === 'string' ? p : this.get(p)));
        return (eng, sc) => {
          let s = '';
          for (const p of parts) s += typeof p === 'string' ? p : eng.toStr(p(eng, sc));
          return s;
        };
      }
      case 'if': {
        const c = this.get(e.c),
          t = this.get(e.t),
          f = this.get(e.f);
        return (eng, sc) => (eng.truthy(c(eng, sc)) ? t(eng, sc) : f(eng, sc));
      }
      case 'un': {
        const x = this.get(e.x);
        if (e.op === '!') return (eng, sc) => !eng.truthy(x(eng, sc));
        if (e.op === '-') return (eng, sc) => -x(eng, sc);
        if (e.op === '~') return (eng, sc) => ~x(eng, sc);
        return () => {
          throw new EvalErr('un');
        };
      }
      case 'bin': {
        if (e.op === '|>')
          return this.get(
            e.r.e === 'call'
              ? { e: 'call', fn: e.r.fn, args: [e.l, ...e.r.args] }
              : { e: 'call', fn: e.r, args: [e.l] },
          );
        const l = this.get(e.l),
          r = this.get(e.r),
          op = e.op;
        if (op === '&&')
          return (eng, sc) => (eng.truthy(l(eng, sc)) ? eng.truthy(r(eng, sc)) : false);
        if (op === '||')
          return (eng, sc) => (eng.truthy(l(eng, sc)) ? true : eng.truthy(r(eng, sc)));
        if (op === '??')
          return (eng, sc) => {
            const v = l(eng, sc);
            return v === ABSENT || v === null ? r(eng, sc) : v;
          };
        return (eng, sc) => eng.applyBin(op, l(eng, sc), r(eng, sc));
      }
      case 'member': {
        const base = this.get(e.x),
          name = e.name,
          safe = e.safe;
        return (eng, sc) => {
          const x = base(eng, sc);
          if (x?.__nsref) return eng.nsValue(x, name, sc);
          if (safe && (x === null || x === ABSENT)) return ABSENT;
          return eng.access(eng.deref(x), name);
        };
      }
      case 'index': {
        const base = this.get(e.x),
          index = this.get(e.i);
        return (eng, sc) => {
          const x = eng.matVal(base(eng, sc)),
            i = index(eng, sc);
          if (isArr(x)) {
            const n = Number(i);
            if (n < 0 || n >= x.items.length)
              throw new EvalErr(`index ${n} out of bounds`, 'E5005');
            return x.items[n];
          }
          if (isMap(x)) return x.entries.has(i) ? x.entries.get(i) : ABSENT;
          if (isRec(x)) return eng.access(x, i);
          throw new EvalErr('index on non-collection');
        };
      }
      case 'call': {
        const args = e.args.map((a) => this.get(a)),
          fn = this.callee(e.fn);
        return (eng, sc) => {
          const vs = args.map((a) => a(eng, sc));
          return eng.call(fn(eng, sc), vs, sc);
        };
      }
      case 'lambda': {
        this.get(e.body);
        return (_eng, sc) => ({ __clo: true, params: e.params, body: e.body, scope: sc });
      }
      case 'with': {
        const base = this.get(e.base),
          patch = this.get(e.patch);
        return (eng, sc) => {
          const v = eng.deref(base(eng, sc));
          if (!(v?.__pre === 'obj' || isMap(v) || isRec(v)))
            throw new EvalErr('with on non-record');
          return eng.withValue(v, patch(eng, sc));
        };
      }
      case 'match': {
        const subject = this.get(e.subject),
          arms = e.arms.map((a) => ({ ...a, op: this.get(a.body) }));
        return (eng, sc) => {
          const v = eng.deref(subject(eng, sc));
          const run = (arm: (typeof arms)[number]) => {
            const locals = new Map(sc.locals);
            locals.set(arm.v, v);
            return arm.op(eng, { ...sc, locals });
          };
          let fallback: (typeof arms)[number] | undefined;
          for (const arm of arms) {
            if (!arm.type) fallback = arm;
            else if (eng.memberOf(v, (sc.menv ?? eng.env).resolve(arm.type), sc)) return run(arm);
          }
          if (fallback) return run(fallback);
          throw new EvalErr('match: no arm matched');
        };
      }
      case 'comp':
      case 'mapcomp': {
        const clauses = e.clauses.map((c) => ({
          name: c.v,
          iter: this.get(c.iter),
          filters: c.filters.map((f) => this.get(f)),
        }));
        if (e.e === 'comp') this.get(e.head);
        const key = e.e === 'mapcomp' ? this.get(e.key) : null,
          val = e.e === 'mapcomp' ? this.get(e.val) : null;
        return (eng, sc) => {
          const entries: [string, Value][] = [],
            items: Value[] = [];
          const visit = (ci: number, scope: Scope) => {
            if (ci === clauses.length) {
              if (e.e === 'comp') items.push({ spread: false, v: { __expr: e.head, scope } });
              else {
                const k = key!(eng, scope);
                if (typeof k !== 'string') throw new EvalErr('map key must be string');
                if (entries.some(([n]) => n === k))
                  throw new EvalErr(`duplicate key ${k}`, 'E5004');
                entries.push([k, val!(eng, scope)]);
              }
              return;
            }
            const c = clauses[ci];
            for (const item of eng.iterate(c.iter(eng, scope))) {
              const locals = new Map(scope.locals);
              locals.set(c.name, item);
              const next = { ...scope, locals };
              if (c.filters.every((f) => eng.truthy(f(eng, next)))) visit(ci + 1, next);
            }
          };
          visit(0, sc);
          return e.e === 'comp' ? { __pre: 'arr', items } : { __pre: 'obj', entries };
        };
      }
    }
  }
}
