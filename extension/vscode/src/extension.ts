// The VS Code extension for Decl (docs/tooling/04_extension.md §2–§13):
// a client for decl-lsp — bundled (server/lsp.js, the reference
// implementation) or any decl-lsp by path — plus the editor-side
// features: the output preview, input bindings, the trace view, the
// REPL terminal, the fixture runner, and the server's lifecycle.
// Nothing is computed here: every answer is the server's.
import * as vscode from 'vscode';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { LanguageClient, TransportKind } from 'vscode-languageclient/node';
import type { LanguageClientOptions, ServerOptions } from 'vscode-languageclient/node';

let client: LanguageClient | undefined;
let output: vscode.LogOutputChannel;
let status: vscode.StatusBarItem;
const previews = new Map<string, { uri: vscode.Uri; root: string | null }>(); // preview uri -> source
const previewEmitter = new vscode.EventEmitter<vscode.Uri>();

const cfg = () => vscode.workspace.getConfiguration('decl');

// ---------------- the server ----------------
// the bundled server is a JavaScript module run by the extension host's
// Node (VS Code forks it as node); a configured server is an executable
function serverOptionsOf(context: vscode.ExtensionContext): {
  options: ServerOptions;
  label: string;
} {
  // DECL_SERVER_PATH (the tests): the same suite against another implementation's decl-lsp
  const configured = (
    process.env.DECL_SERVER_PATH ??
    cfg().get<string>('server.path') ??
    ''
  ).trim();
  const args = cfg().get<string[]>('server.args') ?? [];
  if (configured)
    return {
      options: { command: configured, args, transport: TransportKind.stdio },
      label: configured,
    };
  const module = context.asAbsolutePath(path.join('server', 'lsp.mjs'));
  return { options: { module, args, transport: TransportKind.stdio }, label: `bundled ${module}` };
}

async function startClient(context: vscode.ExtensionContext) {
  const { options: serverOptions, label: command } = serverOptionsOf(context);
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'decl' }],
    outputChannel: output,
    synchronize: {
      configurationSection: 'decl',
      fileEvents: vscode.workspace.createFileSystemWatcher(
        '**/{*.decl,decl.toml,decl.lock,*.json}',
      ),
    },
    initializationOptions: { inputs: cfg().get('inputs') ?? {} },
    // the server's commands are registered by the client; the editor-side
    // behavior of the ones the extension gives a face to lives here, so a
    // lens, a palette entry, or a keybinding all open the preview
    middleware: {
      executeCommand: async (command, args, next) => {
        switch (command) {
          case 'decl.evaluate':
            return openPreview(typeof args?.[1] === 'string' ? args[1] : null);
          case 'decl.validate':
            return validate();
          case 'decl.trace':
            return trace();
          case 'decl.showSyntaxTree':
            return showSyntaxTree();
          case 'decl.reloadWorkspace': {
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
  client = new LanguageClient('decl', 'Decl Language Server', serverOptions, clientOptions);
  status.text = '$(sync~spin) Decl';
  status.tooltip = `starting ${command}`;
  try {
    await client.start();
    status.text = '$(check) Decl';
    status.tooltip = `decl-lsp: ${command}`;
  } catch (e: any) {
    status.text = '$(error) Decl';
    status.tooltip = String(e?.message ?? e);
    if (process.env.DECL_EXTENSION_LOG) {
      try {
        fs.appendFileSync(
          process.env.DECL_EXTENSION_LOG,
          `start failed (${command}): ${e?.stack ?? e}\n`,
        );
      } catch {
        /* the log is best effort */
      }
    }
    vscode.window
      .showErrorMessage(
        `Decl: the language server did not start (${command}): ${e?.message ?? e}`,
        'Show Output',
      )
      .then((pick) => {
        if (pick) output.show();
      });
  }
}
async function stopClient() {
  if (client) {
    await client.stop();
    client = undefined;
  }
}

// ---------------- commands ----------------
async function execute<T = any>(command: string, ...args: any[]): Promise<T | null> {
  if (!client) return null;
  return client.sendRequest<T>('workspace/executeCommand', { command, arguments: args });
}
const activeDecl = (): vscode.TextDocument | null => {
  const ed = vscode.window.activeTextEditor;
  return ed && ed.document.languageId === 'decl' ? ed.document : null;
};
async function pickRoot(
  doc: vscode.TextDocument,
  kind: 'output' | 'input' | 'any',
): Promise<string | undefined> {
  const re =
    kind === 'output'
      ? /^(?:export\s+)?output\s+([A-Za-z_][A-Za-z0-9_]*)/gm
      : kind === 'input'
        ? /^(?:export\s+)?input\s+([A-Za-z_][A-Za-z0-9_]*)/gm
        : /^(?:export\s+)?(?:output|input)\s+([A-Za-z_][A-Za-z0-9_]*)/gm;
  const names = [...doc.getText().matchAll(re)].map((m) => m[1]);
  if (names.length === 0) {
    vscode.window.showInformationMessage(
      `Decl: the module declares no ${kind === 'any' ? 'root' : kind}`,
    );
    return undefined;
  }
  if (names.length === 1) return names[0];
  return vscode.window.showQuickPick(names, { placeHolder: `${kind}` });
}

// the output preview: a read-only JSON document, refreshed by the server
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
    const compact = cfg().get<boolean>('preview.compact');
    const text = compact ? r.document : pretty(r.document);
    const diags = (r.diagnostics ?? []).map((d: string) => `// ${d}`).join('\n');
    return diags ? `${diags}\n${text}\n` : `${text}\n`;
  }
}
function pretty(compact: string): string {
  try {
    return JSON.stringify(JSON.parse(compact), null, 2);
  } catch {
    return compact;
  }
}
async function openPreview(root: string | null) {
  const doc = activeDecl();
  if (!doc) return;
  const name = root ?? (await pickRoot(doc, 'output'));
  const uri = vscode.Uri.parse(
    `${PREVIEW_SCHEME}:${path.basename(doc.fileName)}${name ? `/${name}` : ''}.json?${encodeURIComponent(doc.uri.toString())}`,
  );
  previews.set(uri.toString(), { uri: doc.uri, root: name ?? null });
  const preview = await vscode.workspace.openTextDocument(uri);
  await vscode.languages.setTextDocumentLanguage(preview, 'json');
  await vscode.window.showTextDocument(preview, {
    viewColumn: vscode.ViewColumn.Beside,
    preserveFocus: true,
    preview: false,
  });
}
function refreshPreviews(source?: vscode.Uri) {
  for (const [key, src] of previews)
    if (!source || src.uri.toString() === source.toString())
      previewEmitter.fire(vscode.Uri.parse(key));
}

async function bindInput() {
  const doc = activeDecl();
  if (!doc) return;
  const name = await pickRoot(doc, 'input');
  if (!name) return;
  const picked = await vscode.window.showOpenDialog({
    canSelectMany: false,
    filters: { JSON: ['json'] },
    title: `bind ${name}`,
  });
  if (!picked?.length) return;
  const folder = vscode.workspace.getWorkspaceFolder(doc.uri);
  const rel = folder ? path.relative(folder.uri.fsPath, picked[0].fsPath) : picked[0].fsPath;
  const inputs = { ...(cfg().get<Record<string, string>>('inputs') ?? {}), [name]: rel };
  await cfg().update('inputs', inputs, vscode.ConfigurationTarget.Workspace);
  refreshPreviews();
}
async function unbindInput() {
  const inputs = { ...(cfg().get<Record<string, string>>('inputs') ?? {}) };
  const name = await vscode.window.showQuickPick(Object.keys(inputs), { placeHolder: 'unbind' });
  if (!name) return;
  delete inputs[name];
  await cfg().update('inputs', inputs, vscode.ConfigurationTarget.Workspace);
  refreshPreviews();
}

async function validate() {
  const doc = activeDecl();
  if (!doc) return;
  const name = await pickRoot(doc, 'any');
  const r = await execute('decl.validate', doc.uri.toString(), name);
  if (!r) return;
  const lines = [
    ...(r.verdicts ?? []).map((v: any) =>
      v.errors === 0 && v.warnings === 0
        ? `${v.name}: ok`
        : `${v.name}: ${v.errors} error(s), ${v.warnings} warning(s)`,
    ),
  ];
  output.appendLine(`validate ${path.basename(doc.fileName)}${name ? ` ${name}` : ''}`);
  for (const d of r.diagnostics ?? []) output.appendLine(`  ${d}`);
  for (const l of lines) output.appendLine(`  ${l}`);
  const bad = (r.verdicts ?? []).some((v: any) => v.errors > 0);
  (bad ? vscode.window.showWarningMessage : vscode.window.showInformationMessage)(
    `Decl: ${lines.join('; ') || 'no roots'}`,
    'Show Output',
  ).then((pick) => {
    if (pick) output.show();
  });
}

async function trace() {
  const doc = activeDecl();
  const ed = vscode.window.activeTextEditor;
  if (!doc || !ed) return;
  const word = doc.getText(
    doc.getWordRangeAtPosition(ed.selection.active, /[A-Za-z_$][A-Za-z0-9_$.[\]"]*/),
  );
  const p = await vscode.window.showInputBox({
    prompt: 'the canonical path to trace',
    value: word,
  });
  if (!p) return;
  const r = await execute('decl.trace', doc.uri.toString(), p);
  if (!r) return;
  output.appendLine(`trace ${p}`);
  for (const l of r.lines ?? []) output.appendLine(`  ${l}`);
  traceProvider.set(r.lines ?? []);
  await vscode.commands.executeCommand('setContext', 'decl.traceVisible', true);
  await vscode.commands.executeCommand('decl.trace.focus');
}

// the syntax tree: a read-only document following the editor
const SYNTAX_SCHEME = 'decl-syntax';
const syntaxEmitter = new vscode.EventEmitter<vscode.Uri>();
class SyntaxTreeProvider implements vscode.TextDocumentContentProvider {
  onDidChange = syntaxEmitter.event;
  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const source = decodeURIComponent(uri.query);
    const r = await execute('decl.showSyntaxTree', source);
    return r?.tree
      ? r.tree.replace(/\) \(/g, ')\n(').replace(/ \(/g, '\n(')
      : '// the language server is not running';
  }
}
async function showSyntaxTree() {
  const doc = activeDecl();
  if (!doc) return;
  const uri = vscode.Uri.parse(
    `${SYNTAX_SCHEME}:${path.basename(doc.fileName)}.tree?${encodeURIComponent(doc.uri.toString())}`,
  );
  const tree = await vscode.workspace.openTextDocument(uri);
  await vscode.window.showTextDocument(tree, {
    viewColumn: vscode.ViewColumn.Beside,
    preserveFocus: true,
    preview: false,
  });
}

// the trace view: the derivation of a place, or its root cause, as a tree
type TraceNode = { label: string; depth: number; children: TraceNode[] };
class TraceProvider implements vscode.TreeDataProvider<TraceNode> {
  private roots: TraceNode[] = [];
  private emitter = new vscode.EventEmitter<TraceNode | undefined>();
  onDidChangeTreeData = this.emitter.event;
  set(lines: string[]) {
    // the REPL's indentation (two spaces per level) is the tree
    const stack: TraceNode[] = [];
    this.roots = [];
    for (const l of lines) {
      const depth = (l.match(/^ */)?.[0].length ?? 0) / 2;
      const node: TraceNode = { label: l.trim(), depth, children: [] };
      while (stack.length && stack[stack.length - 1].depth >= depth) stack.pop();
      (stack.length ? stack[stack.length - 1].children : this.roots).push(node);
      stack.push(node);
    }
    this.emitter.fire(undefined);
  }
  getTreeItem(n: TraceNode): vscode.TreeItem {
    const item = new vscode.TreeItem(
      n.label,
      n.children.length
        ? vscode.TreeItemCollapsibleState.Expanded
        : vscode.TreeItemCollapsibleState.None,
    );
    if (/\(invalid\)|^error/.test(n.label)) item.iconPath = new vscode.ThemeIcon('error');
    return item;
  }
  getChildren(n?: TraceNode): TraceNode[] {
    return n ? n.children : this.roots;
  }
}
const traceProvider = new TraceProvider();

// fixtures in the Test Explorer: a corpus is a directory with valid/ and invalid/ children
function fixtureController(context: vscode.ExtensionContext): vscode.TestController {
  const ctrl = vscode.tests.createTestController('decl.fixtures', 'Decl fixtures');
  const corpora = new Map<string, vscode.TestItem>();
  const discover = async () => {
    ctrl.items.replace([]);
    corpora.clear();
    // a corpus is the directory holding valid/ and invalid/ (a feature of
    // tests/validation, or a project's own fixtures); `decl.fixtures.directories`
    // narrows to the globs given, relative to the workspace folder
    const globs = (cfg().get<string[]>('fixtures.directories') ?? []).map(
      (g) =>
        new RegExp(
          '^' +
            g
              .replace(/[.+^${}()|[\]\\]/g, '\\$&')
              .replace(/\*\*\//g, '(?:.*/)?')
              .replace(/\*\*/g, '.*')
              .replace(/\*/g, '[^/]*') +
            '$',
        ),
    );
    const files = await vscode.workspace.findFiles(
      '**/{valid,invalid}/*.decl',
      '**/node_modules/**',
    );
    for (const f of files) {
      const root = path.dirname(path.dirname(f.fsPath));
      const folder = vscode.workspace.getWorkspaceFolder(f);
      const rel = folder ? path.relative(folder.uri.fsPath, root).split(path.sep).join('/') : root;
      if (globs.length && !globs.some((g) => g.test(rel))) continue;
      let corpus = corpora.get(root);
      if (!corpus) {
        corpus = ctrl.createTestItem(root, rel || path.basename(root), vscode.Uri.file(root));
        corpora.set(root, corpus);
        ctrl.items.add(corpus);
      }
      corpus.children.add(ctrl.createTestItem(f.fsPath, path.relative(root, f.fsPath), f));
    }
  };
  ctrl.resolveHandler = async () => {
    await discover();
  };
  ctrl.createRunProfile(
    'judge',
    vscode.TestRunProfileKind.Run,
    async (request, token) => {
      const run = ctrl.createTestRun(request);
      const targets = request.include ?? [...corpora.values()];
      for (const target of targets) {
        const corpus =
          corpora.get(target.id) ?? [...corpora.values()].find((c) => target.id.startsWith(c.id));
        if (!corpus) continue;
        const cli = declCli(context);
        const { spawnSync } = await import('node:child_process');
        const r = spawnSync(cli[0], [...cli.slice(1), 'validate', corpus.id], { encoding: 'utf8' });
        const failed = new Map<string, string>();
        for (const line of (r.stderr ?? '').split('\n')) {
          const m = /^FAIL (\S+) (.*)$/.exec(line);
          if (m) failed.set(path.resolve(corpus.id, m[1]), m[2]);
        }
        const items = target === corpus ? [...corpus.children].map(([, i]) => i) : [target];
        for (const item of items) {
          if (token.isCancellationRequested) break;
          run.started(item);
          const why = failed.get(item.id);
          if (why) run.failed(item, new vscode.TestMessage(why));
          else run.passed(item);
        }
      }
      run.end();
    },
    true,
  );
  if (cfg().get<boolean>('fixtures.runOnSave'))
    context.subscriptions.push(
      vscode.workspace.onDidSaveTextDocument((doc) => {
        if (!/[\\/](valid|invalid)[\\/][^\\/]+\.decl$/.test(doc.fileName)) return;
        for (const c of corpora.values()) {
          const item = c.children.get(doc.fileName);
          if (item) vscode.commands.executeCommand('testing.runTests', { include: [item] });
        }
      }),
    );
  return ctrl;
}

// the REPL in the terminal: `decl repl` on the file, with the bindings
function declCli(context: vscode.ExtensionContext): string[] {
  const configured = (cfg().get<string>('server.path') ?? '').trim();
  if (configured) {
    const beside = path.join(
      path.dirname(configured),
      process.platform === 'win32' ? 'decl.exe' : 'decl',
    );
    if (fs.existsSync(beside)) return [beside];
    return ['decl'];
  }
  return [process.execPath, context.asAbsolutePath(path.join('server', 'cli.mjs'))];
}
let repl: vscode.Terminal | undefined;
function openRepl(context: vscode.ExtensionContext) {
  const doc = activeDecl();
  const cli = declCli(context);
  const folder = doc
    ? vscode.workspace.getWorkspaceFolder(doc.uri)
    : vscode.workspace.workspaceFolders?.[0];
  const args = [...cli.slice(1), 'repl'];
  if (doc) args.push(doc.fileName);
  for (const [name, file] of Object.entries(cfg().get<Record<string, string>>('inputs') ?? {}))
    args.push('--input', `${name}=${file}`);
  repl = vscode.window.createTerminal({
    name: 'Decl REPL',
    shellPath: cli[0],
    shellArgs: args,
    cwd: folder?.uri.fsPath,
  });
  repl.show();
}
function sendToRepl(context: vscode.ExtensionContext) {
  const ed = vscode.window.activeTextEditor;
  if (!ed) return;
  if (!repl || repl.exitStatus) openRepl(context);
  const sel = ed.selection.isEmpty
    ? ed.document.lineAt(ed.selection.active.line).text
    : ed.document.getText(ed.selection);
  repl!.sendText(sel, true);
  repl!.show(true);
}

// fixtures: the corpus a file belongs to, judged as `decl validate <dir>`
async function runFixtures(context: vscode.ExtensionContext) {
  const doc = activeDecl();
  let dir = doc ? path.dirname(doc.fileName) : vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!dir) return;
  if (/[\\/](valid|invalid)$/.test(dir)) dir = path.dirname(path.dirname(dir));
  const cli = declCli(context);
  const task = new vscode.Task(
    { type: 'decl', command: 'validate' },
    vscode.TaskScope.Workspace,
    `validate ${path.basename(dir)}`,
    'decl',
    new vscode.ProcessExecution(cli[0], [...cli.slice(1), 'validate', dir]),
    '$decl',
  );
  await vscode.tasks.executeTask(task);
}

// tasks: `decl check|evaluate|validate|fmt`
class DeclTaskProvider implements vscode.TaskProvider {
  private readonly context: vscode.ExtensionContext;
  constructor(context: vscode.ExtensionContext) {
    this.context = context;
  }
  provideTasks(): vscode.Task[] {
    const cli = declCli(this.context);
    const file = activeDecl()?.fileName ?? '${file}';
    const mk = (command: string, args: string[]) =>
      new vscode.Task(
        { type: 'decl', command, args },
        vscode.TaskScope.Workspace,
        `${command} ${args.join(' ')}`.trim(),
        'decl',
        new vscode.ProcessExecution(cli[0], [...cli.slice(1), command, ...args]),
        '$decl',
      );
    return [mk('check', [file]), mk('validate', [file]), mk('fmt', ['--check', file])];
  }
  resolveTask(task: vscode.Task): vscode.Task {
    const def: any = task.definition;
    const cli = declCli(this.context);
    return new vscode.Task(
      def,
      task.scope ?? vscode.TaskScope.Workspace,
      task.name,
      'decl',
      new vscode.ProcessExecution(cli[0], [...cli.slice(1), def.command, ...(def.args ?? [])]),
      '$decl',
    );
  }
}

async function selectServer(context: vscode.ExtensionContext) {
  const items: vscode.QuickPickItem[] = [
    { label: 'bundled', description: 'the reference implementation shipped with the extension' },
    { label: 'decl-lsp on PATH', description: 'whichever implementation is installed' },
    { label: 'custom path…' },
  ];
  const pick = await vscode.window.showQuickPick(items, { placeHolder: 'which decl-lsp' });
  if (!pick) return;
  let value = '';
  if (pick.label === 'decl-lsp on PATH') value = 'decl-lsp';
  else if (pick.label === 'custom path…') {
    value = (await vscode.window.showInputBox({ prompt: 'path to decl-lsp' })) ?? '';
    if (!value) return;
  }
  await cfg().update('server.path', value, vscode.ConfigurationTarget.Global);
  await stopClient();
  await startClient(context);
}

// ---------------- activation ----------------
export async function activate(context: vscode.ExtensionContext) {
  output = vscode.window.createOutputChannel('Decl Language Server', { log: true });
  status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 50);
  status.command = 'decl.showOutput';
  status.show();
  context.subscriptions.push(
    output,
    status,
    vscode.workspace.registerTextDocumentContentProvider(PREVIEW_SCHEME, new PreviewProvider()),
    vscode.workspace.registerTextDocumentContentProvider(SYNTAX_SCHEME, new SyntaxTreeProvider()),
    vscode.window.registerTreeDataProvider('decl.trace', traceProvider),
    fixtureController(context),
    vscode.tasks.registerTaskProvider('decl', new DeclTaskProvider(context)),
    vscode.commands.registerCommand('decl.openOutputPreview', () => openPreview(null)),
    vscode.commands.registerCommand('decl.bindInput', bindInput),
    vscode.commands.registerCommand('decl.unbindInput', unbindInput),
    vscode.commands.registerCommand('decl.runFixtures', () => runFixtures(context)),
    vscode.commands.registerCommand('decl.openRepl', () => openRepl(context)),
    vscode.commands.registerCommand('decl.sendToRepl', () => sendToRepl(context)),
    vscode.commands.registerCommand('decl.restartServer', async () => {
      await stopClient();
      await startClient(context);
    }),
    vscode.commands.registerCommand('decl.selectServer', () => selectServer(context)),
    vscode.commands.registerCommand('decl.showOutput', () => output.show()),
    vscode.workspace.onDidSaveTextDocument((doc) => {
      if (doc.languageId === 'decl' && cfg().get('preview.refresh') !== 'manual') refreshPreviews();
    }),
    vscode.window.onDidChangeActiveTextEditor((ed) => {
      if (ed?.document.languageId === 'decl')
        for (const d of vscode.workspace.textDocuments)
          if (
            d.uri.scheme === SYNTAX_SCHEME &&
            decodeURIComponent(d.uri.query) === ed.document.uri.toString()
          )
            syntaxEmitter.fire(d.uri);
    }),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (e.document.languageId === 'decl')
        for (const d of vscode.workspace.textDocuments)
          if (
            d.uri.scheme === SYNTAX_SCHEME &&
            decodeURIComponent(d.uri.query) === e.document.uri.toString()
          )
            syntaxEmitter.fire(d.uri);
    }),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (e.document.languageId === 'decl' && cfg().get('preview.refresh') === 'type') {
        clearTimeout(typeTimer);
        typeTimer = setTimeout(
          () => refreshPreviews(e.document.uri),
          cfg().get<number>('evaluate.idleDelay') ?? 300,
        );
      }
    }),
    vscode.workspace.onDidChangeConfiguration(async (e) => {
      if (e.affectsConfiguration('decl.server')) {
        await stopClient();
        await startClient(context);
      } else if (e.affectsConfiguration('decl.inputs')) refreshPreviews();
    }),
    vscode.window.onDidCloseTerminal((t) => {
      if (t === repl) repl = undefined;
    }),
  );
  await startClient(context);
}
let typeTimer: ReturnType<typeof setTimeout> | undefined;

export async function deactivate() {
  await stopClient();
}
