// The suite entry VS Code calls: every test file under this directory
import * as path from 'node:path';
import * as fs from 'node:fs';
import Mocha from 'mocha';

export function run(): Promise<void> {
  const mocha = new Mocha({ ui: 'tdd', color: true, timeout: 60000 });
  for (const f of fs.readdirSync(__dirname)) if (f.endsWith('.test.js')) mocha.addFile(path.join(__dirname, f));
  // the runner shows only the count: every failure is also written beside the bundle
  const report = path.join(__dirname, '..', 'results.txt');
  fs.writeFileSync(report, '');
  return new Promise((resolve, reject) => {
    const runner = mocha.run(failures => failures ? reject(new Error(`${failures} test(s) failed`)) : resolve());
    runner.on('pass', t => fs.appendFileSync(report, `ok   ${t.fullTitle()}\n`));
    runner.on('fail', (t, err) => fs.appendFileSync(report, `FAIL ${t.fullTitle()}: ${err?.stack ?? err}\n`));
  });
}
