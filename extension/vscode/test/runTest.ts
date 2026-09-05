// Extension tests (docs/tooling/04_extension.md §20): a VS Code is
// downloaded once, the extension is loaded from this directory, and the
// suite under test/suite runs inside it over test/fixtures. Set
// DECL_SERVER_PATH to run the same suite against another decl-lsp.
import * as path from 'node:path';
import { runTests } from '@vscode/test-electron';

async function main() {
  const extensionDevelopmentPath = path.resolve(__dirname, '../..');
  const extensionTestsPath = path.resolve(__dirname, 'suite');
  const workspace = path.resolve(__dirname, '../../test/fixtures');
  const log = path.resolve(__dirname, 'extension-log.txt');
  try { require('node:fs').unlinkSync(log); } catch { /* none yet */ }
  const env: Record<string, string> = { DECL_EXTENSION_LOG: log };
  if (process.env.DECL_SERVER_PATH) env.DECL_SERVER_PATH = path.resolve(process.env.DECL_SERVER_PATH);   // the suite against another decl-lsp
  await runTests({ extensionDevelopmentPath, extensionTestsPath, launchArgs: [workspace, '--disable-extensions'], extensionTestsEnv: env });
}
main().catch(e => { console.error(e); process.exit(1); });
