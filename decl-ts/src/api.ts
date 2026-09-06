// The high-level API of decl-lang: the operations the `decl` command line
// offers, in the same vocabulary — `evaluate` binds inputs and returns
// outputs, `check`, `validate`, `formatSource` — for programs that would
// otherwise assemble parser, checker, and engine by hand (those pieces
// are exported too, from `decl-lang/core`). It reads files, so it lives
// in the Node entry. The Python package and the Rust crate offer the same
// functions with the same semantics.
import { readFileSync } from 'node:fs';
import { resolve as absPath } from 'node:path';
import { initParser } from './node.ts';
import { parseSource } from './parse.ts';
import { checkModule } from './checker.ts';
import { loadModules, runUniverse } from './module.ts';
import type { Module } from './module.ts';
import { openPackageUniverse, verifyLock } from './package.ts';
import { runPipeline } from './pipeline.ts';
import { readJson } from './semantics.ts';
import type { Diag } from './semantics.ts';
import { format, initFormatter } from './fmt.ts';

/** one diagnostic, in the report's field order (§12.2) */
export type Diagnostic = {
  file: string;
  code?: string;
  id?: string;
  severity: string;
  message: string;
  path: string;
};
export type JsonValue =
  null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
/** a document to bind to an input: the path of a JSON file (a string), or the value itself */
export type InputDocument = string | JsonValue;
export type EvaluateOptions = {
  /** documents to bind, by input name */
  inputs?: Record<string, InputDocument>;
  /** the roots to return — outputs, or inputs bound here or demanded through their fallback; default: the entry module's exported outputs (§5.5) */
  outputs?: string[];
};

/** an operation failed; `diagnostics` carries the report (empty for a usage error) */
export class DeclError extends Error {
  diagnostics: Diagnostic[];
  constructor(message: string, diagnostics: Diagnostic[] = []) {
    super(message);
    this.name = 'DeclError';
    this.diagnostics = diagnostics;
  }
}

let ready: Promise<void> | null = null;
const init = () => (ready ??= initParser());

const tagged = (file: string, d: Diag): Diagnostic => {
  const o: Diagnostic = { file } as Diagnostic;
  if (d.code) o.code = d.code;
  if (d.id) o.id = d.id;
  o.severity = d.severity;
  o.message = d.message;
  o.path = d.path;
  return o;
};
const fail = (fallback: string, diagnostics: Diagnostic[]): never => {
  throw new DeclError(diagnostics.length ? diagnostics[0].message : fallback, diagnostics);
};
// the file a diagnostic is reported against: the entry module by the path
// given, any other module by its absolute path
const fileTag = (given: string, entryPath: string | undefined, modulePath: string) =>
  modulePath === entryPath ? given : modulePath;

function openUniverse(file: string) {
  const abs = absPath(file);
  const pkg = openPackageUniverse(abs);
  const pre: Diag[] = pkg ? [...pkg.diags, ...verifyLock(pkg)] : [];
  const { modules, entry, diags } = loadModules(abs, pkg?.resolver);
  return { modules, entry, diags: [...pre, ...diags] };
}

// the documents to bind, each to the module that declares its input (§10)
function bindInputs(modules: Module[], file: string, inputs: Record<string, InputDocument>) {
  const binds: { module?: Module; input: string; raw: any }[] = [];
  for (const [name, doc] of Object.entries(inputs)) {
    const module = modules.find((m) => m.env.inputs.has(name));
    if (!module) throw new DeclError(`no input named ${name}`);
    let text: string;
    if (typeof doc === 'string') {
      try {
        text = readFileSync(doc, 'utf8');
      } catch {
        fail('', [
          {
            file,
            code: 'E6004',
            severity: 'error',
            message: `bound document cannot be read: ${doc}`,
            path: name,
          },
        ]);
      }
    } else text = JSON.stringify(doc);
    let raw: any;
    try {
      raw = readJson(text!);
    } catch {
      fail('', [
        {
          file,
          code: 'E6004',
          severity: 'error',
          message: `bound document is not well-formed JSON: ${typeof doc === 'string' ? doc : name}`,
          path: name,
        },
      ]);
    }
    binds.push({ module, input: name, raw });
  }
  return binds;
}

/**
 * Evaluate a module: bind the input documents, run the pipeline, and return
 * the requested roots' documents (parsed JSON) by name. Throws DeclError
 * with the diagnostics on any error-severity outcome.
 */
export async function evaluate(
  path: string,
  opts: EvaluateOptions = {},
): Promise<Record<string, JsonValue>> {
  await init();
  const { modules, entry, diags } = openUniverse(path);
  if (diags.length || !entry)
    fail(
      `${path}: cannot be loaded`,
      diags.map((d) => tagged(path, d)),
    );
  const checks = modules.flatMap((m) =>
    checkModule(m.decls, m.env).map((d) => tagged(fileTag(path, entry!.path, m.path), d)),
  );
  if (checks.some((d) => d.severity === 'error')) fail('', checks);
  const { eng, diags: ed } = runUniverse(
    modules,
    entry!,
    bindInputs(modules, path, opts.inputs ?? {}),
  );
  const report = ed.map((d) => tagged(path, d));
  if (report.some((d) => d.severity === 'error')) fail('', report);
  const names =
    opts.outputs ?? entry!.env.outputs.filter((o: any) => o.exported).map((o: any) => o.name);
  const out: Record<string, JsonValue> = {};
  for (const n of names) {
    if (!entry!.env.roots.has(n)) throw new DeclError(`no root named ${n}`, report);
    out[n] = JSON.parse(eng.serialize(entry!.env.roots.get(n), n));
  }
  return out;
}

/** Parse and statically check entry files (module-aware); empty means clean. */
export async function check(...paths: string[]): Promise<Diagnostic[]> {
  await init();
  const out: Diagnostic[] = [];
  for (const f of paths) {
    const { modules, entry, diags } = openUniverse(f);
    out.push(...diags.map((d) => tagged(f, d)));
    for (const m of modules)
      out.push(
        ...checkModule(m.decls, m.env).map((d) => tagged(fileTag(f, entry?.path, m.path), d)),
      );
  }
  return out;
}

/**
 * Validate a file: static checks, then evaluation with the input documents
 * bound; returns every diagnostic (all severities). Throws DeclError when
 * the file does not parse.
 */
export async function validate(
  path: string,
  opts: { inputs?: Record<string, InputDocument> } = {},
): Promise<Diagnostic[]> {
  await init();
  let text: string;
  try {
    text = readFileSync(path, 'utf8');
  } catch {
    throw new DeclError(`${path}: cannot be read`);
  }
  const { decls, errors } = parseSource(text);
  if (errors.length) throw new DeclError(`${path}: ${errors.length} parse error(s)`);
  const checks = checkModule(decls).map((d) => tagged(path, d));
  if (checks.some((d) => d.severity === 'error')) return checks;
  if (opts.inputs && Object.keys(opts.inputs).length) {
    const { modules, entry } = openUniverse(path);
    return [
      ...checks,
      ...runUniverse(modules, entry!, bindInputs(modules, path, opts.inputs)).diags.map((d) =>
        tagged(path, d),
      ),
    ];
  }
  return [...checks, ...runPipeline(decls).diags.map((d) => tagged(path, d))];
}

/** The canonical formatting of a source text; throws DeclError when it does not parse. */
export async function formatSource(text: string): Promise<string> {
  await initFormatter();
  try {
    return format(text);
  } catch (e: any) {
    throw new DeclError(e.message);
  }
}
