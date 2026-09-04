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
import { makeCtx, infer, checkExpr, requireVal, applyGuards, guardsOf, tryResolve } from './infer.ts';
import type { Ty, Target } from './infer.ts';
import type { ICtx, Ty } from './infer.ts';
import { Engine } from './engine.ts';

export type CheckHooks = { record?: (e: Expr, ty: Ty) => void; resolveHook?: (e: Expr, target: Target | null) => void };

export function checkModule(decls: Decl[], linked?: Env, hooks?: CheckHooks): Diag[] {
  const out: Diag[] = [];
  let curDecl: Decl | undefined;            // the declaration being checked
  const cx0 = makeCtx(null as any, () => {}); // the inference context (env set below); its `pos` anchors reports
  const report = (code: string, message: string) => {
    const loc = cx0.pos.at?.loc ?? curDecl?.loc;
    out.push(loc ? { code, message, severity: 'error', path: '', loc } : { code, message, severity: 'error', path: '' });
  };

  const env = linked ?? new Env();
  if (!linked) env.load(decls);
  if (!env.constEval) new Engine(env); // installs env.constEval / env.exprEval (§4.13, §3.16)
  env.onConstDiag = d => out.push(d);  // constant-evaluation errors surface here
  for (const n of env.duplicates) report('E3001', `duplicate name ${n} in module`);
  for (const d of env.finalizeUnitSpace()) out.push(d);   // §3.16 unit/dimension spaces

  // ---------- §4.13: constant positions ----------
  let curTParams = new Set<string>();
  const checkEndpoint = (v: any, where: string) => {
    if (typeof v !== 'string' || curTParams.has(v) || v.includes('.')) return;
    if (env.inputs.has(v) || env.outputs.some(o => o.name === v))
      report('E4021', `non-constant ${where}: ${v} is an input/output, not a module const`);
    else if (!env.consts.has(v))
      report('E3003', `unknown name ${v} in a ${where}`);
  };
  const constViolation = (e: any): string | null => {
    if (!e || typeof e !== 'object') return null;
    if (e.e === 'ctx') return `context variable ${e.name}`;
    if (e.e === 'referrers') return '$referrers';
    if (e.e === 'name' && env.inputs.has(e.name)) return `input ${e.name}`;
    if (e.e === 'name' && env.outputs.some(o => o.name === e.name)) return `output ${e.name}`;
    for (const v of Object.values(e)) {
      if (Array.isArray(v)) { for (const x of v) { const r = constViolation(x); if (r) return r; } }
      else if (v && typeof v === 'object') { const r = constViolation(v); if (r) return r; }
    }
    return null;
  };

  // ---------- AST-level walks ----------
  const walkType = (t: TypeAst | undefined, depth: number, declName?: string) => {
    if (!t) return;
    switch (t.k) {
      case 'range': {
        const kinds = [t.lo, t.hi].map(v => typeof v);
        if (kinds[0] !== kinds[1] && !kinds.includes('string'))
          report('E4010', `mixed range endpoints: ${t.lo}..${t.hi}`);
        checkEndpoint(t.lo, 'range endpoint');
        checkEndpoint(t.hi, 'range endpoint');
        break;
      }
      case 'record': checkRecordCtx(t, depth, declName); t.members.forEach(m => walkMember(m, depth, declName)); break;
      case 'map': walkType(t.key, depth, declName); walkType(t.val, depth, declName); break;
      case 'array':
        checkEndpoint(t.lo, 'array size');
        checkEndpoint(t.hi, 'array size');
        walkType(t.elem, depth, declName);
        break;
      case 'union': case 'isect': t.arms.forEach(a => walkType(a, depth, declName)); break;
      case 'func': t.params.forEach(p => walkType(p, depth, declName)); walkType(t.ret, depth, declName); break;
      case 'named':
        t.args.forEach(a => walkType(a, depth, declName));
        if (t.ext) { checkExtension(t, declName); walkType(t.ext, depth + 1, declName); }
        t.preds?.forEach(p => {
          const bad = constViolation(p);
          if (bad) report('E4021', `non-constant predicate argument: ${bad} (§4.13)`);
          walkExpr(p);
        });
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
      const oKind = om.m === 'derived' ? (om.hidden ? 'hidden' : 'der') : (om as any).dflt ? 'dflt' : (om as any).opt ? 'opt' : 'req';
      const bKind = bm.kind === 'der' && bm.hidden ? 'hidden' : bm.kind;
      // §5.9: overriding narrows; a hidden member stays hidden, a visible one visible
      const allowed: Record<string, string[]> = {
        req: ['req', 'dflt', 'der'],
        opt: ['req', 'opt', 'dflt', 'der'],
        dflt: ['req', 'dflt', 'der'],
        der: ['der'],
        hidden: ['hidden'],
      };
      if (!allowed[bKind]?.includes(oKind)) {
        report('E4032', `illegal member-kind transition for ${(om as any).name}: ${bKind} -> ${oKind} (${declName ?? t.name})`);
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

  const resolveReported = new Set<string>();
  const mapResolveErr = (msg: string, where: string) => {
    const key = `${msg}|${where}`;
    if (resolveReported.has(key)) return;   // one resolution failure, one report
    resolveReported.add(key);
    if (/unknown dimension|circular dimension/.test(msg)) report('E3003', `${msg} (in ${where})`);
    else if (/unknown unit/.test(msg)) report('E4073', `${msg} (in ${where})`);
    else if (/pattern interpolation of .*: unknown type/.test(msg)) report('E3003', `${msg} (in ${where})`);
    else if (/unknown type/.test(msg)) report('E3003', `${msg} (in ${where})`);
    else if (/generic arity/.test(msg)) report('E4022', `${msg} (in ${where})`);
    else if (/outside parameter/.test(msg)) report('E4023', `${msg} (in ${where})`);
    else if (/non-constant value argument/.test(msg)) report('E4021', `${msg} (in ${where})`);
    else if (/pattern interpolation/.test(msg)) report('E4117', `${msg} (in ${where})`);
    else if (/malformed pattern/.test(msg)) report('E4119', `${msg} (in ${where})`);
    else report('E4001', `${msg} (in ${where})`);   // never drop a resolution failure silently
  };
  const resolveOrReport = (t: TypeAst | undefined, where: string): RT | null => {
    if (!t) return null;
    try { return env.resolve(t); } catch (e: any) { mapResolveErr(e.message, where); return null; }
  };

  for (const [name, decl] of env.typeAsts) {
    if (decl.params?.length) continue;   // generic declarations check at instantiation (§3.15)
    let rt: RT | undefined;
    try { rt = env.resolve({ k: 'named', name, args: [] }); }
    catch (e: any) { mapResolveErr(e.message, name); continue; }
    checkResolved(rt, name);
  }

  function checkResolved(rt: RT, name: string, seen = new Set<RT>()) {
    if (!rt || seen.has(rt)) return;
    seen.add(rt);
    switch (rt.t) {
      case 'range': {
        const ks = [typeof rt.lo, typeof rt.hi];
        if (!ks.includes('string') && ks[0] !== ks[1])
          report('E4010', `mixed range endpoints after constant substitution in ${name}`);
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
    curDecl = d;
    curTParams = d.d === 'type' ? new Set((d.params ?? []).map(p => p.name)) : new Set();
    if (d.d === 'type') walkType(d.type, 1, d.name);
    else if (d.d === 'const') { walkType(d.type, 0); walkExpr(d.expr); }
    else if (d.d === 'func') { d.params.forEach(p => walkType(p.type, 0)); walkType(d.ret, 0); walkExpr(d.body); }
    else if (d.d === 'output') { walkType(d.type, 0); walkExpr(d.expr); }
    else if (d.d === 'input') { walkType(d.type, 0); if (d.fallback) walkExpr(d.fallback); }
  }

  // ---------- expression pass: inference, assignability, absence (§3.18, §4.10) ----------
  cx0.env = env; cx0.report = report;
  if (hooks) { cx0.record = hooks.record; cx0.resolveHook = hooks.resolveHook; }
  const isBool = (t: Ty) => !t.rt || (t.rt.t === 'prim' && t.rt.name === 'bool')
    || (t.rt.t === 'lit' && typeof t.rt.v === 'boolean');

  const recCtx = (cx: ICtx, rt: RT, ast?: TypeAst): ICtx => {
    const vars = new Map(cx.vars);
    for (const m of rt.members) {
      const mt: RT | null = m.conj ? { t: 'isectN', arms: m.conj } : (m.type ?? null);
      vars.set(m.name, { rt: mt, abs: m.kind === 'opt' });
    }
    vars.set('$this', { rt, abs: false });
    vars.set('$path', { rt: { t: 'prim', name: 'string' }, abs: false });
    if (ast && ast.k === 'record')
      for (const m of ast.members)
        if (m.m === 'context') vars.set(m.variable, { rt: tryResolve(env, m.type), abs: false });
    return { ...cx, vars, present: new Set(cx.present), nonnull: new Set(cx.nonnull) };
  };

  const checkMemberAst = (cx: ICtx, m: MemberAst) => {
    if (m.m === 'value' && m.dflt) checkExpr(cx, m.dflt, tryResolve(env, m.type));
    else if (m.m === 'derived') checkExpr(cx, m.expr, tryResolve(env, m.type));
    else if (m.m === 'assert') {
      if (!isBool(requireVal(cx, m.cond, infer(cx, m.cond), 'as an assert condition')))
        report('E4001', 'assert condition is not bool');
      if (m.tail?.t === 'inline') m.tail.template.forEach(p => { if (typeof p !== 'string') infer(cx, p); });
      if (m.tail?.t === 'ref') m.tail.args.forEach(a => requireVal(cx, a, infer(cx, a), 'as a diagnostic argument'));
    } else if (m.m === 'when') {
      if (!isBool(requireVal(cx, m.cond, infer(cx, m.cond), 'as a when condition')))
        report('E4001', 'when condition is not bool');
      const c2 = applyGuards(cx, guardsOf(m.cond, true));
      m.body.forEach(b => checkMemberAst(c2, b));
    }
  };

  const seenRecs = new Set<RT>();
  const checkRecordExprs = (rt: RT, cx: ICtx, ast?: TypeAst) => {
    if (rt.t !== 'rec' || seenRecs.has(rt)) return;
    seenRecs.add(rt);
    const cxR = recCtx(cx, rt, ast);
    // member expressions and asserts check in their declaring module's
    // scope (§8.3) — same rule the engine follows at evaluation
    const cxFor = (menv: Env | undefined): ICtx =>
      menv && menv !== cxR.env ? { ...cxR, env: menv } : cxR;
    // D30/E4090: an embedded type's declared bounds must hold at this
    // site — the container is the parent, the collection's key or index
    // type is what $key ranges over (none for a direct member)
    const checkEmbedding = (memberRt: RT, memberName: string, keyRt: RT | null) => {
      const site = `${rt.name ?? 'record'}.${memberName}`;
      const who = memberRt.name ?? 'the member type';
      for (const cd of (memberRt?.ctxDecls ?? []) as any[]) {
        if (cd.variable === '$parent') {
          const bound = cd.type?.t === 'ref' ? cd.type.target : null;
          if (bound && !subsumes(env, rt, bound))
            report('E4090', `embedding site ${site} fails ${who}'s $parent bound (§7.3)`);
        } else if (cd.variable === '$key') {
          if (!keyRt) report('E4090', `embedding site ${site} gives $key no meaning: ${who} is a direct member, not a collection element (§7.3)`);
          else if (!subsumes(env, keyRt, cd.type))
            report('E4090', `embedding site ${site} fails ${who}'s $key bound (§7.3)`);
        }
      }
    };
    const INT: RT = { t: 'prim', name: 'int' };
    for (const m of rt.members) {
      if (m.kind === 'der' && m.expr) checkExpr(cxFor(m.menv), m.expr, m.type ?? null);
      if (m.kind === 'dflt' && m.dflt) checkExpr(cxFor(m.menv), m.dflt, m.type ?? null);
      if (m.type?.t === 'rec') { checkEmbedding(m.type, m.name, null); checkRecordExprs(m.type, cxFor(m.menv)); }
      if (m.type?.t === 'arr' && m.type.elem?.t === 'rec') { checkEmbedding(m.type.elem, m.name, INT); checkRecordExprs(m.type.elem, cxFor(m.menv)); }
      if (m.type?.t === 'map' && m.type.val?.t === 'rec') { checkEmbedding(m.type.val, m.name, m.type.key); checkRecordExprs(m.type.val, cxFor(m.menv)); }
    }
    for (const a of rt.asserts) {
      if (a.kind === 'assert') checkMemberAst(cxFor(a.menv), { m: 'assert', name: a.name, cond: a.cond, tail: a.tail });
      else if (a.kind === 'when') checkMemberAst(cxFor(a.menv), { m: 'when', cond: a.cond, body: a.body });
    }
  };

  for (const [name, decl] of env.typeAsts) {
    if (decl.params?.length) continue;
    let rt: RT;
    try { rt = env.resolve({ k: 'named', name, args: [] }); } catch { continue; }
    checkRecordExprs(rt, cx0, decl.ast);
  }
  // D30/E4090 for $root: every record type owned (transitively) by an
  // evaluation root must have its declared $root bound met by the root's
  // own type — checked once per root declaration
  const checkRootBounds = (rootName: string, rootRt: RT) => {
    const seen = new Set<RT>();
    const walk = (t: RT | undefined) => {
      if (!t || seen.has(t)) return;
      seen.add(t);
      switch (t.t) {
        case 'rec':
          for (const cd of (t.ctxDecls ?? []) as any[]) {
            if (cd.variable !== '$root') continue;
            const bound = cd.type?.t === 'ref' ? cd.type.target : null;
            if (bound && !subsumes(env, rootRt, bound))
              report('E4090', `root ${rootName} fails ${t.name ?? 'a member type'}'s $root bound (§7.3)`);
          }
          for (const m of t.members) walk(m.type);
          break;
        case 'arr': walk(t.elem); break;
        case 'map': walk(t.val); break;
        case 'union': case 'isectN': t.arms.forEach(walk); break;
        case 'pred': walk(t.base); break;
      }
    };
    walk(rootRt);
  };
  // §7.3: the root's own type gives $parent and $key no meaning — the root
  // has no owner and sits under no key — so a declaration of either on it
  // (directly, or on a union arm) is an error at the root
  const checkRootType = (rootName: string, rootRt: RT) => {
    const arms = rootRt.t === 'union' ? rootRt.arms : [rootRt];
    for (const t of arms) {
      if (t.t !== 'rec') continue;
      const who = t.name ?? 'its type';
      for (const cd of (t.ctxDecls ?? []) as any[]) {
        if (cd.variable === '$parent')
          report('E4090', `root ${rootName} gives $parent no meaning: ${who} is the evaluation root's own type (§7.3)`);
        else if (cd.variable === '$key')
          report('E4090', `root ${rootName} gives $key no meaning: ${who} is the evaluation root's own type, not a collection element (§7.3)`);
      }
    }
  };
  for (const d of decls) {
    curDecl = d;
    if (d.d !== 'output' && d.d !== 'input') continue;
    const rt = tryResolve(env, d.type);
    if (rt) { checkRootType(d.name, rt); checkRootBounds(d.name, rt); }
  }
  for (const d of decls) {
    curDecl = d;
    if (d.d === 'const') checkExpr(cx0, d.expr, d.type ? resolveOrReport(d.type, `const ${d.name}`) : null);
    else if (d.d === 'func') {
      const cxF = { ...cx0, vars: new Map(cx0.vars) };
      for (const p of d.params) cxF.vars.set(p.name, { rt: resolveOrReport(p.type, `func ${d.name}`), abs: false });
      checkExpr(cxF, d.body, d.ret ? resolveOrReport(d.ret, `func ${d.name}`) : null);
    } else if (d.d === 'output') checkExpr(cx0, d.expr, resolveOrReport(d.type, `output ${d.name}`));
    else if (d.d === 'input' && d.fallback) checkExpr(cx0, d.fallback, resolveOrReport(d.type, `input ${d.name}`));
    else if (d.d === 'input') resolveOrReport(d.type, `input ${d.name}`);
    else if (d.d === 'diagnostic') {
      const cxD = { ...cx0, vars: new Map(cx0.vars) };
      for (const p of d.params) cxD.vars.set(p.name, { rt: tryResolve(env, p.type), abs: false });
      d.template.forEach(p => { if (typeof p !== 'string') infer(cxD, p); });
    } else if (d.d === 'unit' && d.factor) {
      const bad = constViolation(d.factor);
      if (bad) report('E4021', `non-constant unit factor for ${d.name}: ${bad} (§3.16)`);
    }
  }
  return out;
}
