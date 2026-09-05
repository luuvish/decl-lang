// The language server's core over an in-memory host (what the browser's
// worker runs): the same session corpus (tests/lsp/<case>/) replayed over
// the core without a file system, against the same transcripts as the
// stdio server — each case in a fresh process, since the core's state is
// the process's — then the one surface the memory host alone has: the
// client pushing files with `decl/files`.
import { spawnSync } from 'node:child_process';
import { readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initParser } from '../src/node.ts';
import { memoryHost, setHost } from '../src/host.ts';
import { connect, drained } from '../src/lsp-core.ts';
import { check, total } from './common/check.ts';
import { Session, cases, compare, type Msg, type Transport } from './common/lsp-replay.ts';

const self = fileURLToPath(import.meta.url);

/** the core fed directly: what it sends is queued; `recv` waits for its work to drain */
class MemoryTransport implements Transport {
  log: Msg[] = [];
  private out: Msg[] = [];
  private feed = connect({ send: (m) => this.out.push(m), init: async () => {}, exit: () => {} });
  send(msg: Msg) {
    this.feed(msg);
  }
  async recv(): Promise<Msg> {
    for (;;) {
      if (this.out.length) {
        const m = this.out.shift()!;
        this.log.push(m);
        return m;
      }
      await drained();
      if (!this.out.length) throw new Error('the core sent nothing');
    }
  }
  close() {}
}

const at = process.argv.indexOf('--case');
if (at >= 0) {
  // one case, in this process: the workspace's files served from memory
  const dir = process.argv[at + 1];
  const ws = resolve(dir, 'ws');
  await initParser(); // the grammar from disk; the files from memory
  const files = new Map<string, string>();
  for (const f of readdirSync(ws)) files.set(join(ws, f), readFileSync(join(ws, f), 'utf8'));
  setHost(memoryHost(files, { cwd: ws, uriPrefix: 'file://' }));
  const got = await new Session(dir, new MemoryTransport()).run();
  process.stdout.write(JSON.stringify(got));
  process.exit(0);
}

console.log('== lsp core: the session corpus over a memory host ==');
for (const dir of cases()) {
  const r = spawnSync(process.execPath, [self, '--case', dir], { encoding: 'utf8' });
  if (r.status !== 0) {
    check(`${dir.slice(dir.lastIndexOf('/') + 1)}: the replay ran`, false, r.stderr.slice(-300));
    continue;
  }
  compare(dir, JSON.parse(r.stdout), check);
}

console.log('== lsp core: files pushed by the client (decl/files) ==');
{
  await initParser();
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
  const out: Msg[] = [];
  const feed = connect({ send: (m) => out.push(m), init: async () => {}, exit: () => {} });
  let id = 0;
  const request = async (method: string, params: any) => {
    const my = ++id;
    feed({ jsonrpc: '2.0', id: my, method, params });
    await drained();
    return out.find((m) => m.id === my)?.result;
  };
  const uri = 'vscode-vfs://github/ws/main.decl';
  await request('initialize', { capabilities: {} });
  feed({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: { uri, languageId: 'decl', version: 1, text: files.get('/ws/main.decl') },
    },
  });
  await drained();
  const def = await request('textDocument/definition', {
    textDocument: { uri },
    position: { line: 2, character: 19 },
  });
  check(
    "definition follows the import into the memory host, with the client's URI scheme",
    def && def.uri === 'vscode-vfs://github/ws/lib.decl' && def.range.start.line === 0,
    JSON.stringify(def),
  );
  const ev = await request('workspace/executeCommand', {
    command: 'decl.evaluate',
    arguments: [uri, 's'],
  });
  check('decl.evaluate over the memory host', ev?.document === '{"name":"a","port":8080}');
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
  check('decl/files replaces a file of the host', ev2?.document === '{"name":"a","port":9000}');
}
total();
