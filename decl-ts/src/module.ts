// Module loading and linking (§8.1–8.5, §8.8): files are modules, the
// import graph is acyclic, exports are explicit, `std` stays ambient.
// Packages (§8.6–8.7) plug in through the `resolvePackage` hook.
import { host, dirname, resolvePath as absPath } from './host.ts';
import { parseSource } from './parse.ts';
import { Env, sortDiags } from './semantics.ts';
import type { Diag } from './semantics.ts';
import type { Decl, Loc } from './ast.ts';
import { Engine } from './engine.ts';
import { qevaluateUniverse, Unsupported } from './qengine/qeval.ts';
import { strictQEngine, useQEngine } from './pipeline.ts';

export type ExportEntry = { env: Env; name: string };
export type Module = {
  path: string;
  decls: Decl[];
  env: Env;
  exports: Map<string, ExportEntry>;
};
export type LoadResult = { modules: Module[]; entry: Module | null; diags: Diag[] };

export type PackageResolver = (
  spec: string,
  fromDir: string,
) => string | { code: string; message: string };

export function loadModules(
  entryPath: string,
  resolvePackage?: PackageResolver,
  sourceOverride?: Map<string, string>,
): LoadResult {
  const diags: Diag[] = [];
  const report = (code: string, message: string, loc?: Loc) =>
    diags.push({ severity: 'error', code, message, path: '', ...(loc ? { loc } : {}) });
  const modules = new Map<string, Module>();
  const order: Module[] = [];
  const visiting: string[] = [];

  const resolveSpec = (spec: string, fromDir: string): string | null => {
    if (spec.startsWith('./') || spec.startsWith('../')) return absPath(fromDir, spec);
    if (!resolvePackage) {
      report('E3010', `package import "${spec}" outside a package (no manifest)`);
      return null;
    }
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
      const text = host.readFile(abs);
      if (text === null) {
        report('E3004', `module not found: ${abs}`);
        return null;
      }
      src = text;
    }
    const { decls, errors } = parseSource(src);
    if (errors.length) {
      const e = errors[0];
      report('E2001', `${abs}: ${errors.length} parse error(s)`, {
        sl: e.row,
        sc: e.col,
        el: e.row,
        ec: e.col + 1,
      });
      return null;
    }
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
      mod.env.typeAsts.has(n) ||
      mod.env.consts.has(n) ||
      mod.env.funcs.has(n) ||
      mod.env.diags.has(n) ||
      mod.env.inputs.has(n) ||
      mod.env.outputs.some((o) => o.name === n) ||
      mod.env.imports.has(n) ||
      mod.env.namespaces.has(n);

    for (const d of decls) {
      if (d.d === 'import') {
        const tm = targets.get(d.from);
        if (!tm) continue;
        if (d.ns !== undefined) {
          if (taken(d.ns)) {
            report('E3006', `import ${d.ns} collides with an existing binding in ${abs}`);
            continue;
          }
          mod.env.namespaces.set(d.ns, { env: tm.env, exports: tm.exports });
          continue;
        }
        for (const it of d.names!) {
          const local = it.as ?? it.name;
          const ex = tm.exports.get(it.name);
          if (!ex) {
            report('E3005', `${tm.path} does not export ${it.name}`);
            continue;
          }
          if (taken(local)) {
            report('E3006', `import ${local} collides with an existing binding in ${abs}`);
            continue;
          }
          mod.env.imports.set(local, ex);
        }
      } else if (d.d === 're_export') {
        const tm = targets.get(d.from);
        if (!tm) continue;
        for (const it of d.names) {
          const ex = tm.exports.get(it.name);
          if (!ex) {
            report('E3005', `${tm.path} does not export ${it.name}`);
            continue;
          }
          mod.exports.set(it.as ?? it.name, ex); // interface statement, not a scope statement (§8.4)
        }
      }
    }
    for (const d of decls) {
      if (!d.exported || !('name' in d) || typeof (d as any).name !== 'string') continue;
      if (d.d === 'unit' || d.d === 'dimension') continue; // imports and re-exports have no name: skipped above
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
  for (const m of mods)
    for (const d of m.decls) {
      if (d.d === 'output' || d.d === 'input') {
        const prev = rootOwners.get(d.name);
        if (prev && prev !== m.path)
          report('E3018', `root ${d.name} declared in both ${prev} and ${m.path}`);
        rootOwners.set(d.name, m.path);
      }
    }
  // exported units and dimensions travel to every module's spaces (§8.2)
  for (const m of mods)
    for (const d of m.decls) {
      if (!d.exported) continue;
      if (d.d === 'dimension') {
        for (const m2 of mods) {
          if (m2 === m) continue;
          if (
            m2.env.dimDecls.has(d.name) &&
            !m2.decls.some((x) => x.d === 'dimension' && x.name === d.name)
          )
            continue;
          if (m2.env.dimDecls.has(d.name))
            report('E3001', `dimension ${d.name} redeclared across modules`);
          else m2.env.dimDecls.set(d.name, { terms: d.terms });
        }
      } else if (d.d === 'unit') {
        for (const m2 of mods) {
          if (m2 === m) continue;
          if (
            m2.env.unitDecls.has(d.name) &&
            !m2.decls.some((x) => x.d === 'unit' && x.name === d.name)
          )
            continue;
          if (m2.env.unitDecls.has(d.name))
            report('E4073', `unit ${d.name} redeclared across modules`);
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
export function runUniverse(
  mods: Module[],
  entry: Module,
  binds: { module?: Module; input: string; raw: any }[] = [],
): { eng: Engine; diags: Diag[] } {
  // the query engine populates entry.env.roots and forces them, so the returned
  // engine serializes exactly as the tree walker's does; bound input documents
  // are bound through the value layer before the outputs that read them
  if (useQEngine()) {
    try {
      const { report, eng } = qevaluateUniverse(mods, entry, binds);
      entry.env.diagnostics.splice(0, entry.env.diagnostics.length, ...report.diagnostics);
      return { eng, diags: entry.env.diagnostics };
    } catch (e) {
      if (!(e instanceof Unsupported) || strictQEngine()) throw e; // else fall back
    }
  }
  const bind = (eng: Engine) => {
    for (const m of mods) {
      m.env.constEval = (n: string) => eng.forceConstIn(m.env, n, '');
      m.env.exprEval = (e: any) =>
        eng.ev(e, { inst: null, locals: new Map(), rootName: '', menv: m.env });
    }
    // bound documents first: an output may read an input (§5.5), and a
    // bound input is a root of the universe (§9.2); unbound inputs with a
    // fallback bind on first demand (§9.4)
    for (const b of binds) {
      const m = b.module ?? entry;
      const decl = m.env.inputs.get(b.input)!;
      const sc: any = { inst: null, locals: new Map(), rootName: b.input, menv: m.env };
      eng.bindRoot(b.input, b.raw, m.env.resolve(decl.type), sc, false);
    }
    for (const m of mods)
      for (const o of m.env.outputs) {
        const sc: any = { inst: null, locals: new Map(), rootName: o.name, menv: m.env };
        eng.bindRoot(o.name, o.expr, m.env.resolve(o.type), sc, true);
      }
  };
  const eng = Engine.evaluate(entry.env, bind, () => entry.env.roots.values());
  eng.validateAll('');
  // §6.7: evaluation- and validation-time diagnostics in (path, id) order
  entry.env.diagnostics.splice(
    0,
    entry.env.diagnostics.length,
    ...sortDiags(entry.env.diagnostics),
  );
  return { eng, diags: entry.env.diagnostics };
}
