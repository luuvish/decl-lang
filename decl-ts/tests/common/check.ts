// The corpus drivers' shared shape: the repository root, the command line
// and the server, `check` counting passes and failures, `total` ending the
// run with the count and the exit status.
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

export const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
export const cli = join(root, 'decl-ts/src/cli.ts');
export const lspServer = join(root, 'decl-ts/src/lsp.ts');

let pass = 0,
  fail = 0;
export const check = (name: string, cond: boolean, detail = '') => {
  if (cond) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    fail++;
    console.log(`  FAIL ${name} ${detail}`);
  }
};
export function total(): never {
  console.log(`TOTAL ${pass} ok, ${fail} failed`);
  process.exit(fail ? 1 : 0);
}

/** where two texts first differ, line by line */
export function firstDiff(expected: string, got: string): string {
  const a = expected.split('\n'),
    b = got.split('\n');
  for (let i = 0; i < Math.max(a.length, b.length); i++)
    if (a[i] !== b[i])
      return `line ${i + 1}: expected ${JSON.stringify(a[i])}, got ${JSON.stringify(b[i])}`;
  return '';
}
