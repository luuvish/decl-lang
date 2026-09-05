// The API corpus (tests/api/cases.json) through the high-level API
// (src/api.ts): every case run from the repository root, the answers as
// one JSON array in the form tests/api/README.md fixes — what the parity
// harness diffs across the three drivers (this file, decl-rs/examples/
// api_corpus.rs, decl-py/scripts/api_corpus.py) and what tests/api_test.ts
// compares with tests/api/expected.json.
//
//     node decl-ts/scripts/api-corpus.ts
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { evaluate, check, validate, formatSource, DeclError } from '../src/index.ts';
import type { InputDocument } from '../src/api.ts';

export const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

export type Case = {
  name: string;
  evaluate?: string;
  check?: string[];
  validate?: string;
  format_source?: string;
  inputs?: Record<string, { file?: string; json?: unknown }>;
  outputs?: string[];
};
export type Answer =
  | { name: string; ok: true; value: unknown }
  | { name: string; ok: false; message: string; diagnostics: unknown[] };

// a document is a file, named by its path, or the value itself
const documents = (inputs: Case['inputs']): Record<string, InputDocument> =>
  Object.fromEntries(
    Object.entries(inputs ?? {}).map(([k, v]) => [k, 'file' in v ? v.file! : (v.json as any)]),
  );

export async function runCase(c: Case): Promise<Answer> {
  try {
    let value: unknown;
    if (c.evaluate !== undefined) {
      const opts: { inputs?: Record<string, InputDocument>; outputs?: string[] } = {};
      if (c.inputs) opts.inputs = documents(c.inputs);
      if (c.outputs) opts.outputs = c.outputs;
      value = await evaluate(c.evaluate, opts);
    } else if (c.check) value = await check(...c.check);
    else if (c.validate !== undefined)
      value = await validate(c.validate, c.inputs ? { inputs: documents(c.inputs) } : {});
    else if (c.format_source !== undefined) value = await formatSource(c.format_source);
    else throw new Error(`unknown call in ${c.name}`);
    return { name: c.name, ok: true, value };
  } catch (e) {
    if (!(e instanceof DeclError)) throw e;
    return { name: c.name, ok: false, message: e.message, diagnostics: e.diagnostics };
  }
}

/** every case's answer, the cases' paths read from the repository root */
export async function runAll(): Promise<{ cases: Case[]; answers: Answer[] }> {
  process.chdir(root);
  const cases: Case[] = JSON.parse(readFileSync('tests/api/cases.json', 'utf8'));
  const answers: Answer[] = [];
  for (const c of cases) answers.push(await runCase(c));
  return { cases, answers };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  console.log(JSON.stringify((await runAll()).answers, null, 2));
}
