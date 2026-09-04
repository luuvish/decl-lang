// `decl repl` (ROADMAP Phase 6): the session corpus replayed against its
// transcripts (tests/repl/<case>/), then the commands the corpus leaves
// out because they write files or depend on the clock.
import { spawnSync } from 'node:child_process';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync, writeFileSync, mkdtempSync, readdirSync, existsSync, copyFileSync } from 'node:fs';
import { tmpdir } from 'node:os';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const cli = join(root, 'decl-ts/src/cli.ts');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};
const run = (args: string[], opts: { cwd?: string; input?: string } = {}) => {
  const r = spawnSync('node', [cli, 'repl', ...args], { encoding: 'utf8', cwd: opts.cwd ?? root, input: opts.input });
  return { code: r.status ?? -1, out: r.stdout, err: r.stderr };
};

console.log('== repl: the session corpus ==');
for (const c of readdirSync(join(root, 'tests/repl')).sort()) {
  const dir = join(root, 'tests/repl', c);
  if (!existsSync(join(dir, 'session.txt'))) continue;
  const entry = existsSync(join(dir, 'main.decl')) ? [`tests/repl/${c}/main.decl`] : [];
  const r = run([...entry, '--script', `tests/repl/${c}/session.txt`]);
  const want = readFileSync(join(dir, 'transcript.txt'), 'utf8');
  const wantCode = /^error: /m.test(want) ? 1 : 0;
  check(`${c}: transcript`, r.out === want, firstDiff(want, r.out));
  check(`${c}: exit ${wantCode}`, r.code === wantCode, `got ${r.code} ${r.err.slice(0, 200)}`);
}
function firstDiff(a: string, b: string): string {
  const al = a.split('\n'), bl = b.split('\n');
  for (let i = 0; i < Math.max(al.length, bl.length); i++)
    if (al[i] !== bl[i]) return `line ${i + 1}: expected ${JSON.stringify(al[i])}, got ${JSON.stringify(bl[i])}`;
  return '';
}

console.log('== repl: files, the clock, and the command line ==');
{
  const dir = mkdtempSync(join(tmpdir(), 'decl-repl-'));
  for (const f of ['main.decl', 'doc.json']) copyFileSync(join(root, 'tests/repl/documents', f), join(dir, f));
  const session = [
    ':bind deployed=doc.json',
    ':update deployed.port = 9100',
    'y = deployed.port + 1',
    ':save deployed=saved.json',
    ':write scratch.decl',
    ':history log.txt',
    ':time',
  ].join('\n') + '\n';
  writeFileSync(join(dir, 'session.txt'), session);
  const r = run(['main.decl', '--script', 'session.txt'], { cwd: dir });
  check('files: session runs clean', r.code === 0, r.out + r.err);
  check(':save writes the edited document', readFileSync(join(dir, 'saved.json'), 'utf8') === '{"port":9100,"name":"doc"}\n');
  check(':write writes the scratch module', readFileSync(join(dir, 'scratch.decl'), 'utf8') === 'output y: 2..65536 = deployed.port + 1\n', readFileSync(join(dir, 'scratch.decl'), 'utf8'));
  check(':history file writes a replayable session', readFileSync(join(dir, 'log.txt'), 'utf8') === ':bind deployed=doc.json\n:update deployed.port = 9100\ny = deployed.port + 1\n');
  check(':time reports milliseconds', /^total \d+\.\d ms \(load \d+\.\d ms, check \d+\.\d ms, bind \d+\.\d ms, evaluate \d+\.\d ms\)$/m.test(r.out), r.out);
  // the written log replays to the same state
  const again = run(['main.decl', '--script', 'log.txt'], { cwd: dir });
  check('the written log replays', again.code === 0 && again.out.split('\n').every(l => !l.startsWith('error')), again.out);

  // --input binds before the first line, --script - reads standard input, --compact prints the wire form
  const piped = run(['main.decl', '--input', 'deployed=doc.json', '--script', '-', '--compact'], { cwd: dir, input: 'deployed\n' });
  check('--input, --script -, --compact', piped.out === '> deployed\n{"port":9000,"name":"doc","replicas":1,"label":"doc:9000"}\n(partial)\n', piped.out);

  // :reload re-reads the disk and :undo restores the previous text; :load starts over
  writeFileSync(join(dir, 'session2.txt'), 'site.port\n:reload\nsite.port\n:undo\nsite.port\n:redo\nsite.port\n:load main.decl\n:history\n');
  const before = readFileSync(join(dir, 'main.decl'), 'utf8');
  writeFileSync(join(dir, 'main.decl'), before.replace('port: 443', 'port: 8443'));
  // the session snapshot is taken at :load, so the first answer is the text as it was when the script started
  const rl = run(['main.decl', '--script', 'session2.txt'], { cwd: dir });
  check(':reload / :undo / :redo / :load', rl.out === '> site.port\n8443\n(partial)\n> :reload\n> site.port\n8443\n(partial)\n> :undo\n> site.port\n8443\n(partial)\n> :redo\n> site.port\n8443\n(partial)\n> :load main.decl\n> :history\n* 0  (start)\n', rl.out);
  writeFileSync(join(dir, 'main.decl'), before);

  // usage errors
  check('unknown option is a usage error', run(['--nope']).code === 2);
  check('a missing script is a usage error', run(['--script', join(dir, 'nope.txt')]).code === 2);
  check('a bad --input spec is a usage error', run(['main.decl', '--input', 'x']).code === 2);
}

console.log(`TOTAL ${pass} ok, ${fail} failed`);
process.exit(fail ? 1 : 0);
