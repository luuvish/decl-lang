// The language server's core over an in-memory host (what the browser
// runs): the same answers as the stdio server, without a file system.
import { initParser } from '../src/node.ts';
import { memoryHost, setHost } from '../src/host.ts';
import { connect, drained } from '../src/lsp-core.ts';

let pass = 0,
  fail = 0;
const check = (name: string, cond: boolean, detail = '') => {
  if (cond) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    fail++;
    console.log(`  FAIL ${name} ${detail}`);
  }
};
await initParser(); // the grammar from disk; the files from memory
const files = new Map<string, string>([
  [
    '/ws/lib.decl',
    'export type Service = { name: string, port: 1..65535 = 8080 }\nexport const MAX = 16\n',
  ],
  [
    '/ws/main.decl',
    'import { Service, MAX as LIMIT } from "./lib.decl"\nconst top = LIMIT\nexport output s: Service = { name: "a" }\n',
  ],
]);
setHost(memoryHost(files, { cwd: '/ws', uriPrefix: 'vscode-vfs://github' }));

const out: any[] = [];
const feed = connect({ send: (m) => out.push(m), init: async () => {}, exit: () => {} });
let id = 0;
const request = async (method: string, params: any) => {
  const my = ++id;
  feed({ jsonrpc: '2.0', id: my, method, params });
  await drained();
  return out.find((m) => m.id === my)?.result;
};
const uri = 'vscode-vfs://github/ws/main.decl';
console.log('== lsp core over a memory host ==');
await request('initialize', { capabilities: {} });
feed({
  jsonrpc: '2.0',
  method: 'textDocument/didOpen',
  params: {
    textDocument: { uri, languageId: 'decl', version: 1, text: files.get('/ws/main.decl') },
  },
});
await drained();
const diags = out.find((m) => m.method === 'textDocument/publishDiagnostics');
check(
  'a clean module publishes no diagnostics',
  diags && diags.params.diagnostics.length === 0,
  JSON.stringify(diags),
);
const def = await request('textDocument/definition', {
  textDocument: { uri },
  position: { line: 2, character: 19 },
});
check(
  "definition follows the import into the memory host, with the client's URI scheme",
  def && def.uri === 'vscode-vfs://github/ws/lib.decl' && def.range.start.line === 0,
  JSON.stringify(def),
);
const hover = await request('textDocument/hover', {
  textDocument: { uri },
  position: { line: 1, character: 13 },
});
check(
  'hover through the import',
  hover && hover.contents.value.includes('MAX = 16'),
  JSON.stringify(hover),
);
const ev = await request('workspace/executeCommand', {
  command: 'decl.evaluate',
  arguments: [uri, 's'],
});
check(
  'decl.evaluate over the memory host',
  ev && ev.document === '{"name":"a","port":8080}',
  JSON.stringify(ev),
);
feed({
  jsonrpc: '2.0',
  method: 'decl/files',
  params: {
    files: [
      {
        uri: 'vscode-vfs://github/ws/lib.decl',
        text: 'export type Service = { name: string, port: 1..65535 = 9000 }\nexport const MAX = 16\n',
      },
    ],
  },
});
await drained();
const ev2 = await request('workspace/executeCommand', {
  command: 'decl.evaluate',
  arguments: [uri, 's'],
});
check(
  'decl/files replaces a file of the host',
  ev2 && ev2.document === '{"name":"a","port":9000}',
  JSON.stringify(ev2),
);
console.log(`TOTAL ${pass} ok, ${fail} failed`);
process.exit(fail ? 1 : 0);
