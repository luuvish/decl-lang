// Static checks over the AST + resolved types (growing toward the full
// chapter 3–4 checker). Implemented:
//   E3001 duplicate module name         E3003 unknown type name
//   E4010 mixed range endpoints         E4011 empty range / array size
//   E4012 structurally empty intersection
//   E4013 non-discriminable record union arms
//   E4014 more than one non-record object arm in a union
//   E4015 map key not string-shaped     E4030 inheritance widening
//   E4032 illegal member-kind transition
//   E4052 ?? mixed with &&/|| unparenthesized
//   E4094 context variable without / with an invalid context declaration
import type { Decl, Expr, MemberAst, TypeAst } from './ast.ts';
import { Env } from './semantics.ts';
import type { Diag, RT } from './semantics.ts';
import { subsumes, structurallyEmpty } from './subsume.ts';

export function checkModule(decls: Decl[]): Diag[] {
  const out: Diag[] = [];
  const report = (code: string, message: string) =>
    out.push({ code, message, severity: 'error', path: '' });

  const env = new Env();
  env.load(decls);
  for (const n of env.duplicates) report('E3001', `duplicate name ${n} in module`);

  // ---------- AST-level walks ----------
  const walkType = (t: TypeAst | undefined, depth: number, declName?: string) => {
    if (!t) return;
    switch (t.k) {
      case 'range': {
        const kinds = [t.lo, t.hi].map(v => typeof v);
        if (kinds[0] !== kinds[1] && !kinds.includes('string'))
          report('E4010', `mixed range endpoints: ${t.lo}..${t.hi}`);
        break;
      }
      case 'record': checkRecordCtx(t, depth, declName); t.members.forEach(m => walkMember(m, depth, declName)); break;
      case 'map': walkType(t.key, depth, declName); walkType(t.val, depth, declName); break;
      case 'array': walkType(t.elem, depth, declName); break;
      case 'union': case 'isect': t.arms.forEach(a => walkType(a, depth, declName)); break;
      case 'func': t.params.forEach(p => walkType(p, depth, declName)); walkType(t.ret, depth, declName); break;
      case 'named':
        t.args.forEach(a => walkType(a, depth, declName));
        if (t.ext) { checkExtension(t, declName); walkType(t.ext, depth + 1, declName); }
        t.preds?.forEach(p => walkExpr(p));
        break;
    }
  };
  const walkMember = (m: MemberAst, depth: number, declName?: string) => {
    switch (m.m) {
      case 'value': walkType(m.type, depth + 1, declName); if (m.dflt) walkExpr(m.dflt); break;
      case 'derived': walkType(m.type, depth + 1, declName); walkExpr(m.expr); break;
      case 'context': walkType(m.type, depth + 1, declName); break;
      case 'assert': walkExpr(m.cond); break;
      case 'when': walkExpr(m.cond); m.body.forEach(x => walkMember(x, depth, declName)); break;
    }
  };
  const isBoolOp = (e: Expr) => e.e === 'bin' && (e.op === '&&' || e.op === '||');
  const walkExpr = (e: any) => {
    if (!e || typeof e !== 'object') return;
    if (e.e === 'bin' && e.op === '??' && (isBoolOp(e.l) || isBoolOp(e.r)))
      report('E4052', '`??` mixed with `&&`/`||` without parentheses');
    for (const v of Object.values(e)) {
      if (Array.isArray(v)) v.forEach(walkExpr);
      else if (v && typeof v === 'object') walkExpr(v);
    }
  };

  // ---------- D30: context obligations ----------
  const ctxUses = (m: MemberAst): Set<string> => {
    const used = new Set<string>();
    const scan = (e: any, recDepth: number) => {
      if (!e || typeof e !== 'object') return;
      if (e.e === 'ctx' && ['$parent', '$root', '$key'].includes(e.name)) used.add(e.name);
      for (const v of Object.values(e)) {
        if (Array.isArray(v)) v.forEach(x => scan(x, recDepth));
        else if (v && typeof v === 'object' && !(v as any).k) scan(v, recDepth);
      }
    };
    if (m.m === 'value' && m.dflt) scan(m.dflt, 0);
    if (m.m === 'derived') scan(m.expr, 0);
    if (m.m === 'assert') scan(m.cond, 0);
    if (m.m === 'when') { scan(m.cond, 0); m.body.forEach(b => ctxUses(b).forEach(u => used.add(u))); }
    return used;
  };
  const checkRecordCtx = (rec: TypeAst & { k: 'record' }, depth: number, declName?: string) => {
    const declared = new Map(rec.members.filter(m => m.m === 'context')
      .map((m: any) => [m.variable, m.type as TypeAst]));
    for (const [v, ty] of declared) {
      if ((v === '$parent' || v === '$root')) {
        const isRef = ty.k === 'named' && ty.name === 'ref';
        if (!isRef) report('E4094', `${v} declaration must be ref<...> (${declName ?? 'anonymous'})`);
      }
      if (v === '$key' && ty.k === 'named' && ty.name === 'ref')
        report('E4094', `$key declares a plain value type, not ref<...>`);
    }
    if (depth > 1) return;   // lexically nested: parent evident, no declaration required
    const used = new Set<string>();
    rec.members.forEach(m => ctxUses(m).forEach(u => used.add(u)));
    for (const u of used)
      if (!declared.has(u))
        report('E4094', `${u} used without a context declaration in ${declName ?? 'anonymous type'}`);
  };

  // ---------- inheritance (extension) ----------
  const checkExtension = (t: TypeAst & { k: 'named' }, declName?: string) => {
    let base: RT;
    try { base = env.resolve({ k: 'named', name: t.name, args: t.args }); }
    catch { return; }  // unknown base reported by the resolution pass
    if (base.t !== 'rec') { report('E4031', `extending non-record type ${t.name}`); return; }
    const ext = t.ext as TypeAst & { k: 'record' };
    for (const om of ext.members) {
      if (om.m === 'assert' || om.m === 'when' || om.m === 'context') continue;
      const bm = base.members.find((x: any) => x.name === (om as any).name);
      if (!bm) continue;   // addition
      const oKind = om.m === 'derived' ? 'der' : (om as any).dflt ? 'dflt' : (om as any).opt ? 'opt' : 'req';
      const allowed: Record<string, string[]> = {
        req: ['req', 'dflt', 'der'],
        opt: ['req', 'opt', 'dflt', 'der'],
        dflt: ['req', 'dflt', 'der'],
        der: ['der'],
      };
      if (!allowed[bm.kind]?.includes(oKind)) {
        report('E4032', `illegal member-kind transition for ${(om as any).name}: ${bm.kind} -> ${oKind} (${declName ?? t.name})`);
        continue;
      }
      const oType = (om as any).type ? env.tryResolve((om as any).type) : undefined;
      if (oType && bm.type && !subsumes(env, oType, bm.type))
        report('E4030', `override widens inherited member ${(om as any).name} (${declName ?? t.name})`);
    }
  };

  // ---------- resolution-level checks ----------
  (env as any).tryResolve = (ast: TypeAst): RT | undefined => {
    try { return env.resolve(ast); } catch { return undefined; }
  };

  for (const [name, decl] of env.typeAsts) {
    if (decl.params?.length) continue;   // generic declarations check at instantiation (§3.15)
    let rt: RT | undefined;
    try { rt = env.resolve({ k: 'named', name, args: [] }); }
    catch (e: any) {
      if (/unknown type/.test(e.message)) report('E3003', `${e.message} (in ${name})`);
      continue;
    }
    checkResolved(rt, name);
  }

  function checkResolved(rt: RT, name: string, seen = new Set<RT>()) {
    if (!rt || seen.has(rt)) return;
    seen.add(rt);
    switch (rt.t) {
      case 'range': {
        if (structurallyEmpty(env, rt)) report('E4011', `empty range in ${name}`);
        break;
      }
      case 'arr':
        if (structurallyEmpty(env, rt)) report('E4011', `empty array size in ${name}`);
        checkResolved(rt.elem, name, seen);
        break;
      case 'isectN':
        if (structurallyEmpty(env, rt)) report('E4012', `structurally empty intersection in ${name}`);
        rt.arms.forEach((a: RT) => checkResolved(a, name, seen));
        break;
      case 'map': {
        const strShaped = (k: RT): boolean =>
          (k.t === 'prim' && k.name === 'string') || k.t === 'pattern'
          || (k.t === 'lit' && typeof k.v === 'string')
          || (k.t === 'union' && k.arms.every(strShaped))
          || (k.t === 'pred' && strShaped(k.base));
        if (!strShaped(rt.key)) report('E4015', `map key type not string-shaped in ${name}`);
        checkResolved(rt.val, name, seen);
        break;
      }
      case 'union': {
        const recs = rt.arms.filter((a: RT) => a.t === 'rec');
        if (recs.length >= 2) {
          const disc = recs[0].members.filter((m: any) => m.type?.t === 'lit'
            && recs.every((r: RT) => r.members.some((x: any) => x.name === m.name && x.type?.t === 'lit')));
          const tuples = new Set(recs.map((r: RT) =>
            JSON.stringify(disc.map((d: any) =>
              String(r.members.find((x: any) => x.name === d.name)!.type.v)))));
          if (disc.length === 0 || tuples.size !== recs.length)
            report('E4013', `record union arms not discriminable in ${name}`);
        }
        const nonRecObj = rt.arms.filter((a: RT) => a.t === 'map' || a.t === 'quantity');
        if (nonRecObj.length > 1) report('E4014', `more than one non-record object arm in ${name}`);
        rt.arms.forEach((a: RT) => checkResolved(a, name, seen));
        break;
      }
      case 'rec':
        for (const m of rt.members) if (m.type) checkResolved(m.type, name, seen);
        break;
      case 'pred': checkResolved(rt.base, name, seen); break;
      case 'ref': checkResolved(rt.target, name, seen); break;
    }
  }

  // AST walks over all declarations
  for (const d of decls) {
    if (d.d === 'type') walkType(d.type, 1, d.name);
    else if (d.d === 'const') { walkType(d.type, 0); walkExpr(d.expr); }
    else if (d.d === 'func') { d.params.forEach(p => walkType(p.type, 0)); walkType(d.ret, 0); walkExpr(d.body); }
    else if (d.d === 'output') { walkType(d.type, 0); walkExpr(d.expr); }
    else if (d.d === 'input') { walkType(d.type, 0); if (d.fallback) walkExpr(d.fallback); }
  }
  return out;
}
