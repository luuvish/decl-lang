// The command-line corpus (tests/cli/cases.json) through the reference
// `decl` and `decl-lsp`: every case's exit status, standard output,
// standard error, and the files it leaves, against the recorded
// expectations (tests/cli/README.md).
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, existsSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { check, total, root, cli, lspServer } from './common/check.ts';

type Case = {
  name: string;
  files?: Record<string, string>;
  program?: string;
  args: string[];
  stdin?: string;
  exit: number;
  stdout: string;
  stderr: string;
  after?: Record<string, string | null>;
};

const version = JSON.parse(readFileSync(join(root, 'decl-ts/package.json'), 'utf8')).version;
const cases: Case[] = JSON.parse(readFileSync(join(root, 'tests/cli/cases.json'), 'utf8'));
console.log('== cli: the cases of tests/cli ==');
for (const c of cases) {
  const dir = mkdtempSync(join(tmpdir(), 'decl-cli-'));
  for (const [name, text] of Object.entries(c.files ?? {})) writeFileSync(join(dir, name), text);
  const program = c.program === 'decl-lsp' ? lspServer : cli;
  const args = c.args.map((a) => a.split('<dir>').join(dir));
  const r = spawnSync(process.execPath, [program, ...args], {
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
