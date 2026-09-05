// The web extension's suite (docs/tooling/04_extension.md §13): run by
// @vscode/test-web inside VS Code for the Web in a browser — the server
// is the worker over the in-memory host, fed the workspace's files.
import * as vscode from 'vscode';
// mocha's prebuilt browser build: `mocha` becomes a global. Since mocha 12
// the package is `type: module`, so esbuild inlines this UMD file as ESM,
// where its `module.exports = factory()` would replace this CommonJS bundle's
// own exports (`run` included) with mocha itself — the build defines
// `exports` away so the UMD takes its global branch.
import 'mocha/mocha.js';
declare const mocha: Mocha & { setup(options: Mocha.MochaOptions | Mocha.Interface): Mocha };
declare function suite(name: string, fn: () => void): void;
declare function test(name: string, fn: () => Promise<void> | void): void;

const assert = {
  ok(cond: unknown, message?: string) {
    if (!cond) throw new Error(message ?? 'assertion failed');
  },
  strictEqual(a: unknown, b: unknown, message?: string) {
    if (a !== b) throw new Error(message ?? `${String(a)} !== ${String(b)}`);
  },
};
const until = async (cond: () => boolean, ms = 30000) => {
  const t0 = Date.now();
  while (!cond()) {
    if (Date.now() - t0 > ms) throw new Error('timed out');
    await new Promise((r) => setTimeout(r, 200));
  }
};

// a diagnostic's code is a string, a number, or an object carrying the value
const codeOf = (c: vscode.Diagnostic['code']) =>
  typeof c === 'object' && c ? String(c.value) : String(c);

export function run(): Promise<void> {
  return new Promise((resolve, reject) => {
    // mocha 12's default browser reporter is the HTML one, which needs a document
    // the extension host worker does not have; spec writes to the console
    mocha.setup({ ui: 'tdd', reporter: 'spec', timeout: 60000 });
    suite('vscode-decl (web)', () => {
      test('the worker server publishes diagnostics and answers hover and definition', async () => {
        const ext = vscode.extensions.getExtension('luuvish.vscode-decl');
        assert.ok(ext, 'the extension is installed');
        await ext!.activate();
        const folder = vscode.workspace.workspaceFolders![0].uri;
        const doc = await vscode.workspace.openTextDocument(
          vscode.Uri.joinPath(folder, 'broken.decl'),
        );
        await vscode.window.showTextDocument(doc);
        await until(() => vscode.languages.getDiagnostics(doc.uri).length > 0);
        assert.strictEqual(codeOf(vscode.languages.getDiagnostics(doc.uri)[0].code), 'E4011');
        const main = await vscode.workspace.openTextDocument(
          vscode.Uri.joinPath(folder, 'main.decl'),
        );
        await vscode.window.showTextDocument(main);
        const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
          'vscode.executeHoverProvider',
          main.uri,
          new vscode.Position(1, 7),
        );
        assert.ok(
          hovers.length &&
            String((hovers[0].contents[0] as vscode.MarkdownString).value).includes('const top'),
          'hover through the worker',
        );
        const defs = await vscode.commands.executeCommand<vscode.Location[]>(
          'vscode.executeDefinitionProvider',
          main.uri,
          new vscode.Position(2, 19),
        );
        assert.ok(
          defs.length && defs[0].uri.path.endsWith('lib.decl'),
          'definition follows the import through the worker host',
        );
      });
    });
    mocha.run((failures: number) =>
      failures ? reject(new Error(`${failures} test(s) failed`)) : resolve(),
    );
  });
}
