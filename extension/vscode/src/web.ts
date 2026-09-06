// The web extension (docs/tooling/04_extension.md §13): on vscode.dev and
// github.dev the language server runs in a web worker over the reference
// implementation's core (server/lsp-web.js) with an in-memory host.
// The extension hands the worker the grammar and runtime wasm, keeps its
// files current from the workspace, and gives the same previews as the
// desktop extension; the REPL terminal and the tasks need a process and
// are not here.
import * as vscode from 'vscode';
import { LanguageClient } from 'vscode-languageclient/browser';
import type { LanguageClientOptions } from 'vscode-languageclient/browser';

let client: LanguageClient | undefined;
let output: vscode.LogOutputChannel;
const previews = new Map<string, { uri: vscode.Uri; root: string | null }>();
const previewEmitter = new vscode.EventEmitter<vscode.Uri>();
const cfg = () => vscode.workspace.getConfiguration('decl');
// VS Code hands configuration objects out as read-only Proxies; the worker's
// structured clone rejects those ("#<Object> could not be cloned"), so what
// goes to the server is a plain copy
const plain = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;

const base64 = (bytes: Uint8Array): string => {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000)
    s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
};
const decoder = new TextDecoder();

async function workspaceFiles(): Promise<{ uri: string; text: string }[]> {
  const found = await vscode.workspace.findFiles(
    '**/*.{decl,json,toml,lock}',
    '**/node_modules/**',
    2000,
  );
  const out: { uri: string; text: string }[] = [];
  for (const f of found) {
    try {
      const bytes = await vscode.workspace.fs.readFile(f);
      if (bytes.byteLength > 1_000_000) continue;
      out.push({ uri: f.toString(), text: decoder.decode(bytes) });
    } catch {
      /* unreadable: absent for the server */
    }
  }
  return out;
}

async function execute<T = any>(command: string, ...args: any[]): Promise<T | null> {
  if (!client) return null;
  return client.sendRequest<T>('workspace/executeCommand', { command, arguments: args });
}
const activeDecl = (): vscode.TextDocument | null => {
  const ed = vscode.window.activeTextEditor;
  return ed && ed.document.languageId === 'decl' ? ed.document : null;
};

const PREVIEW_SCHEME = 'decl-evaluate';
class PreviewProvider implements vscode.TextDocumentContentProvider {
  onDidChange = previewEmitter.event;
  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const src = previews.get(uri.toString());
    if (!src) return '';
    const r = await execute('decl.evaluate', src.uri.toString(), src.root ?? undefined);
    if (!r) return '// the language server is not running';
    if (r.document === null)
      return `// not evaluated\n${(r.diagnostics ?? []).map((d: string) => `// ${d}`).join('\n')}`;
    // the root's declared form (docs/tooling/05_render.md §8): its text, or
    // a fan-out's files each under a `# path` line
    if (r.rendered) {
      const rd = r.rendered;
      if (rd.kind === 'error') return `// not rendered\n// ${rd.diagnostic}\n`;
      if (rd.kind === 'files')
        return rd.files
          .map((f: { path: string; text: string }) => `# ${f.path}\n${f.text}`)
          .join('\n');
      return rd.text;
    }
    let text = r.document;
    if (!cfg().get<boolean>('preview.compact')) {
      try {
        text = JSON.stringify(JSON.parse(r.document), null, 2);
      } catch {
        /* keep */
      }
    }
    const diags = (r.diagnostics ?? []).map((d: string) => `// ${d}`).join('\n');
    return diags ? `${diags}\n${text}\n` : `${text}\n`;
  }
}
async function openPreview(root: string | null) {
  const doc = activeDecl();
  if (!doc) return;
  const name =
    root ?? doc.getText().match(/^(?:export\s+)?output\s+([A-Za-z_][A-Za-z0-9_]*)/m)?.[1] ?? null;
  const uri = vscode.Uri.parse(
    `${PREVIEW_SCHEME}:${doc.uri.path.split('/').pop()}${name ? `/${name}` : ''}.json?${encodeURIComponent(doc.uri.toString())}`,
  );
  previews.set(uri.toString(), { uri: doc.uri, root: name });
  const preview = await vscode.workspace.openTextDocument(uri);
  // the language of the preview follows the root's declared form
  const first = name ? await execute('decl.evaluate', doc.uri.toString(), name) : null;
  const kind = first?.rendered?.kind;
  await vscode.languages.setTextDocumentLanguage(
    preview,
    kind === 'yaml' ? 'yaml' : kind === 'text' || kind === 'files' ? 'plaintext' : 'json',
  );
  await vscode.window.showTextDocument(preview, {
    viewColumn: vscode.ViewColumn.Beside,
    preserveFocus: true,
    preview: false,
  });
}
const refreshPreviews = () => {
  for (const key of previews.keys()) previewEmitter.fire(vscode.Uri.parse(key));
};

const SYNTAX_SCHEME = 'decl-syntax';
class SyntaxTreeProvider implements vscode.TextDocumentContentProvider {
  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const r = await execute('decl.showSyntaxTree', decodeURIComponent(uri.query));
    return r?.tree
      ? r.tree.replace(/\) \(/g, ')\n(').replace(/ \(/g, '\n(')
      : '// the language server is not running';
  }
}

export async function activate(context: vscode.ExtensionContext) {
  output = vscode.window.createOutputChannel('Decl Language Server', { log: true });
  const serverUri = vscode.Uri.joinPath(context.extensionUri, 'server', 'lsp-web.js');
  // VS Code's extension host creates the worker on the page and loads the
  // script by URL (importScripts), so the server must be a classic script
  const worker = new Worker(serverUri.toString(true));
  worker.onerror = (e) => output.appendLine(`worker error: ${e.message}`);
  const wasm = {
    grammar: base64(
      await vscode.workspace.fs.readFile(
        vscode.Uri.joinPath(context.extensionUri, 'server', 'tree-sitter-decl.wasm'),
      ),
    ),
    runtime: base64(
      await vscode.workspace.fs.readFile(
        vscode.Uri.joinPath(context.extensionUri, 'server', 'tree-sitter.wasm'),
      ),
    ),
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ language: 'decl' }],
    outputChannel: output,
    // no `synchronize`: the client would post the configuration Proxies; the
    // change handler below sends a plain copy instead
    initializationOptions: { wasm, inputs: plain(cfg().get('inputs') ?? {}) },
    middleware: {
      executeCommand: async (command, args, next) => {
        switch (command) {
          case 'decl.evaluate':
            return openPreview(typeof args?.[1] === 'string' ? args[1] : null);
          case 'decl.showSyntaxTree': {
            const doc = activeDecl();
            if (!doc) return null;
            const tree = await vscode.workspace.openTextDocument(
              vscode.Uri.parse(
                `${SYNTAX_SCHEME}:${doc.uri.path.split('/').pop()}.tree?${encodeURIComponent(doc.uri.toString())}`,
              ),
            );
            await vscode.window.showTextDocument(tree, {
              viewColumn: vscode.ViewColumn.Beside,
              preserveFocus: true,
              preview: false,
            });
            return null;
          }
          case 'decl.reloadWorkspace': {
            await pushFiles();
            const r = await next(command, args);
            refreshPreviews();
            return r;
          }
          default:
            return next(command, args);
        }
      },
    },
  };
  client = new LanguageClient('decl', 'Decl Language Server', worker, clientOptions);
  const pushFiles = async () => {
    if (client) await client.sendNotification('decl/files', { files: await workspaceFiles() });
  };
  await client.start();
  await pushFiles();

  const watcher = vscode.workspace.createFileSystemWatcher('**/*.{decl,json,toml,lock}');
  const changed = async (uri: vscode.Uri) => {
    try {
      const bytes = await vscode.workspace.fs.readFile(uri);
      await client?.sendNotification('decl/files', {
        files: [{ uri: uri.toString(), text: decoder.decode(bytes) }],
      });
    } catch {
      await client?.sendNotification('decl/files', { remove: [uri.toString()] });
    }
    refreshPreviews();
  };
  context.subscriptions.push(
    output,
    watcher,
    watcher.onDidChange(changed),
    watcher.onDidCreate(changed),
    watcher.onDidDelete((uri) => {
      void client?.sendNotification('decl/files', { remove: [uri.toString()] });
      refreshPreviews();
    }),
    vscode.workspace.registerTextDocumentContentProvider(PREVIEW_SCHEME, new PreviewProvider()),
    vscode.workspace.registerTextDocumentContentProvider(SYNTAX_SCHEME, new SyntaxTreeProvider()),
    vscode.commands.registerCommand('decl.openOutputPreview', () => openPreview(null)),
    vscode.commands.registerCommand('decl.showOutput', () => output.show()),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (!e.affectsConfiguration('decl')) return;
      void client?.sendNotification('workspace/didChangeConfiguration', {
        settings: { decl: plain(cfg()) },
      });
      refreshPreviews();
    }),
    vscode.workspace.onDidSaveTextDocument((doc) => {
      if (doc.languageId === 'decl' && cfg().get('preview.refresh') !== 'manual') refreshPreviews();
    }),
  );
}

export async function deactivate() {
  if (client) {
    await client.stop();
    client = undefined;
  }
}
