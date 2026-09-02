#!/usr/bin/env node
// The `decl` CLI (ROADMAP Phase 4):
//   decl check <files...>                     parse + static checks (module-aware)
//   decl evaluate <file> [--root <name>]          evaluate outputs -> JSON on stdout
//   decl validate <dir>                       judge a fixture corpus (@expect-* metadata)
//   decl validate <file> [--input n=doc.json] [--expect-errors E1,E2]
//   decl fmt <files...> [--check]             canonical formatting (in place)
import { readFileSync, writeFileSync, statSync } from 'node:fs';
import { resolve as absPath } from 'node:path';
import { initParser, parseSource } from './parse.ts';
import { readJson } from './semantics.ts';
import type { Diag } from './semantics.ts';
import { checkModule } from './checker.ts';
import { loadModules, runUniverse } from './module.ts';
import { openPackageUniverse, verifyLock } from './package.ts';
import { judgeCorpus, judgeFixture, runPipeline } from './conformance.ts';
import { format, initFormatter } from './fmt.ts';

const args = process.argv.slice(2);
const cmd = args.shift();

const flags = new Map<string, string | boolean>();
const positional: string[] = [];
for (let i = 0; i < args.length; i++) {
  if (args[i].startsWith('--')) {
    const name = args[i].slice(2);
    if (i + 1 < args.length && !args[i + 1].startsWith('--') && ['root', 'input', 'expect-errors'].includes(name))
      flags.set(name, args[++i]);
    else flags.set(name, true);
  } else positional.push(args[i]);
}

// --json: collect diagnostics as objects and emit one JSON array on
// stdout at exit (the §12 machine-readable report), instead of lines
const jsonMode = !!flags.get('json');
const collected: (Diag & { file: string })[] = [];
let evalOut: string | null = null;   // evaluate's canonical JSON, captured in --json mode
const printDiag = (file: string, d: Diag) => {
  if (jsonMode) { collected.push({ file, ...d }); return; }
  console.error(`${file}: ${d.severity}${d.code ? ` [${d.code}]` : ''}${d.id ? ` ${d.id}` : ''}${d.path ? ` at ${d.path}` : ''}: ${d.message}`);
};

function openUniverse(file: string) {
  const abs = absPath(file);
  const pkg = openPackageUniverse(abs);
  const preDiags: Diag[] = pkg ? [...pkg.diags, ...verifyLock(pkg)] : [];
  const { modules, entry, diags } = loadModules(abs, pkg?.resolver);
  return { modules, entry, diags: [...preDiags, ...diags] };
}

async function main(): Promise<number> {
  await initParser();
  switch (cmd) {
    case 'check': {
      if (!positional.length) return usage();
      let bad = 0;
      for (const f of positional) {
        const { modules, diags } = openUniverse(f);
        for (const d of diags) { printDiag(f, d); bad++; }
        for (const m of modules) {
          for (const d of checkModule(m.decls, m.env)) { printDiag(m.path, d); bad++; }
        }
      }
      if (bad === 0) console.error(`ok: ${positional.length} entry file(s) check clean`);
      return bad ? 1 : 0;
    }
    case 'evaluate': {
      const f = positional[0];
      if (!f) return usage();
      const { modules, entry, diags } = openUniverse(f);
      if (diags.length || !entry) { diags.forEach(d => printDiag(f, d)); return 1; }
      let bad = 0;
      for (const m of modules)
        for (const d of checkModule(m.decls, m.env)) { printDiag(m.path, d); bad++; }
      if (bad) return 1;
      const { eng, diags: ed } = runUniverse(modules, entry);
      const errs = ed.filter(d => d.severity === 'error');
      ed.forEach(d => printDiag(f, d));
      if (errs.length) return 1;
      const rootFlag = flags.get('root');
      const names = typeof rootFlag === 'string' ? [rootFlag]
        : modules.flatMap(m => m.env.outputs.map(o => o.name));
      const pieces = names.map(n => {
        const v = entry.env.roots.get(n);
        if (v === undefined) { console.error(`no output named ${n}`); process.exitCode = 1; return null; }
        return `${JSON.stringify(n)}:${eng.serialize(v, n)}`;
      }).filter(Boolean);
      const text = names.length === 1 && typeof rootFlag === 'string'
        ? eng.serialize(entry.env.roots.get(names[0]), names[0])
        : `{${pieces.join(',')}}`;
      if (jsonMode) evalOut = text; else console.log(text);
      return 0;
    }
    case 'validate': {
      const target = positional[0];
      if (!target) return usage();
      if (statSync(target).isDirectory()) {
        let ok = 0, fail = 0;
        for (const v of judgeCorpus(absPath(target))) {
          if (v.ok) ok++;
          else { fail++; console.error(`FAIL ${v.file} ${v.detail}`); }
        }
        console.error(`${ok} ok, ${fail} failed`);
        return fail ? 1 : 0;
      }
      // single-file validation: evaluate + optionally bind inputs
      const src = readFileSync(target, 'utf8');
      const { decls, errors } = parseSource(src);
      if (errors.length) { console.error(`${target}: ${errors.length} parse error(s)`); return 1; }
      const checks = checkModule(decls);
      checks.forEach(d => printDiag(target, d));
      let diags: Diag[] = [...checks];
      if (!checks.length) {
        const inputFlag = flags.get('input');
        if (typeof inputFlag === 'string') {
          const [name, file] = inputFlag.split('=');
          const { loadModules: _lm } = { loadModules };   // single-module path with a bound input
          const { modules, entry } = openUniverse(target);
          const raw = readJson(readFileSync(file, 'utf8'));
          const { diags: ed } = (await import('./module.ts')).runUniverse(modules, entry!, [{ input: name, raw }]);
          diags = [...diags, ...ed];
          ed.forEach(d => printDiag(target, d));
        } else {
          const ed = runPipeline(decls);
          diags = [...diags, ...ed];
          ed.forEach(d => printDiag(target, d));
        }
      }
      const expect = flags.get('expect-errors');
      const errCodes = diags.filter(d => d.severity === 'error').map(d => d.code ?? '');
      if (typeof expect === 'string') {
        const want = expect.split(',').map(s => s.trim()).filter(Boolean);
        const missing = want.filter(w => !errCodes.includes(w));
        const extra = errCodes.filter(c => !want.includes(c));
        if (missing.length || extra.length) {
          if (missing.length) console.error(`expected error(s) not reported: ${missing.join(', ')}`);
          if (extra.length) console.error(`unexpected error(s): ${extra.join(', ')}`);
          return 1;
        }
        console.error(`ok: expected errors reported (${want.join(', ') || 'none'})`);
        return 0;
      }
      return errCodes.length ? 1 : 0;
    }
    case 'fmt': {
      if (!positional.length) return usage();
      await initFormatter();
      let changed = 0, bad = 0;
      for (const f of positional) {
        const src = readFileSync(f, 'utf8');
        let out: string;
        try { out = format(src); }
        catch (e: any) { console.error(`${f}: ${e.message}`); bad++; continue; }
        if (out !== src) {
          changed++;
          if (flags.get('check')) console.error(`would reformat ${f}`);
          else { writeFileSync(f, out); console.error(`reformatted ${f}`); }
        }
      }
      return bad || (flags.get('check') && changed) ? 1 : 0;
    }
    default: return usage();
  }
}

function usage(): number {
  console.error(`usage:
  decl check <files...>
  decl evaluate <file> [--root <name>]
  decl validate <dir>
  decl validate <file> [--input name=doc.json] [--expect-errors E1,E2]
  decl fmt <files...> [--check]
  (check / validate accept --json: diagnostics as a JSON array on stdout)`);
  return 2;
}

process.exitCode = await main();
if (jsonMode) {
  console.log(cmd === 'evaluate'
    ? `{"ok":${process.exitCode === 0},"value":${evalOut ?? 'null'},"diagnostics":${JSON.stringify(collected)}}`
    : JSON.stringify(collected));
}
