// LSP server end-to-end over stdio (Phase 4 exit criterion:
// diagnostics displayed in an editor): initialize, open/change with
// publishDiagnostics, hover, and definition — including one import hop.
import { spawn } from 'node:child_process';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
let pass = 0, fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) { pass++; console.log(`  ok   ${name}`); }
  else { fail++; console.log(`  FAIL ${name} ${detail}`); }
};

const server = spawn('node', [join(root, 'decl-ts/src/lsp.ts')], { stdio: ['pipe', 'pipe', 'inherit'] });
let buf = Buffer.alloc(0);
const pendingReplies = new Map<number, (r: any) => void>();
const notifications: any[] = [];
const notifyWaiters: ((m: any) => boolean)[] = [];
server.stdout.on('data', c => {
  buf = Buffer.concat([buf, c]);
  for (; ;) {
    const he = buf.indexOf('\r\n\r\n');
    if (he < 0) return;
    const m = /Content-Length: (\d+)/i.exec(buf.subarray(0, he).toString())!;
    const len = parseInt(m[1], 10);
    if (buf.length < he + 4 + len) return;
    const msg = JSON.parse(buf.subarray(he + 4, he + 4 + len).toString());
    buf = buf.subarray(he + 4 + len);
    if (msg.id !== undefined && pendingReplies.has(msg.id)) {
      pendingReplies.get(msg.id)!(msg.result); pendingReplies.delete(msg.id);
    } else {
      notifications.push(msg);
      for (let i = notifyWaiters.length - 1; i >= 0; i--)
        if (notifyWaiters[i](msg)) notifyWaiters.splice(i, 1);
    }
  }
});
let nextId = 1;
const send = (msg: any) => {
  const body = JSON.stringify(msg);
  server.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
};
const request = (method: string, params: any): Promise<any> => new Promise(res => {
  const id = nextId++;
  pendingReplies.set(id, res);
  send({ jsonrpc: '2.0', id, method, params });
});
const notifyServer = (method: string, params: any) => send({ jsonrpc: '2.0', method, params });
const nextDiagnostics = (uri: string): Promise<any> => new Promise(res => {
  notifyWaiters.push(m => {
    if (m.method === 'textDocument/publishDiagnostics' && m.params.uri === uri) { res(m.params); return true; }
    return false;
  });
});

const dir = mkdtempSync(join(tmpdir(), 'decl-lsp-'));
const libPath = join(dir, 'lib.decl');
writeFileSync(libPath, 'export type Service = { name: string, port?: 1..65535 = 8080 }\nexport const MAX = 16\n');
const mainPath = join(dir, 'main.decl');
const mainUri = pathToFileURL(mainPath).toString();
writeFileSync(mainPath, '');

const init = await request('initialize', { processId: null, rootUri: null, capabilities: {} });
check('initialize advertises capabilities',
  init.capabilities.hoverProvider === true && init.capabilities.definitionProvider === true);
notifyServer('initialized', {});

// syntax error diagnostics
{
  const p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didOpen', { textDocument: { uri: mainUri, languageId: 'decl', version: 1, text: 'const x = \n' } });
  const d = await p;
  check('syntax error published', d.diagnostics.length > 0 && d.diagnostics[0].message === 'syntax error', JSON.stringify(d));
}
// checker diagnostics with a useful anchor
{
  const p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 2 },
    contentChanges: [{ text: 'type Bad = 10..3\n' }],
  });
  const d = await p;
  check('checker diagnostic published with code', d.diagnostics.some((x: any) => x.code === 'E4011'), JSON.stringify(d));
  check('diagnostic anchored to the name', d.diagnostics[0].range.start.line === 0 && d.diagnostics[0].range.start.character > 0, JSON.stringify(d.diagnostics[0].range));
}
// clean file + import; hover and definition
const mainSrc = 'import { Service, MAX as LIMIT } from "./lib.decl"\nconst top = LIMIT\nexport output s: Service = { name: "a" }\nexport output t: Service = {\n    name: "b"\n}\nconst first = s.name\n';
{
  const p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 3 },
    contentChanges: [{ text: mainSrc }],
  });
  const d = await p;
  check('clean module publishes no diagnostics', d.diagnostics.length === 0, JSON.stringify(d));
}
{
  // hover over `top` (line 1, "const top = LIMIT")
  const h = await request('textDocument/hover', { textDocument: { uri: mainUri }, position: { line: 1, character: 7 } });
  check('hover shows the declaration', h && h.contents.value.includes('const top = LIMIT'), JSON.stringify(h));
  // hover over the renamed import LIMIT
  const h2 = await request('textDocument/hover', { textDocument: { uri: mainUri }, position: { line: 1, character: 13 } });
  check('hover follows a renamed import', h2 && h2.contents.value.includes('MAX = 16'), JSON.stringify(h2));
}
{
  // definition of Service in the output annotation (line 2)
  const col = mainSrc.split('\n')[2].indexOf('Service') + 2;
  const def = await request('textDocument/definition', { textDocument: { uri: mainUri }, position: { line: 2, character: col } });
  check('definition jumps across the import', def && def.uri.endsWith('lib.decl') && def.range.start.line === 0, JSON.stringify(def));
}

{
  const td = await request('textDocument/typeDefinition', { textDocument: { uri: mainUri }, position: { line: 6, character: 14 } });
  check('type definition of a value of type Service', td && td.uri.endsWith('lib.decl') && td.range.start.line === 0, JSON.stringify(td));
  const refs = await request('textDocument/references', { textDocument: { uri: mainUri }, position: { line: 2, character: 19 }, context: { includeDeclaration: true } });
  check('references of Service: declaration, import item, two annotations', refs.length === 4 && refs.filter((r: any) => r.uri.endsWith('lib.decl')).length === 1, JSON.stringify(refs));
  const hl = await request('textDocument/documentHighlight', { textDocument: { uri: mainUri }, position: { line: 6, character: 14 } });
  check('highlight of s: its declaration and its use', hl.length === 2 && hl[0].range.start.line === 2 && hl[1].range.start.line === 6, JSON.stringify(hl));
  const c1 = await request('textDocument/completion', { textDocument: { uri: mainUri }, position: { line: 1, character: 15 } });
  check('completion of a name prefix', c1.items.some((i: any) => i.label === 'LIMIT'), JSON.stringify(c1));
  const broken = mainSrc + 'const e = s.\n';
  let p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', { textDocument: { uri: mainUri, version: 20 }, contentChanges: [{ text: broken }] });
  await p;
  const c2 = await request('textDocument/completion', { textDocument: { uri: mainUri }, position: { line: 7, character: 12 } });
  check('member completion while the text does not parse', c2.items.map((i: any) => i.label).join(',') === 'name,port', JSON.stringify(c2));
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', { textDocument: { uri: mainUri, version: 21 }, contentChanges: [{ text: mainSrc }] });
  await p;
  const syms = await request('textDocument/documentSymbol', { textDocument: { uri: mainUri } });
  check('document symbols', syms.map((s: any) => s.name).join(',') === 'top,s,t,first', JSON.stringify(syms));
  const folds = await request('textDocument/foldingRange', { textDocument: { uri: mainUri } });
  check('folding of the multi-line output', folds.length === 1 && folds[0].startLine === 3 && folds[0].endLine === 5, JSON.stringify(folds));
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', { textDocument: { uri: mainUri, version: 22 }, contentChanges: [{ text: 'const x=1\n' }] });
  await p;
  const fmt = await request('textDocument/formatting', { textDocument: { uri: mainUri }, options: { tabSize: 4, insertSpaces: true } });
  check('formatting replaces the document with its canonical form', fmt.length === 1 && fmt[0].newText === 'const x = 1\n', JSON.stringify(fmt));
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', { textDocument: { uri: mainUri, version: 23 }, contentChanges: [{ text: mainSrc }] });
  await p;
  const pr = await request('textDocument/prepareRename', { textDocument: { uri: mainUri }, position: { line: 1, character: 7 } });
  check('prepare rename gives the name range', pr && pr.placeholder === 'top' && pr.range.start.character === 6, JSON.stringify(pr));
  const rn = await request('textDocument/rename', { textDocument: { uri: mainUri }, position: { line: 2, character: 19 }, newName: 'Svc' });
  check('rename edits every module', rn && Object.keys(rn.changes).length === 2 && rn.changes[mainUri].length === 3, JSON.stringify(rn));
  const lenses = await request('textDocument/codeLens', { textDocument: { uri: mainUri } });
  check('lenses on the outputs', lenses.length === 2 && lenses[0].command.command === 'decl.evaluate', JSON.stringify(lenses));
  const ev = await request('workspace/executeCommand', { command: 'decl.evaluate', arguments: [mainUri, 's'] });
  check('decl.evaluate returns the document', ev && ev.document === '{"name":"a","port":8080}' && ev.diagnostics.length === 0, JSON.stringify(ev));
  const va = await request('workspace/executeCommand', { command: 'decl.validate', arguments: [mainUri, 's'] });
  check('decl.validate returns the verdict', va && va.verdicts.length === 1 && va.verdicts[0].errors === 0, JSON.stringify(va));
}

await request('shutdown', {});
notifyServer('exit', {});
server.stdin.end();

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
