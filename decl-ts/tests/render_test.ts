// The render corpus (tests/render) through the reference: the cases of
// cases.json (templates, @render, fan-out — the recorded outcome, in the
// shape of tests/cli), the format goldens of formats.json (`--format
// yaml`, `--indent n`, and the YAML read back to the golden document),
// every golden document bound from its YAML twin under inputs/, and the
// documents under invalid/ that the reader must refuse with their
// messages (tests/render/README.md).
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, mkdirSync, existsSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { check, total, root, cli, firstDiff } from './common/check.ts';
import { readJson } from '../src/semantics.ts';
import { readYaml, toJson } from '../src/yaml.ts';

type Entry = {
  module?: string;
  markdown?: string;
  inputs?: string[];
  output?: string;
  rejected?: boolean;
  golden: string;
};
const manifest: Entry[] = JSON.parse(
  readFileSync(join(root, 'tests/golden/manifest.json'), 'utf8'),
);
const tmp = mkdtempSync(join(tmpdir(), 'decl-render-'));
const moduleOf = (e: Entry): string => {
  if (e.module) return e.module;
  const md = readFileSync(join(root, e.markdown!), 'utf8');
  const src = [...md.matchAll(/```decl\n([\s\S]*?)```/g)].map((m) => m[1]).join('\n');
  const p = join(tmp, 'guide.decl');
  writeFileSync(p, src);
  return p;
};
const run = (args: string[]) => spawnSync('node', [cli, ...args], { encoding: 'utf8', cwd: root });
const argsOf = (e: Entry, inputs = e.inputs) => {
  const args = [e.rejected ? 'validate' : 'evaluate', moduleOf(e)];
  for (const spec of inputs ?? []) args.push('--input', spec);
  if (e.output) args.push('--output', e.output);
  return args;
};

console.log('== render: formats.json — the layouts of the goldens ==');
type Format = { golden: string; yaml: string; indent: Record<string, string> };
const formats: Format[] = JSON.parse(readFileSync(join(root, 'tests/render/formats.json'), 'utf8'));
for (const f of formats) {
  const e = manifest.find((m) => m.golden === f.golden)!;
  const golden = readFileSync(join(root, f.golden), 'utf8');
  const yaml = readFileSync(join(root, f.yaml), 'utf8');
  const r = run([...argsOf(e), '--format', 'yaml']);
  check(`${f.yaml}: --format yaml`, r.status === 0 && r.stdout === yaml, firstDiff(yaml, r.stdout));
  const back = toJson(readYaml(yaml)) + '\n';
  check(`${f.yaml}: read back to the golden`, back === golden, firstDiff(golden, back));
  for (const [n, file] of Object.entries(f.indent)) {
    const want = readFileSync(join(root, file), 'utf8');
    const ri = run([...argsOf(e), '--indent', n]);
    check(`${file}: --indent ${n}`, ri.status === 0 && ri.stdout === want, firstDiff(want, ri.stdout));
    const parsed = toJson(readJson(want)) + '\n';
    check(`${file}: parses back to the golden`, parsed === golden, firstDiff(golden, parsed));
  }
}

console.log('== render: inputs/ — every bound document from its YAML twin ==');
const twin = (spec: string) =>
  spec.replace(/=tests\/golden\/inputs\/(.*)\.json$/, '=tests/render/inputs/$1.yaml');
for (const e of manifest) {
  if (!e.inputs?.length) continue;
  const inputs = e.inputs.map(twin);
  if (inputs.every((s, i) => s === e.inputs![i])) continue;
  const r = run(argsOf(e, inputs));
  const expected = readFileSync(join(root, e.golden), 'utf8');
  const got = e.rejected ? r.stderr : r.stdout;
  check(
    `${e.golden} from ${inputs.map((s) => s.split('=')[1]).join(', ')}`,
    r.status === (e.rejected ? 1 : 0) && got === expected,
    `exit ${r.status}; ${firstDiff(expected, got)}`,
  );
}

console.log('== render: invalid/ — what the reader refuses ==');
type Invalid = { file: string; message: string };
const invalid: Invalid[] = JSON.parse(
  readFileSync(join(root, 'tests/render/invalid/cases.json'), 'utf8'),
);
for (const c of invalid) {
  const file = `tests/render/invalid/${c.file}`;
  let reader = '';
  try {
    readYaml(readFileSync(join(root, file), 'utf8'));
  } catch (e: any) {
    reader = e.message;
  }
  check(`${c.file}: the reader says "${c.message}"`, reader === c.message, `got "${reader}"`);
  const r = run(['validate', 'tests/render/invalid/doc.decl', '--input', `doc=${file}`]);
  const want = `tests/render/invalid/doc.decl: error [E6004] at doc: bound document is not well-formed YAML: ${file}: ${c.message}\n`;
  check(`${c.file}: E6004 on the command line`, r.status === 1 && r.stderr === want, firstDiff(want, r.stderr));
}
console.log('== render: cases.json — templates, @render, fan-out (the recorded outcome) ==');
type Case = {
  name: string;
  files?: Record<string, string>;
  args: string[];
  stdin?: string;
  exit: number;
  stdout: string;
  stderr: string;
  after?: Record<string, string | null>;
};
const version = JSON.parse(readFileSync(join(root, 'decl-ts/package.json'), 'utf8')).version;
const cases: Case[] = JSON.parse(readFileSync(join(root, 'tests/render/cases.json'), 'utf8'));
for (const c of cases) {
  const dir = mkdtempSync(join(tmpdir(), 'decl-render-'));
  for (const [name, text] of Object.entries(c.files ?? {})) {
    mkdirSync(dirname(join(dir, name)), { recursive: true });
    writeFileSync(join(dir, name), text);
  }
  const args = c.args.map((a) => a.split('<dir>').join(dir));
  const r = spawnSync(process.execPath, [cli, ...args], {
    encoding: 'utf8',
    cwd: root,
    input: c.stdin ?? '',
  });
  const norm = (s: string) => s.split(dir).join('<dir>').split(version).join('<version>');
  const got = { exit: r.status, stdout: norm(r.stdout), stderr: norm(r.stderr) };
  const same = got.exit === c.exit && got.stdout === c.stdout && got.stderr === c.stderr;
  check(c.name, same, same ? '' : JSON.stringify({ got, want: [c.exit, c.stdout, c.stderr] }));
  for (const [name, text] of Object.entries(c.after ?? {})) {
    const p = join(dir, name);
    const actual = existsSync(p) ? readFileSync(p, 'utf8') : null;
    check(`${c.name}: ${name} afterwards`, actual === text, JSON.stringify({ actual, text }));
  }
  rmSync(dir, { recursive: true, force: true });
}
total();
