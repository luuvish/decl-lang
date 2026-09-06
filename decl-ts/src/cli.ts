#!/usr/bin/env node
// The `decl` CLI (ROADMAP Phase 4):
//   decl check <files...>                     parse + static checks (module-aware)
//   decl evaluate <file> [--input n=doc.json]... [--output n[=file]]...
//                                             bind documents, evaluate -> JSON (stdout, or files)
//   decl validate <dir>                       judge a fixture corpus (@expect-* metadata)
//   decl validate <file> [--input n=doc.json]... [--expect-errors E1,E2]
//   decl fmt <files...> [--check]             canonical formatting (in place)
//   decl repl [file.decl] [--input n=doc.json]... [--script file]
//                                             an interactive session (repl.ts, docs/tooling/02_repl.md)
import { readFileSync, writeFileSync, existsSync, statSync, mkdirSync } from 'node:fs';
import { resolve as absPath, dirname } from 'node:path';
import { initParser } from './node.ts';
import { readJson } from './semantics.ts';
import { isYamlPath, readYaml, toJson } from './yaml.ts';
import { declaredForm, layout, emitRoot, RenderError } from './render.ts';
import type { Form, Emitted } from './render.ts';
import type { Diag } from './semantics.ts';
import { checkModule } from './checker.ts';
import { loadModules, runUniverse } from './module.ts';
import { openPackageUniverse, verifyLock } from './package.ts';
import { judgeCorpus } from './conformance.ts';
import { format, initFormatter } from './fmt.ts';
import { runRepl } from './repl.ts';
import { VERSION } from './version.ts';

const args = process.argv.slice(2);
const cmd = args.shift();
// `decl --version`: the package's version, the same string on every registry
if (cmd === '--version') {
  console.log(`decl ${VERSION}`);
  process.exit(0);
}

// `decl repl`: its own argument syntax (docs/tooling/02_repl.md)
if (cmd === 'repl') {
  await initParser();
  await initFormatter();
  process.exit(await runRepl(args));
}

const flags = new Map<string, string | boolean>();
const inputFlags: string[] = []; // --input name=doc.json, repeatable
const outputFlags: string[] = []; // --output name[=file|dir|-], repeatable
const templateFlags: string[] = []; // --template [root=]path, repeatable
const positional: string[] = [];
for (let i = 0; i < args.length; i++) {
  if (args[i].startsWith('--')) {
    const name = args[i].slice(2);
    if (
      i + 1 < args.length &&
      !args[i + 1].startsWith('--') &&
      ['output', 'input', 'expect-errors', 'format', 'indent', 'template'].includes(name)
    ) {
      if (name === 'input') inputFlags.push(args[++i]);
      else if (name === 'output') outputFlags.push(args[++i]);
      else if (name === 'template') templateFlags.push(args[++i]);
      else flags.set(name, args[++i]);
    } else flags.set(name, true);
  } else positional.push(args[i]);
}

// the documents named by --input, each bound to the module that declares
// its input (§10): `name=doc.json`, or `name=doc.yaml` read as YAML by its
// extension (docs/tooling/05_render.md §2). A usage error (bad spec,
// unknown input) exits 2 at once; a document that cannot be read or is
// not well-formed is one E6004 diagnostic against the entry file, printed
// and returned (exit 1 — and, for validate, an error code like any other)
function inputBinds(
  modules: { env: any }[],
  entryFile: string,
): { module?: any; input: string; raw: any }[] | Diag {
  const binds: { module?: any; input: string; raw: any }[] = [];
  for (const spec of inputFlags) {
    const eq = spec.indexOf('=');
    if (eq < 0) {
      console.error(`--input expects name=doc.json, got ${spec}`);
      process.exit(2);
    }
    const name = spec.slice(0, eq),
      file = spec.slice(eq + 1);
    const module = modules.find((m) => m.env.inputs.has(name));
    if (!module) {
      console.error(`no input named ${name}`);
      process.exit(2);
    }
    let text: string;
    const problem = (message: string): Diag => {
      const d: Diag = { severity: 'error', code: 'E6004', message, path: name };
      printDiag(entryFile, d);
      return d;
    };
    try {
      text = readFileSync(file, 'utf8');
    } catch {
      return problem(`bound document cannot be read: ${file}`);
    }
    let raw: any;
    if (isYamlPath(file)) {
      try {
        raw = readYaml(text);
      } catch (e: any) {
        return problem(`bound document is not well-formed YAML: ${file}: ${e.message}`);
      }
    } else {
      try {
        raw = readJson(text);
      } catch {
        return problem(`bound document is not well-formed JSON: ${file}`);
      }
    }
    binds.push({ module, input: name, raw });
  }
  return binds;
}

// --json: collect diagnostics as objects and emit one JSON array on
// stdout at exit (the §12 machine-readable report), instead of lines
const jsonMode = !!flags.get('json');
const collected: Record<string, string>[] = [];
let evalOut: string | null = null; // evaluate's canonical JSON, captured in --json mode
// one diagnostic, in the report's field order (§12.2): file, code, id,
// severity, message, path — absent fields omitted, so every implementation
// emits the same bytes
const diagJson = (file: string, d: Diag): Record<string, string> => {
  const o: Record<string, string> = { file };
  if (d.code) o.code = d.code;
  if (d.id) o.id = d.id;
  o.severity = d.severity;
  o.message = d.message;
  o.path = d.path;
  return o;
};
const printDiag = (file: string, d: Diag) => {
  if (jsonMode) {
    collected.push(diagJson(file, d));
    return;
  }
  console.error(
    `${file}: ${d.severity}${d.code ? ` [${d.code}]` : ''}${d.id ? ` ${d.id}` : ''}${d.path ? ` at ${d.path}` : ''}: ${d.message}`,
  );
};
// the file a diagnostic is reported against: the entry module by the path
// given on the command line, any other module by its absolute path
const fileTag = (given: string, entryPath: string | undefined, modulePath: string) =>
  modulePath === entryPath ? given : modulePath;

// --format json|yaml, --indent n, --pretty: the layout of every document
// emitted (docs/tooling/05_render.md §3.4, §4); null for a usage error
function formOverrides(): { format?: 'json' | 'yaml'; indent?: number } | null {
  const out: { format?: 'json' | 'yaml'; indent?: number } = {};
  const format = flags.get('format');
  if (format !== undefined) {
    if (format !== 'json' && format !== 'yaml') {
      console.error(`--format expects json or yaml, got ${format === true ? 'nothing' : format}`);
      return null;
    }
    if (format === 'yaml' && jsonMode) {
      console.error('--json reports are JSON: it cannot be combined with --format yaml');
      return null;
    }
    out.format = format;
  }
  const indent = flags.get('indent');
  if (indent !== undefined && flags.get('pretty')) {
    console.error('--indent and --pretty exclude each other');
    return null;
  }
  if (indent !== undefined) {
    if (typeof indent !== 'string' || !/^(0|[1-9][0-9]?)$/.test(indent) || Number(indent) > 16) {
      console.error(
        `--indent expects an integer in 0..16, got ${indent === true ? 'nothing' : indent}`,
      );
      return null;
    }
    out.indent = Number(indent);
  } else if (flags.get('pretty')) out.indent = 2;
  return out;
}

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
      let bad = 0,
        reported = 0;
      for (const f of positional) {
        const { modules, entry, diags } = openUniverse(f);
        for (const d of diags) {
          printDiag(f, d);
          bad++;
          reported++;
        }
        for (const m of modules) {
          for (const d of checkModule(m.decls, m.env)) {
            printDiag(fileTag(f, entry?.path, m.path), d);
            if (d.severity === 'error') bad++; // a warning (W0001) is reported, not a failure
            reported++;
          }
        }
      }
      if (reported === 0) console.error(`ok: ${positional.length} entry file(s) check clean`);
      return bad ? 1 : 0;
    }
    case 'evaluate': {
      const f = positional[0];
      if (!f) return usage();
      // what to emit, and where (§5.5, docs/tooling/05_render.md §3.2): each
      // `--output name[=file|dir|-]` names a root — an output, or an input
      // bound by --input or demanded through its fallback — and where its
      // document goes: the file given, `-` for stdout, or, alone, the file
      // the root's `@render` declares, else stdout; with no --output, the
      // entry module's exported outputs, as one object keyed by name, on
      // stdout
      const targets: { name: string; dest?: string }[] = [];
      for (const spec of outputFlags) {
        const eq = spec.indexOf('=');
        const name = eq < 0 ? spec : spec.slice(0, eq),
          dest = eq < 0 ? undefined : spec.slice(eq + 1);
        if (!name || dest === '') {
          console.error(`--output expects name or name=file, got ${spec}`);
          return 2;
        }
        targets.push({ name, dest });
      }
      // the overrides of the declared forms (docs/tooling/05_render.md §3.4)
      const over = formOverrides();
      if (over === null) return 2;
      const { modules, entry, diags } = openUniverse(f);
      if (diags.length || !entry) {
        diags.forEach((d) => printDiag(f, d));
        return 1;
      }
      let bad = 0;
      for (const m of modules)
        for (const d of checkModule(m.decls, m.env)) {
          printDiag(fileTag(f, entry.path, m.path), d);
          if (d.severity === 'error') bad++;
        }
      if (bad) return 1;
      // each target's declared form (§3): an invalid @render is E7004 at
      // emission and the root is not emitted; the others still are
      // each target's declared form (§3): an invalid @render is E7004 at
      // emission and the root is not emitted; the others still are
      const declOf = (n: string) => {
        for (const m of modules)
          for (const d of m.decls) if (d.d === 'output' && d.name === n) return { d, m };
        return null;
      };
      const forms = new Map<string, Form | { error: string }>();
      const moduleDirs = new Map<string, string>();
      for (const t of targets) {
        const found = declOf(t.name);
        forms.set(t.name, found ? declaredForm(found.d) : { format: 'json' });
        moduleDirs.set(t.name, found ? dirname(found.m.path) : dirname(absPath(f)));
      }
      // the templates named by --template (§3.4): `[root=]path`, `-` for
      // standard input; a root named must be emitted, and once
      const tplFlags = new Map<string, string>(); // '' names every root
      for (const spec of templateFlags) {
        const m = /^([A-Za-z_][A-Za-z0-9_]*)=([\s\S]+)$/.exec(spec);
        const root = m ? m[1] : '';
        const path = m ? m[2] : spec;
        if (!path) {
          console.error(`--template expects [root=]path, got ${spec}`);
          return 2;
        }
        if (root && !targets.some((t) => t.name === root)) {
          console.error(`--template ${root}=: no --output ${root}`);
          return 2;
        }
        if (tplFlags.has(root)) {
          console.error(root ? `--template ${root}= given twice` : '--template given twice');
          return 2;
        }
        tplFlags.set(root, path);
      }
      // destinations (§3.2, §6): one root at most to stdout; a fan-out root
      // goes to a directory, never to stdout
      const fanOut = (t: { name: string; dest?: string }) => {
        const form = forms.get(t.name)!;
        return !('error' in form) && !!form.each;
      };
      const toStdout = (t: { name: string; dest?: string }) => {
        const form = forms.get(t.name)!;
        if (t.dest === '-') return true;
        if (t.dest !== undefined) return false;
        return !('error' in form) && !form.file;
      };
      for (const t of targets) {
        if (!fanOut(t)) continue;
        if (t.dest === '-') {
          console.error(`--output ${t.name}=-: a fan-out root cannot go to stdout`);
          return 2;
        }
        if (t.dest === undefined && !(forms.get(t.name) as Form).file) {
          console.error(
            `--output ${t.name}: a fan-out root needs a directory (${t.name}=dir, or file in @render)`,
          );
          return 2;
        }
      }
      if (targets.filter((t) => !fanOut(t) && toStdout(t)).length > 1) {
        console.error('--output: at most one document can go to stdout');
        return 2;
      }
      const binds = inputBinds(modules, f);
      if (!Array.isArray(binds)) return 1;
      const { eng, diags: ed } = runUniverse(modules, entry, binds);
      const errs = ed.filter((d) => d.severity === 'error');
      ed.forEach((d) => printDiag(f, d));
      if (errs.length) return 1;
      const names = targets.length
        ? targets.map((t) => t.name)
        : entry.env.outputs.filter((o: any) => o.exported).map((o: any) => o.name);
      let missing = 0;
      for (const n of names)
        if (!entry.env.roots.has(n)) {
          console.error(`no root named ${n}`);
          missing++;
        }
      if (missing) return 1;
      const doc = (n: string) => readJson(eng.serialize(entry.env.roots.get(n), n));
      let text: string | null = null;
      if (targets.length === 0) {
        if (names.length === 0)
          console.error(`${f}: exports no output; --output <name> selects a root`);
        const all = { __jobj: true, entries: names.map((n) => [n, doc(n)]) };
        // a --json report carries the document itself, whatever the layout
        text = jsonMode
          ? toJson(all) + '\n'
          : layout(all, { format: over.format ?? 'json', indent: over.indent });
      } else {
        // templates are read once (§3.3): by absolute path, or standard input
        const texts = new Map<string, string | null>();
        const readTpl = (abs: string): string | null => {
          if (!texts.has(abs)) {
            try {
              texts.set(abs, readFileSync(abs, 'utf8'));
            } catch {
              texts.set(abs, null);
            }
          }
          return texts.get(abs)!;
        };
        let stdinText: string | null = null;
        const templateFor = (
          t: { name: string },
          form: Form,
        ): { path: string; text: string; dir: string } | null | { unreadable: string } => {
          const given = tplFlags.get(t.name) ?? tplFlags.get('');
          if (given !== undefined) {
            if (given === '-') {
              if (stdinText === null) {
                try {
                  stdinText = readFileSync(0, 'utf8');
                } catch {
                  return { unreadable: '-' };
                }
              }
              return { path: '-', text: stdinText, dir: process.cwd() };
            }
            const abs = absPath(given);
            const body = readTpl(abs);
            return body === null
              ? { unreadable: given }
              : { path: given, text: body, dir: dirname(abs) };
          }
          if (!form.template) return null;
          const abs = absPath(moduleDirs.get(t.name)!, form.template);
          const body = readTpl(abs);
          return body === null
            ? { unreadable: form.template }
            : { path: form.template, text: body, dir: dirname(abs) };
        };
        for (const t of targets) {
          const form = forms.get(t.name)!;
          if ('error' in form) {
            printDiag(f, { severity: 'error', code: 'E7004', message: form.error, path: t.name });
            bad++;
            continue;
          }
          const tpl = templateFor(t, form);
          if (tpl && 'unreadable' in tpl) {
            printDiag(tpl.unreadable, {
              severity: 'error',
              code: 'E7003',
              message: 'template cannot be read',
              path: t.name,
            });
            bad++;
            continue;
          }
          let em: Emitted;
          try {
            em = emitRoot({
              eng,
              menv: entry.env,
              rootName: t.name,
              value: entry.env.roots.get(t.name),
              form,
              format: over.format,
              indent: over.indent,
              template: tpl ?? undefined,
              readTemplate: readTpl,
            });
          } catch (e: any) {
            if (e instanceof RenderError) {
              printDiag(e.file ?? f, e.diag());
              bad++;
              continue;
            }
            throw e;
          }
          const dest = t.dest === '-' ? undefined : (t.dest ?? form.file);
          if (em.kind === 'many') {
            // a fan-out's files, in element order, under the directory
            for (const [rel, body] of em.files) {
              const file = absPath(dest!, rel);
              try {
                mkdirSync(dirname(file), { recursive: true });
                writeFileSync(file, body);
              } catch {
                console.error(`cannot write ${file}`);
                return 1;
              }
            }
            continue;
          }
          if (dest === undefined) {
            // the report's value: the document itself, or a template's text as a string
            text = jsonMode
              ? (tpl ? JSON.stringify(em.text) : toJson(doc(t.name))) + '\n'
              : em.text;
            continue;
          }
          try {
            mkdirSync(dirname(dest), { recursive: true });
            writeFileSync(dest, em.text);
          } catch {
            console.error(`cannot write ${dest}`);
            return 1;
          }
        }
      }
      if (jsonMode) evalOut = text === null ? null : text.trimEnd();
      else if (text !== null) process.stdout.write(text);
      return bad ? 1 : 0;
    }
    case 'validate': {
      const target = positional[0];
      if (!target) return usage();
      if (flags.get('expect-errors') === true) {
        console.error('--expect-errors expects a list of codes: E1,E2');
        return 2;
      }
      if (existsSync(target) && statSync(target).isDirectory()) {
        let ok = 0,
          fail = 0;
        for (const v of judgeCorpus(absPath(target))) {
          if (v.ok) ok++;
          else {
            fail++;
            console.error(`FAIL ${v.file} ${v.detail}`);
          }
        }
        console.error(`${ok} ok, ${fail} failed`);
        return fail ? 1 : 0;
      }
      // single-file validation, module-aware like `check` and `evaluate`:
      // load the universe, check every module, then evaluate with the
      // --input documents bound (none bound is fine: fallbacks apply)
      const { modules, entry, diags: loadDiags } = openUniverse(target);
      loadDiags.forEach((d) => printDiag(target, d));
      let diags: Diag[] = [...loadDiags];
      if (!loadDiags.length && entry) {
        const checks: Diag[] = [];
        for (const m of modules)
          for (const d of checkModule(m.decls, m.env)) {
            printDiag(fileTag(target, entry.path, m.path), d);
            checks.push(d);
          }
        diags = checks;
        if (!checks.some((d) => d.severity === 'error')) {
          const binds = inputBinds(modules, target);
          if (Array.isArray(binds)) {
            const { diags: ed } = runUniverse(modules, entry, binds);
            diags = [...checks, ...ed];
            ed.forEach((d) => printDiag(target, d));
          } else diags = [...checks, binds];
        }
      }
      const expect = flags.get('expect-errors');
      const errCodes = diags.filter((d) => d.severity === 'error').map((d) => d.code ?? '');
      if (typeof expect === 'string') {
        const want = expect
          .split(',')
          .map((s) => s.trim())
          .filter(Boolean);
        const missing = want.filter((w) => !errCodes.includes(w));
        const extra = errCodes.filter((c) => !want.includes(c));
        if (missing.length || extra.length) {
          if (missing.length)
            console.error(`expected error(s) not reported: ${missing.join(', ')}`);
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
      let changed = 0,
        bad = 0;
      for (const f of positional) {
        let src: string;
        try {
          src = readFileSync(f, 'utf8');
        } catch {
          console.error(`${f}: cannot be read`);
          bad++;
          continue;
        }
        let out: string;
        try {
          out = format(src);
        } catch (e: any) {
          console.error(`${f}: ${e.message}`);
          bad++;
          continue;
        }
        if (out !== src) {
          changed++;
          if (flags.get('check')) console.error(`would reformat ${f}`);
          else {
            writeFileSync(f, out);
            console.error(`reformatted ${f}`);
          }
        }
      }
      return bad || (flags.get('check') && changed) ? 1 : 0;
    }
    default:
      return usage();
  }
}

function usage(): number {
  console.error(`usage:
  decl --version
  decl check <files...>
  decl evaluate <file> [--input name=doc.(json|yaml)]... [--output name[=file|dir|-]]...
                       [--format json|yaml] [--indent n | --pretty] [--template [root=]path]...
  decl validate <dir>
  decl validate <file> [--input name=doc.(json|yaml)]... [--expect-errors E1,E2]
  decl fmt <files...> [--check]
  decl repl [file.decl] [--input name=doc.(json|yaml)]... [--script session.txt | --script -] [--compact]
  (check / validate accept --json: diagnostics as a JSON array on stdout)`);
  return 2;
}

process.exitCode = await main();
// a usage error (exit 2) prints its line or the usage text, and no report
if (jsonMode && process.exitCode !== 2) {
  console.log(
    cmd === 'evaluate'
      ? `{"ok":${process.exitCode === 0},"value":${evalOut ?? 'null'},"diagnostics":${JSON.stringify(collected)}}`
      : JSON.stringify(collected),
  );
}
