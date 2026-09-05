// The web suite in VS Code for the Web (a headless Chromium via @vscode/test-web)
import * as path from 'node:path';
import { runTests } from '@vscode/test-web';
async function main() {
  const extensionDevelopmentPath = path.resolve(__dirname, '../..');
  const extensionTestsPath = path.resolve(__dirname, 'web/index.js');
  const folderPath = path.resolve(__dirname, '../../test/fixtures');
  await runTests({
    browserType: 'chromium',
    headless: true,
    extensionDevelopmentPath,
    extensionTestsPath,
    folderPath,
  });
}
main().catch((e) => {
  console.error(e);
  process.exit(1);
});
