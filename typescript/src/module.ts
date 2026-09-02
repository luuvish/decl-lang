// Module loading and linking (§8.1–8.5, §8.8): files are modules, the
// import graph is acyclic, exports are explicit, `std` stays ambient.
// Packages (§8.6–8.7) plug in through the `resolvePackage` hook.
import { readFileSync } from 'node:fs';
import { dirname, resolve as absPath } from 'node:path';
import { parseSource } from './parse.ts';
import { Env } from './semantics.ts';
import type { Diag } from './semantics.ts';
import type { Decl } from './ast.ts';
import { Engine } from './engine.ts';

export type ExportEntry = { env: Env; name: string };
export type Module = {
  path: string;
  decls: Decl[];
  env: Env;
  exports: Map<string, ExportEntry>;
};
export type LoadResult = { modules: Module[]; entry: Module | null; diags: Diag[] };

export type PackageResolver = (spec: string, fromDir: string) => string | { code: string; message: string };

export function loadModules(entryPath: string, resolvePackage?: PackageResolver,
  sourceOverride?: Map<string, string>): LoadResult {
  const diags: Diag[] = [];
  const report = (code: string, message: string) => diags.push({ severity: 'error', code, message, path: '' });
  const modules = new Map<string, Module>();
  const order: Module[] = [];
  const visiting: string[] = [];

  const resolveSpec = (spec: string, fromDir: string): string | null => {
    if (spec.startsWith('./') || spec.startsWith('../')) return absPath(fromDir, spec);
    if (!resolvePackage) { report('E3010', `package import "${spec}" outside a package (no manifest)`); return null; }
    const r = resolvePackage(spec, fromDir);
    if (typeof r === 'string') return r;
    report(r.code, r.message);
    return null;
  };

  const load = (path: string): Module | null => {
    const abs = absPath(path);
    if (modules.has(abs)) return modules.get(abs)!;
    const ci = visiting.indexOf(abs);
    if (ci >= 0) {
      report('E3007', `module import cycle: ${[...visiting.slice(ci), abs].join(' -> ')}`);
      return null;
    }
    let src: string;
    if (sourceOverride?.has(abs)) src = sourceOverride.get(abs)!;
    else {
      try { src = readFileSync(abs, 'utf8'); }
      catch { report('E3004', `module not found: ${abs}`); return null; }
    }
    const { decls, errors } = parseSource(src);
    if (errors.length) { report('E2001', `${abs}: ${errors.length} parse error(s)`); return null; }
    const env = new Env();
    env.load(decls);
    for (const n of env.duplicates) report('E3001', `duplicate name ${n} in ${abs}`);
    const mod: Module = { path: abs, decls, env, exports: new Map() };
    visiting.push(abs);
    const targets = new Map<string, Module>();
    for (const d of decls) {
      if (d.d !== 'import' && d.d !== 're_export') continue;
      const target = resolveSpec(d.from, dirname(abs));
      if (target === null) continue;
      const tm = load(target);
      if (tm) targets.set(d.from, tm);
    }
    visiting.pop();
    modules.set(abs, mod);

    const taken = (n: string) =>
      mod.env.typeAsts.has(n) || mod.env.consts.has(n) || mod.env.funcs.has(n)
      || mod.env.diags.has(n) || mod.env.inputs.has(n)
      || mod.env.outputs.some(o => o.name === n)
      || mod.env.imports.has(n) || mod.env.namespaces.has(n);

    for (const d of decls) {
      if (d.d === 'import') {
        const tm = targets.get(d.from);
        if (!tm) continue;
        if (d.ns !== undefined) {
          if (taken(d.ns)) { report('E3006', `import ${d.ns} collides with an existing binding in ${abs}`); continue; }
          mod.env.namespaces.set(d.ns, { env: tm.env, exports: tm.exports });
          continue;
        }
        for (const it of d.names!) {
          const local = it.as ?? it.name;
          const ex = tm.exports.get(it.name);
          if (!ex) { report('E3005', `${tm.path} does not export ${it.name}`); continue; }
          if (taken(local)) { report('E3006', `import ${local} collides with an existing binding in ${abs}`); continue; }
          mod.env.imports.set(local, ex);
        }
      } else if (d.d === 're_export') {
        const tm = targets.get(d.from);
        if (!tm) continue;
        for (const it of d.names!) {
          const ex = tm.exports.get(it.name);
          if (!ex) { report('E3005', `${tm.path} does not export ${it.name}`); continue; }
          mod.exports.set(it.as ?? it.name, ex);   // interface statement, not a scope statement (§8.4)
        }
      }
    }
    for (const d of decls) {
      if (!d.exported || !('name' in d) || typeof (d as any).name !== 'string') continue;
      if (d.d === 'unit' || d.d === 'dimension' || d.d === 'import' || d.d === 're_export') continue;
      mod.exports.set((d as any).name, { env: mod.env, name: (d as any).name });
    }
    order.push(mod);
    return mod;
  };

  const entry = load(entryPath);
  if (entry) linkUniverse(order, entry, report);
  return { modules: order, entry, diags };
}

// universe-wide wiring: shared evaluation state, exported unit/dimension
// spaces, and §8.8 root-name uniqueness
function linkUniverse(mods: Module[], entry: Module, report: (c: string, m: string) => void) {
  const rootOwners = new Map<string, string>();
  for (const m of mods) for (const d of m.decls) {
    if (d.d === 'output' || d.d === 'input') {
      const prev = rootOwners.get(d.name);
      if (prev && prev !== m.path)
        report('E3018', `root ${d.name} declared in both ${prev} and ${m.path}`);
      rootOwners.set(d.name, m.path);
    }
  }
  // exported units and dimensions travel to every module's spaces (§8.2)
  for (const m of mods) for (const d of m.decls) {
    if (!d.exported) continue;
    if (d.d === 'dimension') {
      for (const m2 of mods) {
        if (m2 === m) continue;
        if (m2.env.dimDecls.has(d.name) && !m2.decls.some(x => x.d === 'dimension' && x.name === d.name)) continue;
        if (m2.env.dimDecls.has(d.name)) report('E3001', `dimension ${d.name} redeclared across modules`);
        else m2.env.dimDecls.set(d.name, { terms: d.terms });
      }
    } else if (d.d === 'unit') {
      for (const m2 of mods) {
        if (m2 === m) continue;
        if (m2.env.unitDecls.has(d.name) && !m2.decls.some(x => x.d === 'unit' && x.name === d.name)) continue;
        if (m2.env.unitDecls.has(d.name)) report('E4073', `unit ${d.name} redeclared across modules`);
        else m2.env.unitDecls.set(d.name, { dim: d.dim, factor: d.factor, base: d.base });
      }
    }
  }
  for (const m of mods) {
    if (m === entry) continue;
    m.env.registry = entry.env.registry;
    m.env.roots = entry.env.roots;
    m.env.diagnostics = entry.env.diagnostics;
  }
}

// evaluate the whole universe: every module's outputs are roots (§8.8)
export function runUniverse(mods: Module[], entry: Module,
  binds: { module?: Module; input: string; raw: any }[] = []): { eng: Engine; diags: Diag[] } {
  const eng = new Engine(entry.env);
  for (const m of mods) {
    m.env.constEval = (n: string) => eng.forceConstIn(m.env, n, '');
    m.env.exprEval = (e: any) => eng.ev(e, { inst: null, locals: new Map(), rootName: '', menv: m.env } as any);
  }
  for (const m of mods) for (const o of m.env.outputs) {
    const sc: any = { inst: null, locals: new Map(), rootName: o.name, menv: m.env };
    try { entry.env.roots.set(o.name, eng.bind(eng.ev(o.expr, sc), m.env.resolve(o.type), [o.name], null, sc)); }
    catch { }
  }
  for (const b of binds) {
    const m = b.module ?? entry;
    const decl = m.env.inputs.get(b.input)!;
    const sc: any = { inst: null, locals: new Map(), rootName: b.input, menv: m.env };
    try { entry.env.roots.set(b.input, eng.bind(b.raw, m.env.resolve(decl.type), [b.input], null, sc)); }
    catch { }
  }
  for (const v of entry.env.roots.values()) eng.forceAll(v, false);
  eng.phase = 2;
  for (let i = 0; i < eng.deferredSlots.length; i++) eng.forceSlotSafe(eng.deferredSlots[i].inst, eng.deferredSlots[i].name);
  for (const v of entry.env.roots.values()) eng.forceAll(v, true);
  eng.validateAll('');
  return { eng, diags: entry.env.diagnostics };
}
