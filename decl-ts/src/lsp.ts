#!/usr/bin/env node
// decl-lsp (docs/tooling/03_lsp.md): the language server over stdio.
// Every answer comes from the same checker, inference, and engine as
// the command line, driven through the session object (session.ts) with
// the open buffers overriding the disk; positions come from the source
// ranges every AST node carries, and the types and resolutions recorded
// while the checker runs (infer.ts hooks). Messages are handled strictly
// in order, and the server exits when its input closes.
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, resolve as absPath } from 'node:path';
import { readFileSync } from 'node:fs';
import { parseSource } from './parse.ts';
import { initParser } from './node.ts';
import { checkModule } from './checker.ts';
import { format } from './fmt.ts';
import { typeText, resolveIn } from './infer.ts';
import type { Ty, Target } from './infer.ts';
import { Session, SessionError, fmtDiag } from './session.ts';
import type { Run } from './session.ts';
import type { Module } from './module.ts';
import type { Decl, Expr, Loc, MemberAst, TypeAst } from './ast.ts';
import type { Diag, Seg } from './semantics.ts';
import { parsePath, segText } from './semantics.ts';

// ---------------- transport ----------------
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

// ---------------- documents ----------------
const docs = new Map<string, string>();            // uri -> text
const overlay = new Map<string, string>();         // path -> text (open buffers override the disk)
const pathOf = (uri: string) => fileURLToPath(uri);
const uriOf = (path: string) => pathToFileURL(path).toString();
type Pos = { line: number; character: number };
type Range = { start: Pos; end: Pos };
const rangeOf = (l: Loc): Range => ({ start: { line: l.sl, character: l.sc }, end: { line: l.el, character: l.ec } });
const contains = (l: Loc, p: Pos) =>
  (l.sl < p.line || (l.sl === p.line && l.sc <= p.character)) && (p.line < l.el || (p.line === l.el && p.character <= l.ec));
const span = (l: Loc) => (l.el - l.sl) * 100000 + (l.ec - l.sc);
const config = { inputs: {} as Record<string, string> };

// ---------------- analysis ----------------
// one analysis per open document: its universe (the document as entry),
// and for every module the checker's tables — the type of every
// expression and what every name denotes
type Tables = { types: Map<Expr, Ty>; res: Map<Expr, Target | null> };
type Analysis = { path: string; text: string; session: Session; run: Run; tables: Map<string, Tables> };
const analyses = new Map<string, Analysis>();
const lastGood = new Map<string, Analysis>();      // the last analysis of a document that parsed (completion while typing)

function analysisOf(uri: string): Analysis | null {
  const text = docs.get(uri);
  if (text === undefined) return null;
  const have = analyses.get(uri);
  if (have && have.text === text) return have;
  const path = pathOf(uri);
  if (parseSource(text).errors.length) return null;
  const session = new Session(path, overlay);
  const run = session.run(undefined, 'full');
  const a: Analysis = { path, text, session, run, tables: new Map() };
  analyses.set(uri, a);
  lastGood.set(uri, a);
  return a;
}
function tablesOf(a: Analysis, m: Module): Tables {
  const have = a.tables.get(m.path);
  if (have) return have;
  const t: Tables = { types: new Map(), res: new Map() };
  checkModule(m.decls, m.env, { record: (e, ty) => t.types.set(e, ty), resolveHook: (e, target) => t.res.set(e, target) });
  a.tables.set(m.path, t);
  return t;
}
const moduleOf = (a: Analysis, path: string): Module | undefined => a.run.modules.find(m => m.path === path);
const textOf = (a: Analysis, m: Module): string => overlay.get(m.path) ?? readText(m.path);
function readText(path: string): string {
  try { return readFileSync(path, 'utf8'); } catch { return ''; }
}

// ---------------- diagnostics ----------------
function anchorFor(src: string, message: string): Loc {
  const names = message.match(/[A-Za-z_][A-Za-z0-9_.]*/g) ?? [];
  const lines = src.split('\n');
  for (const n of names) {
    if (['error', 'in', 'the', 'a', 'is', 'not', 'std', 'module', 'import', 'type', 'name'].includes(n)) continue;
    const re = new RegExp(`\\b${n.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`);
    for (let i = 0; i < lines.length; i++) {
      const mm = re.exec(lines[i]);
      if (mm) return { sl: i, sc: mm.index, el: i, ec: mm.index + n.length };
    }
  }
  return { sl: 0, sc: 0, el: 0, ec: Math.max(1, (lines[0] ?? '').length) };
}
// the source position of a document path: the literal the path leads to
// in the root's declaration, or the deepest literal on the way
function locOfPath(decls: Decl[], segs: Seg[]): Loc | null {
  const root = segs[0] as string;
  const decl = decls.find(d => (d.d === 'output' || d.d === 'input') && d.name === root);
  if (!decl?.loc) return null;
  let e: Expr | undefined = decl.d === 'output' ? decl.expr : decl.fallback;
  let best: Loc = decl.loc;
  for (const s of segs.slice(1)) {
    if (!e) break;
    let next: Expr | undefined;
    const k = segText(s);
    if (e.e === 'paren') e = e.x;
    if (e.e === 'obj' && typeof k === 'string') next = e.entries.find(en => en.key === k)?.val;
    else if (e.e === 'arr' && typeof k === 'number') next = e.items[k]?.expr;
    else if (e.e === 'with') { e = e.base; continue; }
    if (!next) break;
    e = next;
    if (e.loc) best = e.loc;
  }
  return best;
}
const severityOf = (s: string) => s === 'error' ? 1 : s === 'warning' ? 2 : 3;

function analyze(uri: string) {
  const src = docs.get(uri)!;
  const path = pathOf(uri);
  const out: any[] = [];
  const push = (loc: Loc, d: Diag) => {
    const item: any = { range: rangeOf(loc), severity: severityOf(d.severity), source: 'decl' };
    if (d.code || d.id) item.code = d.id ?? d.code;
    item.message = d.path ? `${d.message} (at ${d.path})` : d.message;
    out.push(item);
  };
  const { errors, decls } = parseSource(src);
  if (errors.length) {
    for (const e of errors) out.push({ range: { start: { line: e.row, character: e.col }, end: { line: e.row, character: e.col + 1 } }, severity: 1, source: 'decl', code: 'E2001', message: 'syntax error' });
  } else {
    const a = analysisOf(uri)!;
    const r = a.run;
    for (const d of r.loadDiags) {
      // a loading problem is anchored to the import it concerns when one is named
      const imp = decls.find(x => (x.d === 'import' || x.d === 're_export') && x.loc && d.message.includes(x.from.replace(/^\.\//, '').replace(/\.decl$/, '')));
      push(imp?.loc ?? anchorFor(src, d.message), d);
    }
    for (const c of r.checks) {
      if (c.file !== path) continue;
      push(c.diag.loc ?? anchorFor(src, c.diag.message), c.diag);
    }
    for (const d of r.diags) {
      if (d.severity === 'information') continue;
      let segs: Seg[] | null = null;
      try { segs = d.path ? parsePath(d.path, '') : null; } catch { segs = null; }
      const loc = segs ? locOfPath(decls, segs) : null;
      if (!loc) continue;                              // a root declared elsewhere: its own module's business
      push(loc, d);
    }
  }
  notify('textDocument/publishDiagnostics', { uri, diagnostics: out });
}

// ---------------- positions -> nodes ----------------
type Hit = { node: any; parents: any[] };
// the innermost AST node (declaration, member, type, or expression) at a position
function nodeAt(decls: Decl[], pos: Pos): Hit | null {
  let best: Hit | null = null;
  const visit = (x: any, parents: any[]) => {
    if (!x || typeof x !== 'object') return;
    if (Array.isArray(x)) { for (const y of x) visit(y, parents); return; }
    const own = x.loc && contains(x.loc, pos);
    if (own && (!best || span(x.loc) <= span(best.node.loc))) best = { node: x, parents };
    for (const [k, v] of Object.entries(x)) {
      if (k === 'loc' || !v || typeof v !== 'object') continue;
      visit(v, own ? [...parents, x] : parents);
    }
  };
  visit(decls, []);
  return best;
}
const isExpr = (x: any) => x && typeof x.e === 'string';
const isType = (x: any) => x && typeof x.k === 'string';
const isDecl = (x: any) => x && typeof x.d === 'string';
const isMember = (x: any) => x && typeof x.m === 'string';

// the range of a declaration's name token (the declaration site)
function nameRange(text: string, decl: Decl, name: string): Loc {
  const loc = decl.loc!;
  const lines = text.split('\n');
  for (let i = loc.sl; i <= loc.el && i < lines.length; i++) {
    const from = i === loc.sl ? loc.sc : 0;
    const re = new RegExp(`\\b${name}\\b`, 'g');
    re.lastIndex = from;
    const m = re.exec(lines[i]);
    if (m) return { sl: i, sc: m.index, el: i, ec: m.index + name.length };
  }
  return loc;
}
function memberRange(text: string, member: MemberAst, name: string): Loc {
  const loc = member.loc!;
  const line = text.split('\n')[loc.sl] ?? '';
  const i = line.indexOf(name, loc.sc);
  return i >= 0 ? { sl: loc.sl, sc: i, el: loc.sl, ec: i + name.length } : loc;
}

// ---------------- what is under the cursor ----------------
type Site = { kind: string; module: Module; decl?: Decl; member?: MemberAst; range: Loc; name: string; type?: Ty };

// the declaration a target denotes, as a site in its module
function siteOfTarget(a: Analysis, t: Target | null): Site | null {
  if (!t || !t.env) return null;
  const m = a.run.modules.find(x => x.env === t.env);
  if (!m) return null;
  const text = textOf(a, m);
  const decl = m.decls.find(d => (d as any).name === t.name && d.loc && d.d !== 'import');
  if (decl) return { kind: decl.d, module: m, decl, range: nameRange(text, decl, t.name), name: t.name };
  return null;
}
function memberSite(a: Analysis, m: Module, rt: any, member: string): Site | null {
  // the member's declaring type, extension chains followed (§4)
  const seen = new Set<string>();
  let typeName: string | undefined = rt?.t === 'rec' ? rt.name : rt?.t === 'pred' ? rt.base?.name : undefined;
  while (typeName && !seen.has(typeName)) {
    seen.add(typeName);
    const target = resolveIn(m.env, typeName);
    const site = siteOfTarget(a, target);
    const decl: any = site?.decl;
    if (!decl || decl.d !== 'type') return null;
    const body: TypeAst = decl.type;
    const members: MemberAst[] = body.k === 'record' ? body.members : body.k === 'named' && body.ext?.k === 'record' ? body.ext.members : [];
    const mem = members.find((x: any) => x.name === member);
    if (mem?.loc) return { kind: 'member', module: site!.module, decl, member: mem, range: memberRange(textOf(a, site!.module), mem, member), name: member };
    typeName = body.k === 'named' ? body.name : undefined;
  }
  return null;
}

function siteAt(a: Analysis, uri: string, pos: Pos): { site: Site | null; type?: Ty; hit: Hit | null; module: Module } | null {
  const m = moduleOf(a, pathOf(uri));
  if (!m) return null;
  const hit = nodeAt(m.decls, pos);
  if (!hit) return { site: null, hit: null, module: m };
  const t = tablesOf(a, m);
  const n = hit.node;
  if (isExpr(n)) {
    const type = t.types.get(n);
    if (n.e === 'name') {
      const target = t.res.get(n) ?? resolveIn(m.env, n.name);
      return { site: siteOfTarget(a, target ?? null), type, hit, module: m };
    }
    if (n.e === 'member') {
      const x = n.x;
      if (x.e === 'name' && m.env.namespaces.has(x.name)) {
        const ns = m.env.namespaces.get(x.name)!;
        const ex = ns.exports.get(n.name);
        return { site: ex ? siteOfTarget(a, resolveIn(ex.env, ex.name)) : null, type, hit, module: m };
      }
      const xt = t.types.get(x);
      return { site: memberSite(a, m, xt?.rt, n.name), type, hit, module: m };
    }
    return { site: null, type, hit, module: m };
  }
  if (isType(n) && n.k === 'named') {
    const [head, tail] = n.name.split('.');
    let target: Target | null;
    if (tail && m.env.namespaces.has(head)) { const ex = m.env.namespaces.get(head)!.exports.get(tail); target = ex ? resolveIn(ex.env, ex.name) : null; }
    else target = resolveIn(m.env, head);
    return { site: siteOfTarget(a, target), hit, module: m };
  }
  if (isMember(n) && 'name' in n) {
    // the member's own declaration
    const decl = hit.parents.find(isDecl);
    return { site: decl ? { kind: 'member', module: m, decl, member: n, range: memberRange(textOf(a, m), n, n.name), name: n.name } : null, hit, module: m };
  }
  if (isDecl(n) && typeof n.name === 'string') {
    const r = nameRange(textOf(a, m), n, n.name);
    if (contains(r, pos)) return { site: { kind: n.d, module: m, decl: n, range: r, name: n.name }, hit, module: m };
  }
  return { site: null, hit, module: m };
}

// ---------------- hover ----------------
function declText(a: Analysis, site: Site): string[] {
  const text = textOf(a, site.module);
  const lines = text.split('\n');
  if (site.member?.loc) {
    const l = site.member.loc;
    const docLines: string[] = [];
    let from = l.sl;
    while (from > 0 && /^\s*\/\/\//.test(lines[from - 1])) { from--; docLines.unshift(lines[from].trim()); }
    const body = l.sl === l.el ? [lines[l.sl].slice(l.sc, l.ec)] : [lines[l.sl].slice(l.sc), ...lines.slice(l.sl + 1, l.el), lines[l.el].slice(0, l.ec)];
    return [...docLines, ...body.map(x => x.trim()).filter(Boolean)];
  }
  const l = site.decl!.loc!;
  const docLines: string[] = [];
  let from = l.sl;
  while (from > 0 && /^\s*\/\/\//.test(lines[from - 1])) { from--; docLines.unshift(lines[from].trim()); }
  const body = lines.slice(l.sl, l.el + 1);
  return [...docLines, ...(body.length > 12 ? [...body.slice(0, 11), '    …', body[body.length - 1]] : body)];
}
function hover(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  if (!a) return null;
  const s = siteAt(a, uri, pos);
  if (!s) return null;
  const parts: string[] = [];
  if (s.site) {
    const lines = declText(a, s.site);
    const doc = lines.filter(l => l.startsWith('///')).map(l => l.replace(/^\/\/\/\s?/, ''));
    const code = lines.filter(l => !l.startsWith('///'));
    if (doc.length) parts.push(doc.join('\n'));
    parts.push('```decl\n' + code.join('\n') + '\n```');
  }
  if (s.type) parts.push(`\`${typeText(s.type.rt)}${s.type.abs ? '?' : ''}\``);
  if (!parts.length) return null;
  const range = s.site && s.hit && isExpr(s.hit.node) && s.hit.node.loc ? rangeOf(s.hit.node.loc) : s.hit?.node?.loc ? rangeOf(s.hit.node.loc) : undefined;
  return range ? { contents: { kind: 'markdown', value: parts.join('\n\n') }, range } : { contents: { kind: 'markdown', value: parts.join('\n\n') } };
}

// ---------------- navigation ----------------
const location = (m: Module, loc: Loc) => ({ uri: uriOf(m.path), range: rangeOf(loc) });
function definition(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  return s?.site ? location(s.site.module, s.site.range) : null;
}
function typeDefinition(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  if (!s) return null;
  const rt = s.type?.rt;
  const name = rt?.t === 'rec' ? rt.name : rt?.t === 'pred' ? rt.base?.name : undefined;
  if (!name) return null;
  const site = siteOfTarget(a!, resolveIn(s.module.env, name));
  return site ? location(site.module, site.range) : null;
}
// every reference to a site across the universe: name and member nodes
// that resolve to the same declaration, plus the declaration itself
function references(uri: string, pos: Pos, includeDeclaration: boolean): { module: Module; loc: Loc }[] {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  if (!a || !s?.site) return [];
  const target = s.site;
  const out: { module: Module; loc: Loc }[] = [];
  const same = (x: Site | null) => !!x && x.module === target.module && x.name === target.name && x.kind === target.kind
    && (x.kind !== 'member' || x.decl === target.decl);
  for (const m of a.run.modules) {
    const t = tablesOf(a, m);
    const visit = (x: any) => {
      if (!x || typeof x !== 'object') return;
      if (Array.isArray(x)) { x.forEach(visit); return; }
      if (isExpr(x) && x.loc) {
        if (x.e === 'name' && same(siteOfTarget(a, t.res.get(x) ?? resolveIn(m.env, x.name)))) out.push({ module: m, loc: x.loc });
        if (x.e === 'member') {
          const xx = x.x;
          let site: Site | null = null;
          if (xx.e === 'name' && m.env.namespaces.has(xx.name)) { const ex = m.env.namespaces.get(xx.name)!.exports.get(x.name); site = ex ? siteOfTarget(a, resolveIn(ex.env, ex.name)) : null; }
          else site = memberSite(a, m, t.types.get(xx)?.rt, x.name);
          if (same(site)) out.push({ module: m, loc: memberTokenLoc(textOf(a, m), x) });
        }
      }
      if (isType(x) && x.k === 'named' && x.loc) {
        const [head, tail] = x.name.split('.');
        let tg: Target | null;
        if (tail && m.env.namespaces.has(head)) { const ex = m.env.namespaces.get(head)!.exports.get(tail); tg = ex ? resolveIn(ex.env, ex.name) : null; }
        else tg = resolveIn(m.env, head);
        if (same(siteOfTarget(a, tg))) out.push({ module: m, loc: typeNameLoc(x, tail ? head.length + 1 : 0, tail ?? head) });
      }
      // import items naming the declaration
      if (isDecl(x) && (x.d === 'import' || x.d === 're_export') && x.loc && x.names) {
        const tm = m.env.imports;
        for (const it of x.names) {
          const im = tm.get(it.as ?? it.name);
          if (im && same(siteOfTarget(a, resolveIn(im.env, im.name)))) out.push({ module: m, loc: importItemLoc(textOf(a, m), x, it.name) });
        }
      }
      for (const [k, v] of Object.entries(x)) if (k !== 'loc' && v && typeof v === 'object') visit(v);
    };
    visit(m.decls);
  }
  if (includeDeclaration) out.unshift({ module: target.module, loc: target.range });
  const key = (r: { module: Module; loc: Loc }) => `${r.module.path}:${r.loc.sl}:${r.loc.sc}`;
  const seen = new Set<string>();
  return out.filter(r => !seen.has(key(r)) && seen.add(key(r)))
    .sort((p, q) => p.module.path < q.module.path ? -1 : p.module.path > q.module.path ? 1 : p.loc.sl - q.loc.sl || p.loc.sc - q.loc.sc);
}
const memberTokenLoc = (text: string, e: any): Loc => {
  const l: Loc = e.loc;
  const line = text.split('\n')[l.el] ?? '';
  const i = line.lastIndexOf(e.name, l.ec);
  return i >= 0 ? { sl: l.el, sc: i, el: l.el, ec: i + e.name.length } : l;
};
const typeNameLoc = (t: any, offset: number, name: string): Loc =>
  ({ sl: t.loc.sl, sc: t.loc.sc + offset, el: t.loc.sl, ec: t.loc.sc + offset + name.length });
const importItemLoc = (text: string, d: any, name: string): Loc => {
  const l: Loc = d.loc;
  const line = text.split('\n')[l.sl] ?? '';
  const i = line.indexOf(name, l.sc);
  return i >= 0 ? { sl: l.sl, sc: i, el: l.sl, ec: i + name.length } : l;
};

// ---------------- completion ----------------
function completion(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  const text = docs.get(uri);
  if (!text) return { isIncomplete: false, items: [] };
  const line = text.split('\n')[pos.line] ?? '';
  const prefix = line.slice(0, pos.character);
  // while the text does not parse, the scope is the last one that did
  const session = a?.session ?? lastGood.get(uri)?.session ?? new Session(pathOf(uri), overlay);
  const items = session.complete(prefix, []).map(c => {
    const [label, detail] = c.split('  ');
    const kind = detail ? (detail.startsWith('derived') || detail.startsWith('required') || detail.startsWith('optional') || detail.startsWith('defaulted') ? 5 : 6)
      : /^[A-Z]/.test(label) ? 7 : label.startsWith('$') ? 14 : 6;
    const item: any = { label, kind };
    if (detail) item.detail = detail;
    return item;
  });
  return { isIncomplete: false, items };
}

// ---------------- symbols, folding, formatting ----------------
const SYMBOL_KIND: Record<string, number> = { type: 5, const: 14, func: 12, output: 13, input: 13, diagnostic: 24, dimension: 13, unit: 13 };
function documentSymbols(uri: string): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  const { decls, errors } = parseSource(text);
  if (errors.length) return [];
  const out: any[] = [];
  for (const d of decls as any[]) {
    if (!d.loc || typeof d.name !== 'string' || !(d.d in SYMBOL_KIND)) continue;
    const sym: any = { name: d.name, kind: SYMBOL_KIND[d.d], range: rangeOf(d.loc), selectionRange: rangeOf(nameRange(text, d, d.name)) };
    if (d.d === 'type') {
      const body = d.type.k === 'record' ? d.type : d.type.k === 'named' && d.type.ext?.k === 'record' ? d.type.ext : null;
      if (body) {
        const children = body.members.filter((m: any) => m.loc && typeof m.name === 'string').map((m: any) => ({
          name: m.m === 'assert' ? `assert ${m.name}` : m.hidden ? `${m.name}$` : m.name,
          kind: m.m === 'assert' ? 24 : 7,
          range: rangeOf(m.loc), selectionRange: rangeOf(memberRange(text, m, m.name)),
        }));
        if (children.length) sym.children = children;
      }
    }
    out.push(sym);
  }
  return out;
}
function foldingRanges(uri: string): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  const { decls, errors } = parseSource(text);
  if (errors.length) return [];
  const out: any[] = [];
  const visit = (x: any) => {
    if (!x || typeof x !== 'object') return;
    if (Array.isArray(x)) { x.forEach(visit); return; }
    if (x.loc && x.loc.el > x.loc.sl && (isDecl(x) || (isType(x) && x.k === 'record') || (isExpr(x) && (x.e === 'obj' || x.e === 'arr' || x.e === 'match')) || (isMember(x) && x.m === 'when')))
      out.push({ startLine: x.loc.sl, endLine: x.loc.el, kind: 'region' });
    for (const [k, v] of Object.entries(x)) if (k !== 'loc' && v && typeof v === 'object') visit(v);
  };
  visit(decls);
  const seen = new Set<string>();
  return out.filter(r => !seen.has(`${r.startLine}-${r.endLine}`) && seen.add(`${r.startLine}-${r.endLine}`));
}
function formatting(uri: string): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  let out: string;
  try { out = format(text); } catch { return []; }
  if (out === text) return [];
  const lines = text.split('\n');
  return [{ range: { start: { line: 0, character: 0 }, end: { line: lines.length - 1, character: lines[lines.length - 1].length } }, newText: out }];
}

// ---------------- rename ----------------
function prepareRename(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  if (!s?.site || !s.hit?.node?.loc) return null;
  const n = s.hit.node;
  const loc: Loc = isExpr(n) && n.e === 'member' ? memberTokenLoc(textOf(a!, s.module), n)
    : isType(n) ? typeNameLoc(n, n.name.includes('.') ? n.name.indexOf('.') + 1 : 0, n.name.split('.').pop())
    : isDecl(n) || isMember(n) ? s.site.range : n.loc;
  return { range: rangeOf(loc), placeholder: s.site.name };
}
function rename(uri: string, pos: Pos, newName: string): any {
  const refs = references(uri, pos, true);
  if (!refs.length) return null;
  const changes: Record<string, any[]> = {};
  for (const r of refs) (changes[uriOf(r.module.path)] ??= []).push({ range: rangeOf(r.loc), newText: newName });
  return { changes };
}

// ---------------- lenses and commands ----------------
function codeLenses(uri: string): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  const { decls, errors } = parseSource(text);
  if (errors.length) return [];
  const out: any[] = [];
  for (const d of decls as any[]) {
    if (!d.loc) continue;
    if (d.d === 'output') out.push({ range: rangeOf({ sl: d.loc.sl, sc: d.loc.sc, el: d.loc.sl, ec: d.loc.sc }), command: { title: 'evaluate', command: 'decl.evaluate', arguments: [uri, d.name] } });
    if (d.d === 'input') out.push({ range: rangeOf({ sl: d.loc.sl, sc: d.loc.sc, el: d.loc.sl, ec: d.loc.sc }), command: { title: 'validate', command: 'decl.validate', arguments: [uri, d.name] } });
  }
  return out;
}
function executeCommand(command: string, args: any[]): any {
  const [uri, root] = args ?? [];
  if (typeof uri !== 'string') return null;
  const session = new Session(pathOf(uri), overlay);
  for (const [name, file] of Object.entries(config.inputs)) {
    try { session.apply({ op: 'bind', name, src: { kind: 'file', file, text: readText(absPath(dirname(pathOf(uri)), file)) } }); } catch { /* reported by :validate */ }
  }
  switch (command) {
    case 'decl.evaluate': {
      const { run, docs: ds } = session.evaluate(root ? [root] : []);
      const diags = [...run.loadDiags, ...run.checks.map(c => c.diag), ...run.diags].map(d => fmtDiag(d));
      if (root) return { root, document: ds[0]?.json ?? null, diagnostics: diags };
      const all = run.eng && ds.every(d => d.json !== null) ? `{${ds.map(d => `${JSON.stringify(d.name)}:${d.json}`).join(',')}}` : null;
      return { root: null, document: all, diagnostics: diags };
    }
    case 'decl.validate': {
      const { run, verdicts, diags } = session.validate(root ? [root] : []);
      return { verdicts, diagnostics: [...run.loadDiags, ...run.checks.map(c => c.diag), ...diags].map(d => fmtDiag(d)) };
    }
    case 'decl.trace': return root ? { lines: session.trace(root) } : null;
    case 'decl.reloadWorkspace': analyses.clear(); for (const u of docs.keys()) analyze(u); return null;
    default: return null;
  }
}

// ---------------- request handling ----------------
async function handle(msg: any) {
  const { id, method, params } = msg;
  switch (method) {
    case 'initialize':
      await initParser();
      reply(id, {
        capabilities: {
          textDocumentSync: 1,
          hoverProvider: true,
          definitionProvider: true,
          typeDefinitionProvider: true,
          referencesProvider: true,
          documentHighlightProvider: true,
          documentSymbolProvider: true,
          foldingRangeProvider: true,
          documentFormattingProvider: true,
          renameProvider: { prepareProvider: true },
          completionProvider: { triggerCharacters: ['.', '$', ':'] },
          codeLensProvider: { resolveProvider: false },
          executeCommandProvider: { commands: ['decl.evaluate', 'decl.validate', 'decl.trace', 'decl.reloadWorkspace'] },
        },
        serverInfo: { name: 'decl-lsp', version: '0.3.0' },
      });
      break;
    case 'initialized': break;
    case 'workspace/didChangeConfiguration':
      config.inputs = params?.settings?.decl?.inputs ?? {};
      analyses.clear();
      for (const u of docs.keys()) analyze(u);
      break;
    case 'workspace/didChangeWatchedFiles':
      analyses.clear();
      for (const u of docs.keys()) analyze(u);
      break;
    case 'textDocument/didOpen':
      docs.set(params.textDocument.uri, params.textDocument.text);
      overlay.set(pathOf(params.textDocument.uri), params.textDocument.text);
      analyses.clear();
      analyze(params.textDocument.uri);
      break;
    case 'textDocument/didChange':
      docs.set(params.textDocument.uri, params.contentChanges[0].text);
      overlay.set(pathOf(params.textDocument.uri), params.contentChanges[0].text);
      analyses.clear();
      analyze(params.textDocument.uri);
      break;
    case 'textDocument/didSave': break;
    case 'textDocument/didClose':
      docs.delete(params.textDocument.uri);
      overlay.delete(pathOf(params.textDocument.uri));
      analyses.delete(params.textDocument.uri);
      lastGood.delete(params.textDocument.uri);
      notify('textDocument/publishDiagnostics', { uri: params.textDocument.uri, diagnostics: [] });
      break;
    case 'textDocument/hover': reply(id, hover(params.textDocument.uri, params.position)); break;
    case 'textDocument/definition': reply(id, definition(params.textDocument.uri, params.position)); break;
    case 'textDocument/typeDefinition': reply(id, typeDefinition(params.textDocument.uri, params.position)); break;
    case 'textDocument/references':
      reply(id, references(params.textDocument.uri, params.position, !!params.context?.includeDeclaration).map(r => location(r.module, r.loc)));
      break;
    case 'textDocument/documentHighlight': {
      const path = pathOf(params.textDocument.uri);
      reply(id, references(params.textDocument.uri, params.position, true).filter(r => r.module.path === path).map(r => ({ range: rangeOf(r.loc), kind: 1 })));
      break;
    }
    case 'textDocument/completion': reply(id, completion(params.textDocument.uri, params.position)); break;
    case 'textDocument/documentSymbol': reply(id, documentSymbols(params.textDocument.uri)); break;
    case 'textDocument/foldingRange': reply(id, foldingRanges(params.textDocument.uri)); break;
    case 'textDocument/formatting': reply(id, formatting(params.textDocument.uri)); break;
    case 'textDocument/prepareRename': reply(id, prepareRename(params.textDocument.uri, params.position)); break;
    case 'textDocument/rename': reply(id, rename(params.textDocument.uri, params.position, params.newName)); break;
    case 'textDocument/codeLens': reply(id, codeLenses(params.textDocument.uri)); break;
    case 'workspace/executeCommand': {
      // a refused command (an unknown root, an unreadable binding) answers null, never silence
      let result: any = null;
      try { result = executeCommand(params.command, params.arguments); }
      catch (e: any) { if (!(e instanceof SessionError)) throw e; }
      reply(id, result);
      break;
    }
    case 'shutdown': reply(id, null); break;
    case 'exit': process.exit(0); break;
    default:
      if (id !== undefined) reply(id, null);
  }
}
