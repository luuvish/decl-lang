// `decl repl`: the session corpus (tests/repl/<case>/), each case replayed
// in a fresh copy of its directory against its transcript and the files it
// leaves (tests/repl/README.md), and again under DECL_FULL_RECOMPUTE=1 —
// the incremental step is observationally identical to a full
// recomputation (docs/tooling/02_repl.md §6).
import { spawnSync } from 'node:child_process';
import { readFileSync, readdirSync, existsSync, mkdtempSync, cpSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { check, total, root, cli, firstDiff } from './common/check.ts';

/** milliseconds are the clock's, not the session's */
const normalize = (s: string) => s.replace(/\d+\.\d ms/g, '<ms> ms');
/** what only the incremental step reports: the count of recomputed slots */
const sansCount = (s: string) => s.replace(/, recomputed \d+ of \d+ slots/g, '');

const run = (dir: string, full: boolean) => {
  const entry = existsSync(join(dir, 'main.decl')) ? ['main.decl'] : [];
  const env = { ...process.env };
  if (full) env.DECL_FULL_RECOMPUTE = '1';
  else delete env.DECL_FULL_RECOMPUTE;
  const r = spawnSync(process.execPath, [cli, 'repl', ...entry, '--script', 'session.txt'], {
    encoding: 'utf8',
    cwd: dir,
    env,
  });
  return { code: r.status ?? -1, out: normalize(r.stdout), err: r.stderr };
};

console.log('== repl: the session corpus, in a fresh copy of each case ==');
for (const c of readdirSync(join(root, 'tests/repl')).sort()) {
  const src = join(root, 'tests/repl', c);
  if (!existsSync(join(src, 'session.txt'))) continue;
  const want = readFileSync(join(src, 'transcript.txt'), 'utf8');
  const wantCode = /^error: /m.test(want) ? 1 : 0;
  const dir = mkdtempSync(join(tmpdir(), 'decl-repl-'));
  cpSync(src, dir, { recursive: true });
  const r = run(dir, false);
  check(`${c}: transcript`, r.out === want, firstDiff(want, r.out));
  check(`${c}: exit ${wantCode}`, r.code === wantCode, `got ${r.code} ${r.err.slice(0, 200)}`);
  // the files the session leaves: every file under expected/, byte for byte
  const expected = join(src, 'expected');
  if (existsSync(expected))
    for (const f of readdirSync(expected).sort()) {
      const actual = existsSync(join(dir, f)) ? readFileSync(join(dir, f), 'utf8') : null;
      const text = readFileSync(join(expected, f), 'utf8');
      check(`${c}: ${f} afterwards`, actual === text, JSON.stringify({ actual, text }));
    }
  rmSync(dir, { recursive: true, force: true });
  // the incremental step against a full recomputation
  const dir2 = mkdtempSync(join(tmpdir(), 'decl-repl-'));
  cpSync(src, dir2, { recursive: true });
  const full = run(dir2, true);
  check(
    `${c}: incremental == full`,
    sansCount(r.out) === full.out && r.code === full.code,
    firstDiff(full.out, sansCount(r.out)),
  );
  rmSync(dir2, { recursive: true, force: true });
}
total();
