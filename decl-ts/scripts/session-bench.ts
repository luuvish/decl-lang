// Same workload as the Python/Rust drivers, via the public Session API.
import { readFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';

const repo = resolve(process.env.DECL_BENCH_REPO ?? '.');
const { initParser } = await import(pathToFileURL(join(repo, 'decl-ts/src/node.ts')).href);
const { Session } = await import(pathToFileURL(join(repo, 'decl-ts/src/session.ts')).href);
await initParser();
const config = JSON.parse(readFileSync(process.argv[2] ?? 'tests/benchmarks/session.json', 'utf8'));
const rows = [];
for (const size of config.sizes)
  for (const equal of [true, false]) {
    const entry = join(repo, 'tests/benchmarks/session_case.decl');
    const session = new Session(entry, new Map([[entry, config.source]]));
    session.apply({
      op: 'bind',
      name: 'batch',
      src: {
        kind: 'inline',
        text: JSON.stringify({ items: Array.from({ length: size }, () => ({ n: 1 })) }),
      },
    });
    const first = session.run();
    if (!first.eng || first.diags.length) throw new Error('invalid benchmark');
    const times: number[] = [];
    for (let i = 0; i < config.warmups + config.samples; i++) {
      const n = equal ? 2 + (i % 2) : i % 2 ? 1 : -1;
      const start = performance.now();
      session.apply({ op: 'edit', kind: 'update', path: 'batch.items[0].n', expr: String(n) });
      const run = session.run();
      const value = run.eng.serialize(run.entry.env.roots.get('total'), 'total');
      const elapsed = performance.now() - start;
      if (run.diags.length || value !== String(7 * (size - (n < 0 ? 1 : 0))))
        throw new Error('incorrect result');
      if (i >= config.warmups) times.push(elapsed);
    }
    const sorted = [...times].sort((a, b) => a - b);
    rows.push({ size, equal, samples_ms: times, median_ms: sorted[Math.floor(sorted.length / 2)] });
  }
console.log(JSON.stringify(rows));
