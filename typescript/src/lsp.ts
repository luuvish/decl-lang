#!/usr/bin/env node
// Minimal LSP server over stdio (ROADMAP Phase 4): diagnostics first,
// then hover, then definition — module-aware through the same loader
// the CLI uses, with open buffers overriding the disk.
import { fileURLToPath, pathToFileURL } from 'node:url';
import { Parser, Language } from 'web-tree-sitter';
import { join, dirname } from 'node:path';
import { readFileSync } from 'node:fs';
import { initParser, parseSource, WASM } from './parse.ts';
import { checkModule } from './checker.ts';
import { loadModules } from './module.ts';
import { openPackageUniverse } from './package.ts';

// ---------------- transport ----------------
// messages are handled strictly in order (a client may pipe `initialize`
// and `exit` back to back); the server also exits when its stdin closes
let buffer = Buffer.alloc(0);
let queue: Promise<void> = Promise.resolve();
process.stdin.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  for (; ;) {
    const headerEnd = buffer.indexOf('\r\n\r\n');
    if (headerEnd < 0) return;
    const header = buffer.subarray(0, headerEnd).toString();
    const m = /Content-Length: (\d+)/i.exec(header);
    if (!m) { buffer = buffer.subarray(headerEnd + 4); continue; }
    const len = parseInt(m[1], 10);
    if (buffer.length < headerEnd + 4 + len) return;
    const body = buffer.subarray(headerEnd + 4, headerEnd + 4 + len).toString();
    buffer = buffer.subarray(headerEnd + 4 + len);
    queue = queue.then(() => handle(JSON.parse(body))).catch(e => logErr(String(e?.stack ?? e)));
  }
});
process.stdin.on('end', () => { queue.then(() => process.exit(0)); });
function send(msg: any) {
  const body = JSON.stringify(msg);
  process.stdout.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}
const reply = (id: any, result: any) => send({ jsonrpc: '2.0', id, result });
const notify = (method: string, params: any) => send({ jsonrpc: '2.0', method, params });
const logErr = (message: string) => notify('window/logMessage', { type: 1, message });

// ---------------- documents & analysis ----------------
const docs = new Map<string, string>();          // uri -> text
let tsLang: Language | null = null;
async function ensureInit() {
  if (tsLang) return;
  await initParser();
  await Parser.init();
  tsLang = await Language.load(WASM);
}
const pathOf = (uri: string) => fileURLToPath(uri);
const uriOf = (path: string) => pathToFileURL(path).toString();

function parseTree(src: string) {
  const p = new Parser();
  p.setLanguage(tsLang!);
  return p.parse(src)!;
}

// find an identifier's position to anchor a position-less diagnostic
function anchorFor(src: string, message: string): { line: number; a: number; b: number } {
  const names = message.match(/[A-Za-z_][A-Za-z0-9_.]*/g) ?? [];
  for (const n of names) {
    if (['error', 'in', 'the', 'a', 'is', 'not', 'std'].includes(n)) continue;
    const re = new RegExp(`\\b${n.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`);
    const lines = src.split('\n');
    for (let i = 0; i < lines.length; i++) {
      const mm = re.exec(lines[i]);
      if (mm) return { line: i, a: mm.index, b: mm.index + n.length };
    }
  }
  return { line: 0, a: 0, b: 1 };
}

function analyze(uri: string) {
  const src = docs.get(uri)!;
  const path = pathOf(uri);
  const lspDiags: any[] = [];
  const push = (line: number, a: number, b: number, message: string, code?: string) =>
    lspDiags.push({ range: { start: { line, character: a }, end: { line, character: b } },
      severity: 1, source: 'decl', code, message });

  const { errors } = parseSource(src);
  if (errors.length) {
    for (const e of errors) push(e.row, e.col, e.col + 1, 'syntax error', 'E2001');
  } else {
    const pkg = openPackageUniverse(path);
    const override = new Map<string, string>();
    for (const [u, text] of docs) override.set(pathOf(u), text);
    const { modules, diags } = loadModules(path, pkg?.resolver, override);
    const mine = modules.find(m => m.path === path);
    const all = [...(pkg?.diags ?? []), ...diags,
      ...(mine ? checkModule(mine.decls, mine.env) : [])];
    for (const d of all.filter(d => d.severity === 'error')) {
      const at = anchorFor(src, d.message);
      push(at.line, at.a, at.b, d.message, d.code);
    }
  }
  notify('textDocument/publishDiagnostics', { uri, diagnostics: lspDiags });
}

// ---------------- declarations index (hover / definition) ----------------
type DeclSite = { path: string; row: number; a: number; b: number; line: string; kind: string };

function declIndex(path: string, src: string): Map<string, DeclSite> {
  const out = new Map<string, DeclSite>();
  const tree = parseTree(src);
  const lines = src.split('\n');
  for (const c of tree.rootNode.namedChildren) {
    if (!c || !c.type.endsWith('_declaration')) continue;
    const nameNode = c.childForFieldName('name');
    if (!nameNode) continue;
    out.set(nameNode.text, {
      path, row: nameNode.startPosition.row,
      a: nameNode.startPosition.column, b: nameNode.endPosition.column,
      line: lines[c.startPosition.row].trim(), kind: c.type.replace('_declaration', ''),
    });
  }
  return out;
}

function readSrc(path: string): string | null {
  const open = [...docs.entries()].find(([u]) => pathOf(u) === path);
  if (open) return open[1];
  try { return readFileSync(path, 'utf8'); }
  catch { return null; }
}

// resolve the name under the cursor to its declaration site, following
// one import hop (named, renamed, or namespace member)
function findDecl(uri: string, pos: { line: number; character: number }): DeclSite | null {
  const src = docs.get(uri);
  if (!src) return null;
  const path = pathOf(uri);
  const tree = parseTree(src);
  const node = tree.rootNode.descendantForPosition(
    { row: pos.line, column: pos.character },
    { row: pos.line, column: pos.character });
  if (!node || node.type !== 'identifier') return null;
  const word = node.text;

  const local = declIndex(path, src);
  if (local.has(word)) return local.get(word)!;

  // namespace member: ns.word — look at the sibling chain
  const prevSib = node.parent?.type === 'qualified_name' ? node.parent.namedChildren[0] : null;
  const nsName = prevSib && prevSib.id !== node.id ? prevSib.text : null;

  const { decls } = parseSource(src);
  for (const d of decls) {
    if (d.d !== 'import') continue;
    const target = d.from.startsWith('.')
      ? join(dirname(path), d.from)
      : (() => { const p = openPackageUniverse(path); const r = p?.resolver(d.from, dirname(path)); return typeof r === 'string' ? r : null; })();
    if (!target) continue;
    const tsrc = readSrc(target);
    if (!tsrc) continue;
    if (d.ns !== undefined && d.ns === nsName) {
      const tidx = declIndex(target, tsrc);
      if (tidx.has(word)) return tidx.get(word)!;
    }
    for (const it of d.names ?? []) {
      if ((it.as ?? it.name) !== word) continue;
      const tidx = declIndex(target, tsrc);
      if (tidx.has(it.name)) return tidx.get(it.name)!;
    }
  }
  return null;
}

// ---------------- request handling ----------------
async function handle(msg: any) {
  const { id, method, params } = msg;
  switch (method) {
    case 'initialize':
      await ensureInit();
      reply(id, {
        capabilities: {
          textDocumentSync: 1,
          hoverProvider: true,
          definitionProvider: true,
        },
        serverInfo: { name: 'decl-lsp', version: '0.2.0' },
      });
      break;
    case 'initialized': break;
    case 'textDocument/didOpen':
      docs.set(params.textDocument.uri, params.textDocument.text);
      analyze(params.textDocument.uri);
      break;
    case 'textDocument/didChange':
      docs.set(params.textDocument.uri, params.contentChanges[0].text);
      analyze(params.textDocument.uri);
      break;
    case 'textDocument/didClose':
      docs.delete(params.textDocument.uri);
      break;
    case 'textDocument/hover': {
      const site = findDecl(params.textDocument.uri, params.position);
      reply(id, site ? {
        contents: { kind: 'markdown', value: `**${site.kind}** — \`${site.line}\`` },
      } : null);
      break;
    }
    case 'textDocument/definition': {
      const site = findDecl(params.textDocument.uri, params.position);
      reply(id, site ? {
        uri: uriOf(site.path),
        range: { start: { line: site.row, character: site.a }, end: { line: site.row, character: site.b } },
      } : null);
      break;
    }
    case 'shutdown': reply(id, null); break;
    case 'exit': process.exit(0); break;
    default:
      if (id !== undefined) reply(id, null);
  }
}
