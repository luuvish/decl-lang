// Static checks (growing toward the full chapter-3/4 checker).
// Implemented so far: mixed range endpoints (E4010), `??` mixed with
// `&&`/`||` without parentheses (E4052).
import type { Decl, Expr, MemberAst, TypeAst } from './ast.ts';
import type { Diag } from './semantics.ts';

export function checkModule(decls: Decl[]): Diag[] {
  const out: Diag[] = [];
  const report = (code: string, message: string) =>
    out.push({ code, message, severity: 'error', path: '' });

  const walkType = (t: TypeAst | undefined) => {
    if (!t) return;
    switch (t.k) {
      case 'range': {
        const kinds = [t.lo, t.hi].map(v => typeof v);
        if (kinds[0] !== kinds[1] && !kinds.includes('string'))
          report('E4010', `mixed range endpoints: ${t.lo}..${t.hi}`);
        break;
      }
      case 'record': t.members.forEach(walkMember); break;
      case 'map': walkType(t.key); walkType(t.val); break;
      case 'array': walkType(t.elem); break;
      case 'union': case 'isect': t.arms.forEach(walkType); break;
      case 'func': t.params.forEach(walkType); walkType(t.ret); break;
      case 'named': t.args.forEach(walkType); walkType(t.ext); t.preds?.forEach(walkExpr); break;
    }
  };
  const walkMember = (m: MemberAst) => {
    switch (m.m) {
      case 'value': walkType(m.type); if (m.dflt) walkExpr(m.dflt); break;
      case 'derived': walkType(m.type); walkExpr(m.expr); break;
      case 'context': walkType(m.type); break;
      case 'assert': walkExpr(m.cond); break;
      case 'when': walkExpr(m.cond); m.body.forEach(walkMember); break;
    }
  };
  const isBoolOp = (e: Expr) => e.e === 'bin' && (e.op === '&&' || e.op === '||');
  const walkExpr = (e: Expr | undefined) => {
    if (!e || typeof e !== 'object') return;
    if (e.e === 'bin' && e.op === '??' && (isBoolOp(e.l) || isBoolOp(e.r)))
      report('E4052', '`??` mixed with `&&`/`||` without parentheses');
    for (const v of Object.values(e)) {
      if (Array.isArray(v)) v.forEach(x => { if (x && typeof x === 'object') visitAny(x); });
      else if (v && typeof v === 'object') visitAny(v);
    }
  };
  const visitAny = (x: any) => {
    if (x.e) walkExpr(x);
    else if (x.k) walkType(x);
    else if (x.m) walkMember(x);
    else if (x.expr) walkExpr(x.expr);
    else if (Array.isArray(x)) x.forEach(visitAny);
  };

  for (const d of decls) {
    if (d.d === 'type') { walkType(d.type); }
    else if (d.d === 'const') { walkType(d.type); walkExpr(d.expr); }
    else if (d.d === 'func') { d.params.forEach(p => walkType(p.type)); walkType(d.ret); walkExpr(d.body); }
    else if (d.d === 'output') { walkType(d.type); walkExpr(d.expr); }
    else if (d.d === 'input') { walkType(d.type); if (d.fallback) walkExpr(d.fallback); }
  }
  return out;
}
