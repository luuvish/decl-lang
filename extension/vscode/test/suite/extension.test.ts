// What the extension promises (docs/tooling/04_extension.md §3–§9): the
// language is registered, the server answers through the client, the
// commands exist, and a preview shows the same bytes `decl evaluate`
// prints. The workspace is test/fixtures.
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as vscode from 'vscode';

const fixture = (name: string) => vscode.Uri.file(path.join(vscode.workspace.workspaceFolders![0].uri.fsPath, name));
const until = async (cond: () => boolean, ms = 20000) => { const t0 = Date.now(); while (!cond()) { if (Date.now() - t0 > ms) throw new Error('timed out'); await new Promise(r => setTimeout(r, 100)); } };

suite('vscode-decl', () => {
  suiteSetup(async () => {
    const ext = vscode.extensions.getExtension('luuvish.vscode-decl');
    assert.ok(ext, 'the extension is installed');
    await ext!.activate();
  });

  test('the language and the commands are contributed', async () => {
    const langs = await vscode.languages.getLanguages();
    assert.ok(langs.includes('decl'));
    const cmds = await vscode.commands.getCommands(true);
    for (const c of ['decl.openOutputPreview', 'decl.evaluate', 'decl.validate', 'decl.bindInput', 'decl.trace', 'decl.showSyntaxTree', 'decl.openRepl', 'decl.restartServer'])
      assert.ok(cmds.includes(c), c);
  });

  test('the server publishes diagnostics with positions', async () => {
    const doc = await vscode.workspace.openTextDocument(fixture('broken.decl'));
    await vscode.window.showTextDocument(doc);
    await until(() => vscode.languages.getDiagnostics(doc.uri).length > 0);
    const [d] = vscode.languages.getDiagnostics(doc.uri);
    assert.strictEqual(d.source, 'decl');
    assert.strictEqual(String(d.code), 'E4011');
    assert.strictEqual(d.range.start.line, 0);
  });

  test('hover, definition, completion, and formatting come from the server', async () => {
    const doc = await vscode.workspace.openTextDocument(fixture('main.decl'));
    await vscode.window.showTextDocument(doc);
    await until(() => vscode.languages.getDiagnostics(doc.uri).length === 0 && true);
    const hovers = await vscode.commands.executeCommand<vscode.Hover[]>('vscode.executeHoverProvider', doc.uri, new vscode.Position(1, 7));
    assert.ok(hovers.length && String((hovers[0].contents[0] as vscode.MarkdownString).value).includes('const top'));
    const defs = await vscode.commands.executeCommand<vscode.Location[]>('vscode.executeDefinitionProvider', doc.uri, new vscode.Position(2, 19));
    assert.ok(defs.length && defs[0].uri.fsPath.endsWith('lib.decl'));
    const typeDefs = await vscode.commands.executeCommand<vscode.Location[]>('vscode.executeTypeDefinitionProvider', doc.uri, new vscode.Position(2, 14));
    assert.ok(typeDefs.length && typeDefs[0].uri.fsPath.endsWith('lib.decl') && typeDefs[0].range.start.line === 0, 'go to type definition of the output s reaches Service');
    const completions = await vscode.commands.executeCommand<vscode.CompletionList>('vscode.executeCompletionItemProvider', doc.uri, new vscode.Position(1, 15));
    assert.ok(completions.items.some(i => i.label === 'LIMIT'));
    const messy = await vscode.workspace.openTextDocument(fixture('messy.decl'));
    const edits = await vscode.commands.executeCommand<vscode.TextEdit[]>('vscode.executeFormatDocumentProvider', messy.uri, { tabSize: 4, insertSpaces: true });
    // the editor may split the server's one edit into minimal edits: apply them and compare the text
    const we = new vscode.WorkspaceEdit();
    we.set(messy.uri, edits);
    await vscode.workspace.applyEdit(we);
    assert.strictEqual(messy.getText(), 'const x = 1\n');
  });

  test('the output preview shows the evaluated document', async () => {
    const doc = await vscode.workspace.openTextDocument(fixture('main.decl'));
    await vscode.window.showTextDocument(doc);
    await vscode.commands.executeCommand('decl.evaluate', doc.uri.toString(), 's');
    await until(() => vscode.workspace.textDocuments.some(d => d.uri.scheme === 'decl-evaluate'));
    const preview = vscode.workspace.textDocuments.find(d => d.uri.scheme === 'decl-evaluate')!;
    await until(() => preview.getText().includes('"name": "a"'));
    assert.ok(preview.getText().includes('"port": 8080'));
  });
});
