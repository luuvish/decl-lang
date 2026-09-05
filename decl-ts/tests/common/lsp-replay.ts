// The language-server corpus replayed over a transport (tests/lsp/README.md
// fixes the session format): the same driver as tests/lsp/replay.py, in
// TypeScript. The stdio server (test/lsp.ts) and the in-memory core
// (test/lsp-core.ts) are two transports of one replay.
import { spawn, type ChildProcessByStdio } from 'node:child_process';
import type { Readable, Writable } from 'node:stream';
import { readFileSync, readdirSync, existsSync, realpathSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { isDeepStrictEqual } from 'node:util';
import { root } from './check.ts';

export type Msg = Record<string, any>;

/** what a session needs of a server: messages in, messages out, in order */
export interface Transport {
  send(msg: Msg): void;
  recv(): Promise<Msg>;
  log: Msg[];
  close(): void;
}

/** one server over stdio; every message it sends is logged */
export class StdioTransport implements Transport {
  p: ChildProcessByStdio<Writable, Readable, null>;
  log: Msg[] = [];
  private buf = Buffer.alloc(0);
  private queue: Msg[] = [];
  private waiters: ((m: Msg) => void)[] = [];
  constructor(cmd: string[]) {
    this.p = spawn(cmd[0], cmd.slice(1), { stdio: ['pipe', 'pipe', 'ignore'], cwd: root });
    this.p.stdout.on('data', (c: Buffer) => {
      this.buf = Buffer.concat([this.buf, c]);
      for (;;) {
        const he = this.buf.indexOf('\r\n\r\n');
        if (he < 0) return;
        const m = /Content-Length: (\d+)/i.exec(this.buf.subarray(0, he).toString())!;
        const len = parseInt(m[1], 10);
        if (this.buf.length < he + 4 + len) return;
        const msg = JSON.parse(this.buf.subarray(he + 4, he + 4 + len).toString());
        this.buf = this.buf.subarray(he + 4 + len);
        this.log.push(msg);
        const w = this.waiters.shift();
        if (w) w(msg);
        else this.queue.push(msg);
      }
    });
  }
  send(msg: Msg) {
    const body = JSON.stringify(msg);
    this.p.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
  }
  recv(): Promise<Msg> {
    const q = this.queue.shift();
    if (q) return Promise.resolve(q);
    return new Promise((res) => this.waiters.push(res));
  }
  close() {
    this.p.stdin.end();
  }
}

const find = (text: string, needle: string, nth: number, offset: number) => {
  let i = -1;
  for (let k = 0; k <= nth; k++) {
    i = text.indexOf(needle, i + 1);
    if (i < 0) throw new Error(`needle not found: ${needle}`);
  }
  const line = text.slice(0, i).split('\n').length - 1;
  const col = i - (text.lastIndexOf('\n', i - 1) + 1) + offset;
  return { line, character: col };
};

export class Session {
  ws: string;
  texts = new Map<string, string>();
  versions = new Map<string, number>();
  diags = new Map<string, any[]>();
  answers = new Map<string, any>();
  openFiles: string[] = [];
  transcript: [string, any][] = [];
  private nextId = 0;
  caseDir: string;
  server: Transport;
  constructor(caseDir: string, server: Transport) {
    this.caseDir = caseDir;
    this.server = server;
    this.ws = realpathSync(resolve(caseDir, 'ws'));
  }
  uri(file: string) {
    return pathToFileURL(join(this.ws, file)).toString();
  }
  fileOf(uri: string): string | null {
    for (const f of this.texts.keys()) if (this.uri(f) === uri) return f;
    return null;
  }
  /** the answer (a result, or {error}) and the methods of what arrived before it */
  async request(method: string, params: any): Promise<[any, string[]]> {
    const my = ++this.nextId;
    this.server.send({ jsonrpc: '2.0', id: my, method, params });
    const between: string[] = [];
    for (;;) {
      const m = await this.server.recv();
      if (!('method' in m) && m.id === my)
        return ['result' in m ? m.result : { error: m.error }, between];
      between.push(m.method ?? 'response');
    }
  }
  notify(method: string, params: any) {
    this.server.send({ jsonrpc: '2.0', method, params });
  }
  /** the next publishDiagnostics for the document, and every message seen until it */
  async diagnostics(uri: string): Promise<[any[], Msg[]]> {
    const seen: Msg[] = [];
    for (;;) {
      const m = await this.server.recv();
      seen.push(m);
      if (m.method === 'textDocument/publishDiagnostics' && m.params.uri === uri)
        return [m.params.diagnostics, seen];
    }
  }
  pendingRequest(method: string): any {
    const log = this.server.log;
    for (let i = log.length - 1; i >= 0; i--)
      if (log[i].method === method && 'id' in log[i]) return log[i].id;
    return null;
  }
  // placeholders (tests/lsp/README.md)
  resolve(v: any, doc: string | null): any {
    if (Array.isArray(v)) return v.map((x) => this.resolve(x, doc));
    if (v && typeof v === 'object') {
      const key = Object.keys(v).find((k) => k.startsWith('$'));
      if (key === '$uri') return this.uri(v.$uri);
      if (key === '$pos' || key === '$at' || key === '$span') {
        if (doc === null) throw new Error('a position placeholder needs a textDocument');
        const p = find(this.texts.get(doc)!, v[key], v.nth ?? 0, v.offset ?? 0);
        if (key === '$pos') return p;
        const q = { ...p };
        if (key === '$span') q.character += v[key].length;
        return { start: p, end: q };
      }
      if (key === '$diagnostics') return this.diags.get(v.$diagnostics) ?? [];
      if (key === '$answer') return this.answers.get(v.$answer)[v.index ?? 0];
      return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, this.resolve(x, doc)]));
    }
    return v;
  }
  paramsOf(step: any): any {
    const params = step.params ?? {};
    let doc: string | null = null;
    const td = params.textDocument;
    if (td && typeof td === 'object' && 'uri' in td) doc = this.fileOf(this.resolve(td.uri, null));
    return this.resolve(params, doc);
  }
  /** temp paths and URI encodings normalized; the server's version too */
  norm(v: any): any {
    if (typeof v === 'string') return v.split(this.ws).join('<ws>').split('%2F').join('/');
    if (Array.isArray(v)) return v.map((x) => this.norm(x));
    if (v && typeof v === 'object') {
      const out: any = Object.fromEntries(
        Object.entries(v).map(([k, x]) => [this.norm(k), this.norm(x)]),
      );
      if (out.serverInfo && typeof out.serverInfo === 'object' && 'version' in out.serverInfo)
        out.serverInfo = { ...out.serverInfo, version: '<version>' };
      return out;
    }
    return v;
  }
  record(label: string | undefined, value: any) {
    if (label === undefined) return;
    this.answers.set(label, value);
    this.transcript.push([label, this.norm(value)]);
  }
  static observed(seen: Msg[]) {
    const rows = seen.map((m) => [
      m.method ?? 'response',
      'id' in m
        ? typeof m.id === 'string'
          ? 'str'
          : Number.isInteger(m.id)
            ? 'int'
            : 'float'
        : null,
      m.params && typeof m.params === 'object' ? (m.params.value?.kind ?? null) : null,
    ]);
    const create = seen.find((m) => m.method === 'window/workDoneProgress/create');
    return { seen: rows, 'create id is an integer': !!create && Number.isInteger(create.id) };
  }
  async run(): Promise<[string, any][]> {
    const steps = JSON.parse(readFileSync(join(this.caseDir, 'session.json'), 'utf8')).steps;
    for (const step of steps) {
      const label = step.label;
      if ('open' in step || 'change' in step) {
        const file = step.open ?? step.change;
        this.texts.set(file, step.text);
        if ('open' in step) {
          this.versions.set(file, 1);
          this.openFiles.push(file);
          this.notify('textDocument/didOpen', {
            textDocument: { uri: this.uri(file), languageId: 'decl', version: 1, text: step.text },
          });
        } else {
          this.versions.set(file, this.versions.get(file)! + 1);
          this.notify('textDocument/didChange', {
            textDocument: { uri: this.uri(file), version: this.versions.get(file) },
            contentChanges: [{ text: step.text }],
          });
        }
        const [diags, seen] = await this.diagnostics(this.uri(file));
        this.diags.set(file, diags);
        this.record(label, step.observe ? Session.observed(seen) : diags);
      } else if ('request' in step) {
        const [answer, between] = await this.request(step.request, this.paramsOf(step));
        if (step.between)
          this.record(label, {
            answered: !(answer && typeof answer === 'object' && 'error' in answer),
            between,
          });
        else this.record(label, answer);
      } else if ('notify' in step) this.notify(step.notify, this.paramsOf(step));
      else if ('config' in step) {
        this.notify('workspace/didChangeConfiguration', { settings: step.config });
        for (const file of this.openFiles)
          this.diags.set(file, (await this.diagnostics(this.uri(file)))[0]);
      } else if ('respond' in step) {
        this.server.send({
          jsonrpc: '2.0',
          id: this.pendingRequest(step.respond),
          result: step.result ?? null,
        });
      } else throw new Error(`unknown step: ${JSON.stringify(step)}`);
    }
    this.server.close();
    return this.transcript;
  }
}

/** the corpus's cases, sorted */
export function cases(): string[] {
  return readdirSync(join(root, 'tests/lsp'))
    .filter((c) => existsSync(join(root, 'tests/lsp', c, 'session.json')))
    .sort()
    .map((c) => join(root, 'tests/lsp', c));
}

/** a replayed transcript against the committed one, answer by answer */
export function compare(
  name: string,
  got: [string, any][],
  check: (n: string, ok: boolean, detail?: string) => void,
) {
  const want: [string, any][] = JSON.parse(readFileSync(join(name, 'transcript.json'), 'utf8'));
  const c = name.slice(name.lastIndexOf('/') + 1);
  check(`${c}: ${want.length} answers`, want.length === got.length, `got ${got.length}`);
  for (let i = 0; i < Math.min(want.length, got.length); i++) {
    const same = isDeepStrictEqual(JSON.parse(JSON.stringify(got[i])), want[i]);
    check(
      `${c}: ${want[i][0]}`,
      same,
      same
        ? ''
        : `\n       expected ${JSON.stringify(want[i][1]).slice(0, 200)}\n       got      ${JSON.stringify(got[i][1]).slice(0, 200)}`,
    );
  }
}
