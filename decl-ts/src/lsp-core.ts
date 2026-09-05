// decl-lsp's core (docs/tooling/03_lsp.md): every answer of the language
// server, independent of the transport — stdio under Node (lsp.ts) or a
// web worker in the browser (lsp-web.ts). Every answer comes from the
// same checker, inference, and engine as the command line, driven
// through the session object (session.ts) with the open buffers
// overriding the host's files; positions come from the source ranges
// every AST node carries, and the types and resolutions recorded while
// the checker runs (infer.ts hooks). Messages are handled strictly in
// order.
import { host, dirname, resolvePath as absPath } from './host.ts';
import { parseSource, getLanguage } from './parse.ts';
import { Parser } from 'web-tree-sitter';
import { checkModule } from './checker.ts';
import { format } from './fmt.ts';
import { typeText, resolveIn, stdPath, STD } from './infer.ts';
import type { Ty, Target } from './infer.ts';
import { Session, SessionError, fmtDiag, exprText } from './session.ts';
import type { Run } from './session.ts';
import type { Module } from './module.ts';
import type { Decl, Expr, Loc, MemberAst, TypeAst } from './ast.ts';
import type { Diag, Seg } from './semantics.ts';
import { parsePath, segText, isRec, isArr, isMap } from './semantics.ts';

// ---------------- the connection ----------------
// the transport gives the core how to send, how to initialize the parser
// (from disk, or from the wasm the client hands over), and how to exit
export type Io = {
  send: (msg: any) => void;
  init: (initializationOptions: any) => Promise<void>;
  exit: () => void;
};
let io: Io = { send: () => {}, init: async () => {}, exit: () => {} };
let queue: Promise<void> = Promise.resolve();
/** connect a transport; returns the function it feeds messages to (handled strictly in order) */
export function connect(transport: Io): (msg: any) => void {
  io = transport;
  return (msg: any) => {
    // a request whose handler throws is answered with an error, never left waiting
    queue = queue
      .then(() => handle(msg))
      .catch((e) => {
        logErr(String(e?.stack ?? e));
        if (msg.id !== undefined)
          send({
            jsonrpc: '2.0',
            id: msg.id,
            error: { code: -32603, message: String(e?.message ?? e) },
          });
      });
  };
}
/** the queue of messages handled so far (the transport waits for it before exiting) */
export function drained(): Promise<void> {
  return queue;
}
function send(msg: any) {
  io.send(msg);
}
const reply = (id: any, result: any) => send({ jsonrpc: '2.0', id, result });
const notify = (method: string, params: any) => send({ jsonrpc: '2.0', method, params });
const logErr = (message: string) => notify('window/logMessage', { type: 1, message });

// ---------------- documents ----------------
const docs = new Map<string, string>(); // uri -> text
const overlay = new Map<string, string>(); // path -> text (open buffers override the disk)
const pathOf = (uri: string) => host.pathOf(uri);
const uriOf = (path: string) => host.uriOf(path);
type Pos = { line: number; character: number };
type Range = { start: Pos; end: Pos };
const rangeOf = (l: Loc): Range => ({
  start: { line: l.sl, character: l.sc },
  end: { line: l.el, character: l.ec },
});
const contains = (l: Loc, p: Pos) =>
  (l.sl < p.line || (l.sl === p.line && l.sc <= p.character)) &&
  (p.line < l.el || (p.line === l.el && p.character <= l.ec));
const span = (l: Loc) => (l.el - l.sl) * 100000 + (l.ec - l.sc);
const config = { inputs: {} as Record<string, string> };

// ---------------- analysis ----------------
// one analysis per open document: its universe (the document as entry),
// and for every module the checker's tables — the type of every
// expression and what every name denotes
type Tables = { types: Map<Expr, Ty>; res: Map<Expr, Target | null> };
type Analysis = {
  path: string;
  text: string;
  session: Session;
  run: Run;
  tables: Map<string, Tables>;
};
const analyses = new Map<string, Analysis>();
const lastGood = new Map<string, Analysis>(); // the last analysis of a document that parsed (completion while typing)

function analysisOf(uri: string): Analysis | null {
  const text = docs.get(uri);
  if (text === undefined) return null;
  const have = analyses.get(uri);
  if (have && have.text === text) return have;
  const path = pathOf(uri);
  if (parseSource(text).errors.length) return null;
  const session = new Session(path, overlay);
  const run = withProgress(`Decl: evaluating ${path.split('/').pop()}`, () =>
    session.run(undefined, 'full'),
  );
  const a: Analysis = { path, text, session, run, tables: new Map() };
  analyses.set(uri, a);
  lastGood.set(uri, a);
  return a;
}
function tablesOf(a: Analysis, m: Module): Tables {
  const have = a.tables.get(m.path);
  if (have) return have;
  const t: Tables = { types: new Map(), res: new Map() };
  checkModule(m.decls, m.env, {
    record: (e, ty) => t.types.set(e, ty),
    resolveHook: (e, target) => t.res.set(e, target),
  });
  a.tables.set(m.path, t);
  return t;
}
const moduleOf = (a: Analysis, path: string): Module | undefined =>
  a.run.modules.find((m) => m.path === path);
const textOf = (a: Analysis, m: Module): string => overlay.get(m.path) ?? readText(m.path);
function readText(path: string): string {
  return host.readFile(path) ?? '';
}

// ---------------- diagnostics ----------------
function anchorFor(src: string, message: string): Loc {
  const names = message.match(/[A-Za-z_][A-Za-z0-9_.]*/g) ?? [];
  const lines = src.split('\n');
  for (const n of names) {
    if (
      ['error', 'in', 'the', 'a', 'is', 'not', 'std', 'module', 'import', 'type', 'name'].includes(
        n,
      )
    )
      continue;
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
  const decl = decls.find(
    (d): d is Extract<Decl, { d: 'output' | 'input' }> =>
      (d.d === 'output' || d.d === 'input') && d.name === root,
  );
  if (!decl?.loc) return null;
  let e: Expr | undefined = decl.d === 'output' ? decl.expr : decl.fallback;
  let best: Loc = decl.loc;
  for (const s of segs.slice(1)) {
    if (!e) break;
    let next: Expr | undefined;
    const k = segText(s);
    if (e.e === 'paren') e = e.x;
    if (e.e === 'obj' && typeof k === 'string') next = e.entries.find((en) => en.key === k)?.val;
    else if (e.e === 'arr' && typeof k === 'number') next = e.items[k]?.expr;
    else if (e.e === 'with') {
      e = e.base;
      continue;
    }
    if (!next) break;
    e = next;
    if (e.loc) best = e.loc;
  }
  return best;
}
const severityOf = (s: string) => (s === 'error' ? 1 : s === 'warning' ? 2 : 3);

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
    for (const e of errors)
      out.push({
        range: {
          start: { line: e.row, character: e.col },
          end: { line: e.row, character: e.col + 1 },
        },
        severity: 1,
        source: 'decl',
        code: 'E2001',
        message: 'syntax error',
      });
  } else {
    const a = analysisOf(uri)!;
    const r = a.run;
    for (const d of r.loadDiags) {
      // a loading problem is anchored to the import it concerns when one is named
      const imp = decls.find(
        (x) =>
          (x.d === 'import' || x.d === 're_export') &&
          x.loc &&
          d.message.includes(x.from.replace(/^\.\//, '').replace(/\.decl$/, '')),
      );
      push(imp?.loc ?? anchorFor(src, d.message), d);
    }
    for (const c of r.checks) {
      if (c.file !== path) continue;
      push(c.diag.loc ?? anchorFor(src, c.diag.message), c.diag);
    }
    for (const d of r.diags) {
      if (d.severity === 'information') continue;
      let segs: Seg[] | null;
      try {
        segs = d.path ? parsePath(d.path, '') : null;
      } catch {
        segs = null;
      }
      const loc = segs ? locOfPath(decls, segs) : null;
      if (!loc) continue; // a root declared elsewhere: its own module's business
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
    if (Array.isArray(x)) {
      for (const y of x) visit(y, parents);
      return;
    }
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
type Site = {
  kind: string;
  module: Module;
  decl?: Decl;
  member?: MemberAst;
  range: Loc;
  name: string;
  type?: Ty;
};

// the declaration a target denotes, as a site in its module
function siteOfTarget(a: Analysis, t: Target | null): Site | null {
  if (!t || !t.env) return null;
  const m = a.run.modules.find((x) => x.env === t.env);
  if (!m) return null;
  const text = textOf(a, m);
  const decl = m.decls.find((d) => (d as any).name === t.name && d.loc && d.d !== 'import');
  if (decl)
    return { kind: decl.d, module: m, decl, range: nameRange(text, decl, t.name), name: t.name };
  return null;
}
function memberSite(a: Analysis, m: Module, rt: any, member: string): Site | null {
  // the member's declaring type, extension chains followed (§4)
  const seen = new Set<string>();
  let typeName: string | undefined =
    rt?.t === 'rec' ? rt.name : rt?.t === 'pred' ? rt.base?.name : undefined;
  while (typeName && !seen.has(typeName)) {
    seen.add(typeName);
    const target = resolveIn(m.env, typeName);
    const site = siteOfTarget(a, target);
    const decl: any = site?.decl;
    if (!decl || decl.d !== 'type') return null;
    const body: TypeAst = decl.type;
    const members: MemberAst[] =
      body.k === 'record'
        ? body.members
        : body.k === 'named' && body.ext?.k === 'record'
          ? body.ext.members
          : [];
    const mem = members.find((x: any) => x.name === member);
    if (mem?.loc)
      return {
        kind: 'member',
        module: site!.module,
        decl,
        member: mem,
        range: memberRange(textOf(a, site!.module), mem, member),
        name: member,
      };
    typeName = body.k === 'named' ? body.name : undefined;
  }
  return null;
}

function siteAt(
  a: Analysis,
  uri: string,
  pos: Pos,
): { site: Site | null; type?: Ty; hit: Hit | null; module: Module } | null {
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
        return {
          site: ex ? siteOfTarget(a, resolveIn(ex.env, ex.name)) : null,
          type,
          hit,
          module: m,
        };
      }
      const xt = t.types.get(x);
      return { site: memberSite(a, m, xt?.rt, n.name), type, hit, module: m };
    }
    return { site: null, type, hit, module: m };
  }
  if (isType(n) && n.k === 'named') {
    const [head, tail] = n.name.split('.');
    let target: Target | null;
    if (tail && m.env.namespaces.has(head)) {
      const ex = m.env.namespaces.get(head)!.exports.get(tail);
      target = ex ? resolveIn(ex.env, ex.name) : null;
    } else target = resolveIn(m.env, head);
    return { site: siteOfTarget(a, target), hit, module: m };
  }
  if (isMember(n) && 'name' in n) {
    // the member's own declaration
    const decl = hit.parents.find(isDecl);
    return {
      site: decl
        ? {
            kind: 'member',
            module: m,
            decl,
            member: n,
            range: memberRange(textOf(a, m), n, n.name),
            name: n.name,
          }
        : null,
      hit,
      module: m,
    };
  }
  if (isDecl(n) && typeof n.name === 'string') {
    const r = nameRange(textOf(a, m), n, n.name);
    if (contains(r, pos))
      return { site: { kind: n.d, module: m, decl: n, range: r, name: n.name }, hit, module: m };
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
    while (from > 0 && /^\s*\/\/\//.test(lines[from - 1])) {
      from--;
      docLines.unshift(lines[from].trim());
    }
    const body =
      l.sl === l.el
        ? [lines[l.sl].slice(l.sc, l.ec)]
        : [lines[l.sl].slice(l.sc), ...lines.slice(l.sl + 1, l.el), lines[l.el].slice(0, l.ec)];
    return [...docLines, ...body.map((x) => x.trim()).filter(Boolean)];
  }
  const l = site.decl!.loc!;
  const docLines: string[] = [];
  let from = l.sl;
  while (from > 0 && /^\s*\/\/\//.test(lines[from - 1])) {
    from--;
    docLines.unshift(lines[from].trim());
  }
  const body = lines.slice(l.sl, l.el + 1);
  return [
    ...docLines,
    ...(body.length > 12 ? [...body.slice(0, 11), '    …', body[body.length - 1]] : body),
  ];
}
function hover(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  if (!a) return null;
  const s = siteAt(a, uri, pos);
  if (!s) return null;
  const parts: string[] = [];
  if (s.site) {
    const lines = declText(a, s.site);
    const doc = lines.filter((l) => l.startsWith('///')).map((l) => l.replace(/^\/\/\/\s?/, ''));
    const code = lines.filter((l) => !l.startsWith('///'));
    if (doc.length) parts.push(doc.join('\n'));
    parts.push('```decl\n' + code.join('\n') + '\n```');
  }
  if (s.type) parts.push(`\`${typeText(s.type.rt)}${s.type.abs ? '?' : ''}\``);
  if (!parts.length) return null;
  const range =
    s.site && s.hit && isExpr(s.hit.node) && s.hit.node.loc
      ? rangeOf(s.hit.node.loc)
      : s.hit?.node?.loc
        ? rangeOf(s.hit.node.loc)
        : undefined;
  return range
    ? { contents: { kind: 'markdown', value: parts.join('\n\n') }, range }
    : { contents: { kind: 'markdown', value: parts.join('\n\n') } };
}

// ---------------- navigation ----------------
const location = (m: Module, loc: Loc) => ({ uri: uriOf(m.path), range: rangeOf(loc) });
function definition(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  return s?.site ? location(s.site.module, s.site.range) : null;
}
// the named types a declared type mentions: `T`, `T[]`, `A | B`, `T?`,
// `ref<T>`, `map<K, V>` — the declarations "go to type definition" reaches
function namedTypesOf(ast: any, out: string[] = []): string[] {
  if (!ast) return out;
  switch (ast.k) {
    case 'named':
      out.push(ast.name);
      for (const arg of ast.args ?? []) namedTypesOf(arg, out);
      break;
    case 'array':
      namedTypesOf(ast.elem, out);
      break;
    case 'union':
    case 'isect':
      for (const arm of ast.arms) namedTypesOf(arm, out);
      break;
    case 'map':
      namedTypesOf(ast.val, out);
      break;
  }
  return out;
}
// the same over a resolved type: record names, through refs, arrays, maps, unions
function namedTypesOfRt(rt: any, out: string[] = []): string[] {
  if (!rt) return out;
  switch (rt.t) {
    case 'rec':
      if (rt.name && !rt.name.startsWith('{')) out.push(rt.name);
      break;
    case 'pred':
      namedTypesOfRt(rt.base, out);
      break;
    case 'ref':
      namedTypesOfRt(rt.target, out);
      break;
    case 'arr':
      namedTypesOfRt(rt.elem, out);
      break;
    case 'map':
      namedTypesOfRt(rt.val, out);
      break;
    case 'union':
      for (const arm of rt.arms) namedTypesOfRt(arm, out);
      break;
  }
  return out;
}
// a member's declared type, when its kind has one (an assert has none)
const typeOfMember = (m: MemberAst | null | undefined): TypeAst | undefined =>
  m && 'type' in m ? m.type : undefined;

function typeDefinition(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  if (!s) return null;
  // the declared type first (a member, an output or input, a constant's
  // annotation): the named types it spells, whatever they resolve to —
  // an alias of a literal union has a declaration too; else the
  // expression's inferred type
  let names: string[] = [];
  let env = s.module.env;
  if (s.site) {
    const d: any = s.site.decl;
    const ast =
      typeOfMember(s.site.member) ??
      (d && (d.d === 'output' || d.d === 'input' || d.d === 'const') ? d.type : undefined);
    if (ast) {
      names = namedTypesOf(ast);
      env = s.site.module.env;
    } else if (d?.d === 'const' && d.expr)
      names = namedTypesOfRt(tablesOf(a, s.site.module).types.get(d.expr)?.rt);
  }
  if (!names.length && s.hit && isExpr(s.hit.node) && s.hit.node.e === 'member') {
    // a member access: the member's declared type, where it is declared
    const xt = tablesOf(a, s.module).types.get(s.hit.node.x)?.rt;
    const ms = memberSite(a, s.module, xt, s.hit.node.name);
    const mt = typeOfMember(ms?.member);
    if (mt) {
      names = namedTypesOf(mt);
      env = ms!.module.env;
    }
  }
  if (!names.length) names = namedTypesOfRt(s.type?.rt);
  const seen = new Set<string>();
  const locs: any[] = [];
  for (const n of names) {
    const [head, tail] = n.split('.');
    let target: Target | null;
    if (tail && env.namespaces.has(head)) {
      const ex = env.namespaces.get(head)!.exports.get(tail);
      target = ex ? resolveIn(ex.env, ex.name) : null;
    } else target = resolveIn(env, head);
    const site = siteOfTarget(a, target);
    if (!site) continue;
    const key = `${site.module.path}:${site.range.sl}:${site.range.sc}`;
    if (seen.has(key)) continue;
    seen.add(key);
    locs.push(location(site.module, site.range));
  }
  return locs.length === 0 ? null : locs.length === 1 ? locs[0] : locs;
}
// every reference to a site across the universe: name and member nodes
// that resolve to the same declaration, plus the declaration itself
function references(
  uri: string,
  pos: Pos,
  includeDeclaration: boolean,
): { module: Module; loc: Loc }[] {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  if (!a || !s?.site) return [];
  const target = s.site;
  const out: { module: Module; loc: Loc }[] = [];
  const same = (x: Site | null) =>
    !!x &&
    x.module === target.module &&
    x.name === target.name &&
    x.kind === target.kind &&
    (x.kind !== 'member' || x.decl === target.decl);
  for (const m of a.run.modules) {
    const t = tablesOf(a, m);
    const visit = (x: any) => {
      if (!x || typeof x !== 'object') return;
      if (Array.isArray(x)) {
        x.forEach(visit);
        return;
      }
      if (isExpr(x) && x.loc) {
        if (x.e === 'name' && same(siteOfTarget(a, t.res.get(x) ?? resolveIn(m.env, x.name))))
          out.push({ module: m, loc: x.loc });
        if (x.e === 'member') {
          const xx = x.x;
          let site: Site | null;
          if (xx.e === 'name' && m.env.namespaces.has(xx.name)) {
            const ex = m.env.namespaces.get(xx.name)!.exports.get(x.name);
            site = ex ? siteOfTarget(a, resolveIn(ex.env, ex.name)) : null;
          } else site = memberSite(a, m, t.types.get(xx)?.rt, x.name);
          if (same(site)) out.push({ module: m, loc: memberTokenLoc(textOf(a, m), x) });
        }
      }
      if (isType(x) && x.k === 'named' && x.loc) {
        const [head, tail] = x.name.split('.');
        let tg: Target | null;
        if (tail && m.env.namespaces.has(head)) {
          const ex = m.env.namespaces.get(head)!.exports.get(tail);
          tg = ex ? resolveIn(ex.env, ex.name) : null;
        } else tg = resolveIn(m.env, head);
        if (same(siteOfTarget(a, tg)))
          out.push({ module: m, loc: typeNameLoc(x, tail ? head.length + 1 : 0, tail ?? head) });
      }
      // import items naming the declaration
      if (isDecl(x) && (x.d === 'import' || x.d === 're_export') && x.loc && x.names) {
        const tm = m.env.imports;
        for (const it of x.names) {
          const im = tm.get(it.as ?? it.name);
          if (im && same(siteOfTarget(a, resolveIn(im.env, im.name))))
            out.push({ module: m, loc: importItemLoc(textOf(a, m), x, it.name) });
        }
      }
      for (const [k, v] of Object.entries(x))
        if (k !== 'loc' && v && typeof v === 'object') visit(v);
    };
    visit(m.decls);
  }
  if (includeDeclaration) out.unshift({ module: target.module, loc: target.range });
  const key = (r: { module: Module; loc: Loc }) => `${r.module.path}:${r.loc.sl}:${r.loc.sc}`;
  const seen = new Set<string>();
  return out
    .filter((r) => !seen.has(key(r)) && seen.add(key(r)))
    .sort((p, q) =>
      p.module.path < q.module.path
        ? -1
        : p.module.path > q.module.path
          ? 1
          : p.loc.sl - q.loc.sl || p.loc.sc - q.loc.sc,
    );
}
const memberTokenLoc = (text: string, e: any): Loc => {
  const l: Loc = e.loc;
  const line = text.split('\n')[l.el] ?? '';
  const i = line.lastIndexOf(e.name, l.ec);
  return i >= 0 ? { sl: l.el, sc: i, el: l.el, ec: i + e.name.length } : l;
};
const typeNameLoc = (t: any, offset: number, name: string): Loc => ({
  sl: t.loc.sl,
  sc: t.loc.sc + offset,
  el: t.loc.sl,
  ec: t.loc.sc + offset + name.length,
});
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
  const items = session.complete(prefix, []).map((c) => {
    const [label, detail] = c.split('  ');
    const kind = detail
      ? detail.startsWith('derived') ||
        detail.startsWith('required') ||
        detail.startsWith('optional') ||
        detail.startsWith('defaulted')
        ? 5
        : 6
      : /^[A-Z]/.test(label)
        ? 7
        : label.startsWith('$')
          ? 14
          : 6;
    const item: any = { label, kind };
    if (detail) item.detail = detail;
    return item;
  });
  return { isIncomplete: false, items };
}

// ---------------- symbols, folding, formatting ----------------
const SYMBOL_KIND: Record<string, number> = {
  type: 5,
  const: 14,
  func: 12,
  output: 13,
  input: 13,
  diagnostic: 24,
  dimension: 13,
  unit: 13,
};
function documentSymbols(uri: string): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  const { decls, errors } = parseSource(text);
  if (errors.length) return [];
  const out: any[] = [];
  for (const d of decls as any[]) {
    if (!d.loc || typeof d.name !== 'string' || !(d.d in SYMBOL_KIND)) continue;
    const sym: any = {
      name: d.name,
      kind: SYMBOL_KIND[d.d],
      range: rangeOf(d.loc),
      selectionRange: rangeOf(nameRange(text, d, d.name)),
    };
    if (d.d === 'type') {
      const body =
        d.type.k === 'record'
          ? d.type
          : d.type.k === 'named' && d.type.ext?.k === 'record'
            ? d.type.ext
            : null;
      if (body) {
        const children = body.members
          .filter((m: any) => m.loc && typeof m.name === 'string')
          .map((m: any) => ({
            name: m.m === 'assert' ? `assert ${m.name}` : m.hidden ? `${m.name}$` : m.name,
            kind: m.m === 'assert' ? 24 : 7,
            range: rangeOf(m.loc),
            selectionRange: rangeOf(memberRange(text, m, m.name)),
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
    if (Array.isArray(x)) {
      x.forEach(visit);
      return;
    }
    if (
      x.loc &&
      x.loc.el > x.loc.sl &&
      (isDecl(x) ||
        (isType(x) && x.k === 'record') ||
        (isExpr(x) && (x.e === 'obj' || x.e === 'arr' || x.e === 'match')) ||
        (isMember(x) && x.m === 'when'))
    )
      out.push({ startLine: x.loc.sl, endLine: x.loc.el, kind: 'region' });
    for (const [k, v] of Object.entries(x)) if (k !== 'loc' && v && typeof v === 'object') visit(v);
  };
  visit(decls);
  const seen = new Set<string>();
  return out.filter(
    (r) => !seen.has(`${r.startLine}-${r.endLine}`) && seen.add(`${r.startLine}-${r.endLine}`),
  );
}
function formatting(uri: string): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  let out: string;
  try {
    out = format(text);
  } catch {
    return [];
  }
  if (out === text) return [];
  const lines = text.split('\n');
  return [
    {
      range: {
        start: { line: 0, character: 0 },
        end: { line: lines.length - 1, character: lines[lines.length - 1].length },
      },
      newText: out,
    },
  ];
}

// ---------------- rename ----------------
function prepareRename(uri: string, pos: Pos): any {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  if (!s?.site) {
    const locs = localRanges(uri, pos);
    const here = locs?.find((l) => contains(l, pos));
    return here
      ? {
          range: rangeOf(here),
          placeholder: (docs.get(uri) ?? '').split('\n')[here.sl].slice(here.sc, here.ec),
        }
      : null;
  }
  if (!s.hit?.node?.loc) return null;
  const n = s.hit.node;
  const loc: Loc =
    isExpr(n) && n.e === 'member'
      ? memberTokenLoc(textOf(a!, s.module), n)
      : isType(n)
        ? typeNameLoc(
            n,
            n.name.includes('.') ? n.name.indexOf('.') + 1 : 0,
            n.name.split('.').pop(),
          )
        : isDecl(n) || isMember(n)
          ? s.site.range
          : n.loc;
  return { range: rangeOf(loc), placeholder: s.site.name };
}
function rename(uri: string, pos: Pos, newName: string): any {
  const refs = references(uri, pos, true);
  if (!refs.length) {
    const locs = localRanges(uri, pos);
    return locs && locs.length
      ? { changes: { [uri]: locs.map((l) => ({ range: rangeOf(l), newText: newName })) } }
      : null;
  }
  const changes: Record<string, any[]> = {};
  for (const r of refs)
    (changes[uriOf(r.module.path)] ??= []).push({ range: rangeOf(r.loc), newText: newName });
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
    if (d.d === 'output')
      out.push({
        range: rangeOf({ sl: d.loc.sl, sc: d.loc.sc, el: d.loc.sl, ec: d.loc.sc }),
        command: { title: 'evaluate', command: 'decl.evaluate', arguments: [uri, d.name] },
      });
    if (d.d === 'input')
      out.push({
        range: rangeOf({ sl: d.loc.sl, sc: d.loc.sc, el: d.loc.sl, ec: d.loc.sc }),
        command: { title: 'validate', command: 'decl.validate', arguments: [uri, d.name] },
      });
  }
  return out;
}
function executeCommand(command: string, args: any[]): any {
  const [uri, root] = args ?? [];
  if (typeof uri !== 'string') return null;
  const session = new Session(pathOf(uri), overlay);
  for (const [name, file] of Object.entries(config.inputs)) {
    try {
      session.apply({
        op: 'bind',
        name,
        src: { kind: 'file', file, text: readText(absPath(dirname(pathOf(uri)), file)) },
      });
    } catch {
      /* reported by :validate */
    }
  }
  switch (command) {
    case 'decl.evaluate': {
      const { run, docs: ds } = session.evaluate(root ? [root] : []);
      const diags = [...run.loadDiags, ...run.checks.map((c) => c.diag), ...run.diags].map((d) =>
        fmtDiag(d),
      );
      if (root) return { root, document: ds[0]?.json ?? null, diagnostics: diags };
      const all =
        run.eng && ds.every((d) => d.json !== null)
          ? `{${ds.map((d) => `${JSON.stringify(d.name)}:${d.json}`).join(',')}}`
          : null;
      return { root: null, document: all, diagnostics: diags };
    }
    case 'decl.validate': {
      const { run, verdicts, diags } = session.validate(root ? [root] : []);
      return {
        verdicts,
        diagnostics: [...run.loadDiags, ...run.checks.map((c) => c.diag), ...diags].map((d) =>
          fmtDiag(d),
        ),
      };
    }
    case 'decl.trace':
      return root ? { lines: session.trace(root) } : null;
    case 'decl.showSyntaxTree':
      return syntaxTree(uri);
    case 'decl.reloadWorkspace':
      analyses.clear();
      for (const u of docs.keys()) analyze(u);
      return null;
    default:
      return null;
  }
}

// ---------------- signature help ----------------
const after = (l: Loc, p: Pos) => p.line > l.el || (p.line === l.el && p.character >= l.ec);
const srcOf = (text: string, l: Loc): string => {
  const lines = text.split('\n');
  return l.sl === l.el
    ? lines[l.sl].slice(l.sc, l.ec)
    : [lines[l.sl].slice(l.sc), ...lines.slice(l.sl + 1, l.el), lines[l.el].slice(0, l.ec)].join(
        '\n',
      );
};
function signatureHelp(uri: string, pos: Pos): any {
  const a = analysisOf(uri) ?? lastGood.get(uri);
  if (!a) return null;
  const m = moduleOf(a, pathOf(uri));
  if (!m) return null;
  const hit = nodeAt(m.decls, pos);
  if (!hit) return null;
  const calls = [...hit.parents, hit.node].filter((n) => isExpr(n) && n.e === 'call').reverse();
  for (const c of calls) {
    if (!c.fn.loc || !after(c.fn.loc, pos)) continue;
    let active = 0;
    c.args.forEach((arg: any, i: number) => {
      if (arg.loc && after(arg.loc, pos)) active = i + 1;
      else if (arg.loc && contains(arg.loc, pos)) active = i;
    });
    if (c.fn.e === 'name') {
      const target = resolveIn(m.env, c.fn.name);
      const site = siteOfTarget(a, target);
      const decl: any = site?.decl;
      if (!decl || decl.d !== 'func') return null;
      const text = textOf(a, site!.module);
      const params = decl.params.map(
        (p: any) => `${p.name}: ${p.type?.loc ? srcOf(text, p.type.loc) : '…'}`,
      );
      const ret = decl.ret?.loc ? `: ${srcOf(text, decl.ret.loc)}` : '';
      return {
        signatures: [
          {
            label: `${decl.name}(${params.join(', ')})${ret}`,
            parameters: params.map((p: string) => ({ label: p })),
          },
        ],
        activeSignature: 0,
        activeParameter: Math.min(active, Math.max(0, params.length - 1)),
      };
    }
    const sp = stdPath(c.fn);
    if (sp !== null && STD[sp]) {
      const params = Array.from({ length: STD[sp].arity }, (_, i) => `a${i + 1}`);
      return {
        signatures: [
          {
            label: `std.${sp}(${params.join(', ')})`,
            parameters: params.map((p) => ({ label: p })),
          },
        ],
        activeSignature: 0,
        activeParameter: Math.min(active, Math.max(0, params.length - 1)),
      };
    }
    return null;
  }
  return null;
}

// ---------------- workspace symbols, selection ranges ----------------
function workspaceSymbols(query: string): any[] {
  const out: any[] = [];
  const seen = new Set<string>();
  const q = query.toLowerCase();
  for (const a of lastGood.values())
    for (const m of a.run.modules) {
      if (seen.has(m.path)) continue;
      seen.add(m.path);
      const text = textOf(a, m);
      for (const d of m.decls as any[])
        if (
          d.loc &&
          typeof d.name === 'string' &&
          d.d in SYMBOL_KIND &&
          d.name.toLowerCase().includes(q)
        )
          out.push({
            name: d.name,
            kind: SYMBOL_KIND[d.d],
            location: location(m, nameRange(text, d, d.name)),
          });
    }
  return out.sort((x, y) =>
    x.name < y.name
      ? -1
      : x.name > y.name
        ? 1
        : x.location.uri < y.location.uri
          ? -1
          : x.location.uri > y.location.uri
            ? 1
            : 0,
  );
}
function selectionRanges(uri: string, positions: Pos[]): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  const { decls, errors } = parseSource(text);
  if (errors.length) return positions.map((p) => ({ range: { start: p, end: p } }));
  return positions.map((p) => {
    const hit = nodeAt(decls, p);
    const chain = hit ? [hit.node, ...[...hit.parents].reverse()].filter((n) => n.loc) : [];
    let out: any = { range: { start: p, end: p } };
    for (const n of chain) out = { range: rangeOf(n.loc), parent: out };
    // the innermost node's range first: rebuild from the outside in
    const ranges = chain.map((n) => rangeOf(n.loc));
    let sel: any = undefined;
    for (const r of ranges.reverse()) sel = sel ? { range: r, parent: sel } : { range: r };
    return sel ?? { range: { start: p, end: p } };
  });
}

// ---------------- semantic tokens ----------------
const TOKEN_TYPES = ['type', 'property', 'function', 'variable', 'namespace', 'parameter'];
const TOKEN_MODS = [
  'declaration',
  'required',
  'optional',
  'defaulted',
  'derived',
  'hidden',
  'unresolved',
  'readonly',
];
const T = Object.fromEntries(TOKEN_TYPES.map((t, i) => [t, i])) as Record<string, number>;
const M = Object.fromEntries(TOKEN_MODS.map((t, i) => [t, 1 << i])) as Record<string, number>;
const memberMods = (kind: string, hidden?: boolean) =>
  (kind === 'der'
    ? M.derived
    : kind === 'dflt'
      ? M.defaulted
      : kind === 'opt'
        ? M.optional
        : M.required) | (hidden ? M.hidden : 0);
function semanticTokens(uri: string): any {
  const a = analysisOf(uri);
  if (!a) return { data: [] };
  const m = moduleOf(a, pathOf(uri));
  if (!m) return { data: [] };
  const text = textOf(a, m);
  const t = tablesOf(a, m);
  const toks: { l: Loc; type: number; mods: number }[] = [];
  const push = (l: Loc, type: number, mods = 0) => {
    if (l.sl === l.el && l.ec > l.sc) toks.push({ l, type, mods });
  };
  const memberKind = (rt: any, name: string): { kind: string; hidden?: boolean } | null => {
    const r = rt?.t === 'pred' ? rt.base : rt?.t === 'ref' ? rt.target : rt;
    const mem = r?.t === 'rec' ? r.members.find((x: any) => x.name === name) : null;
    return mem ? { kind: mem.kind, hidden: mem.hidden } : null;
  };
  const visit = (x: any, inFunc: Set<string>) => {
    if (!x || typeof x !== 'object') return;
    if (Array.isArray(x)) {
      x.forEach((y) => visit(y, inFunc));
      return;
    }
    if (isDecl(x) && x.loc && typeof x.name === 'string') {
      const r = nameRange(text, x, x.name);
      const type =
        x.d === 'type' || x.d === 'dimension' || x.d === 'unit'
          ? T.type
          : x.d === 'func' || x.d === 'diagnostic'
            ? T.function
            : T.variable;
      push(r, type, M.declaration | (x.d === 'const' ? M.readonly : 0));
      if (x.d === 'func') {
        inFunc = new Set(x.params.map((p: any) => p.name));
        for (const p of x.params) {
          const pl = paramLoc(text, x, p.name);
          if (pl) push(pl, T.parameter, M.declaration);
        }
      }
    }
    if (
      isMember(x) &&
      x.loc &&
      typeof x.name === 'string' &&
      (x.m === 'value' || x.m === 'derived')
    ) {
      push(
        memberRange(text, x, x.name),
        T.property,
        M.declaration |
          memberMods(x.m === 'derived' ? 'der' : x.dflt ? 'dflt' : x.opt ? 'opt' : 'req', x.hidden),
      );
    }
    if (isType(x) && x.k === 'named' && x.loc) {
      const [head, tail] = x.name.split('.');
      if (tail) {
        push(typeNameLoc(x, 0, head), T.namespace);
        push(typeNameLoc(x, head.length + 1, tail), T.type);
      } else if (!['map', 'ref', 'quantity'].includes(head))
        push(typeNameLoc(x, 0, head), T.type, resolveIn(m.env, head) ? 0 : M.unresolved);
    }
    if (isExpr(x) && x.loc) {
      if (x.e === 'name') {
        const target = t.res.get(x) ?? resolveIn(m.env, x.name);
        if (x.name === 'std') push(x.loc, T.namespace);
        else if (inFunc.has(x.name) || target?.kind === 'var') push(x.loc, T.parameter);
        else if (!target) push(x.loc, T.variable, M.unresolved);
        else
          push(
            x.loc,
            target.kind === 'func'
              ? T.function
              : target.kind === 'namespace'
                ? T.namespace
                : target.kind === 'type'
                  ? T.type
                  : T.variable,
            target.kind === 'const' ? M.readonly : 0,
          );
      } else if (x.e === 'member') {
        const xx = x.x;
        const ml = memberTokenLoc(text, x);
        if (stdPath(x) !== null)
          push(ml, x.e === 'member' && STD[stdPath(x)!] ? T.function : T.namespace);
        else if (xx.e === 'name' && m.env.namespaces.has(xx.name)) {
          const ex = m.env.namespaces.get(xx.name)!.exports.get(x.name);
          const tg = ex ? resolveIn(ex.env, ex.name) : null;
          push(
            ml,
            tg?.kind === 'func' ? T.function : tg?.kind === 'type' ? T.type : T.variable,
            tg ? 0 : M.unresolved,
          );
        } else {
          const mk = memberKind(t.types.get(xx)?.rt, x.name);
          push(ml, T.property, mk ? memberMods(mk.kind, mk.hidden) : 0);
        }
      } else if (x.e === 'lambda') inFunc = new Set([...inFunc, ...x.params]);
      else if (x.e === 'comp' || x.e === 'mapcomp')
        inFunc = new Set([...inFunc, ...x.clauses.map((c: any) => c.v)]);
    }
    for (const [k, v] of Object.entries(x))
      if (k !== 'loc' && v && typeof v === 'object') visit(v, inFunc);
  };
  visit(m.decls, new Set());
  toks.sort((p, q) => p.l.sl - q.l.sl || p.l.sc - q.l.sc);
  const data: number[] = [];
  let pl = 0,
    pc = 0;
  for (const tk of toks) {
    const dl = tk.l.sl - pl,
      dc = dl === 0 ? tk.l.sc - pc : tk.l.sc;
    if (dl === 0 && dc < 0) continue; // overlapping tokens: the first wins
    data.push(dl, dc, tk.l.ec - tk.l.sc, tk.type, tk.mods);
    pl = tk.l.sl;
    pc = tk.l.sc;
  }
  return { data };
}
const paramLoc = (text: string, decl: any, name: string): Loc | null => {
  const l: Loc = decl.loc;
  const line = text.split('\n')[l.sl] ?? '';
  const open = line.indexOf('(', l.sc);
  if (open < 0) return null;
  const re = new RegExp(`\\b${name}\\b`, 'g');
  re.lastIndex = open;
  const mm = re.exec(line);
  return mm ? { sl: l.sl, sc: mm.index, el: l.sl, ec: mm.index + name.length } : null;
};

// ---------------- inlay hints ----------------
const hints = {
  types: true,
  parameterNames: true,
  values: false,
  units: true,
  contextVariables: false,
};
function inlayHints(uri: string, range: Range): any[] {
  const a = analysisOf(uri);
  if (!a) return [];
  const m = moduleOf(a, pathOf(uri));
  if (!m) return [];
  const text = textOf(a, m);
  const t = tablesOf(a, m);
  const out: any[] = [];
  const inRange = (p: Pos) => p.line >= range.start.line && p.line <= range.end.line;
  const visit = (x: any) => {
    if (!x || typeof x !== 'object') return;
    if (Array.isArray(x)) {
      x.forEach(visit);
      return;
    }
    if (hints.types && isMember(x) && x.m === 'derived' && !x.type && x.loc) {
      const ty = t.types.get(x.expr);
      const r = memberRange(text, x, x.name);
      const p = { line: r.el, character: r.ec + (x.hidden ? 1 : 0) };
      if (ty?.rt && inRange(p)) out.push({ position: p, label: `: ${typeText(ty.rt)}`, kind: 1 });
    }
    if (hints.types && isDecl(x) && x.d === 'const' && !x.type && x.loc) {
      const ty = t.types.get(x.expr);
      const r = nameRange(text, x, x.name);
      const p = { line: r.el, character: r.ec };
      if (ty?.rt && inRange(p)) out.push({ position: p, label: `: ${typeText(ty.rt)}`, kind: 1 });
    }
    if (hints.parameterNames && isExpr(x) && x.e === 'call' && x.fn.e === 'name') {
      const site = siteOfTarget(a, t.res.get(x.fn) ?? resolveIn(m.env, x.fn.name));
      const decl: any = site?.decl;
      if (decl?.d === 'func')
        x.args.forEach((arg: any, i: number) => {
          const p = decl.params[i];
          if (p && arg.loc && inRange({ line: arg.loc.sl, character: arg.loc.sc }))
            out.push({
              position: { line: arg.loc.sl, character: arg.loc.sc },
              label: `${p.name}:`,
              kind: 2,
              paddingRight: true,
            });
        });
    }
    if (hints.values && isDecl(x) && x.d === 'output' && x.loc && a.run.eng && a.run.entry) {
      // the evaluated derived members of a literal, at the end of the literal
      const eng = a.run.eng;
      const walk = (v: any, e: any) => {
        if (!v || !e) return;
        if (isRec(v) && e.e === 'obj') {
          const parts: string[] = [];
          for (const mem of v.rt.members) {
            const s = v.slots.get(mem.name);
            if (!s || mem.kind !== 'der' || s.hidden || s.state !== 'ok') continue;
            const txt = eng.serialize(s.value, x.name) ?? '';
            parts.push(`${mem.name} = ${txt.length > 40 ? txt.slice(0, 37) + '…' : txt}`);
          }
          const p = { line: e.loc.el, character: e.loc.ec };
          if (parts.length && inRange(p))
            out.push({ position: p, label: `// ${parts.join(', ')}`, paddingLeft: true });
          for (const en of e.entries) {
            const s = v.slots.get(en.key);
            if (s?.state === 'ok') walk(s.value, en.val);
          }
        } else if (isArr(v) && e.e === 'arr')
          v.items.forEach((it: any, i: number) => walk(it, e.items[i]?.expr));
        else if (isMap(v) && e.e === 'obj')
          for (const en of e.entries) walk(v.entries.get(en.key), en.val);
      };
      walk(a.run.entry.env.roots.get(x.name), x.expr);
    }
    if (
      hints.contextVariables &&
      isExpr(x) &&
      x.e === 'ctx' &&
      x.loc &&
      (x.name === '$parent' || x.name === '$root' || x.name === '$key')
    ) {
      // the bound the enclosing type declares for the variable
      const decl: any = m.decls.find(
        (d) => d.d === 'type' && d.loc && d.loc.sl <= x.loc.sl && x.loc.el <= d.loc.el,
      );
      const body = decl ? recordBodyOf(decl) : null;
      const ctxm = body?.members.find((mm: any) => mm.m === 'context' && mm.variable === x.name);
      const p = { line: x.loc.el, character: x.loc.ec };
      if (ctxm?.type?.loc && inRange(p))
        out.push({ position: p, label: `: ${srcOf(text, ctxm.type.loc)}`, kind: 1 });
    }
    if (hints.units && isExpr(x) && x.e === 'unitlit' && x.loc) {
      try {
        const u = m.env.unitInfo(x.unit);
        const base = m.env.baseUnitOf.get(u.key) ?? u.key;
        const p = { line: x.loc.el, character: x.loc.ec };
        if (base !== x.unit && inRange(p))
          out.push({ position: p, label: `= ${x.num * u.toBase} ${base}`, paddingLeft: true });
      } catch {
        /* an unknown unit is a diagnostic */
      }
    }
    for (const [k, v] of Object.entries(x)) if (k !== 'loc' && v && typeof v === 'object') visit(v);
  };
  visit(m.decls);
  return out.sort(
    (p, q) => p.position.line - q.position.line || p.position.character - q.position.character,
  );
}

// the record body of a type declaration: its own, or its extension's
const recordBodyOf = (decl: any): any =>
  decl?.d === 'type'
    ? decl.type.k === 'record'
      ? decl.type
      : decl.type.k === 'named' && decl.type.ext?.k === 'record'
        ? decl.type.ext
        : null
    : null;

// ---------------- hierarchies ----------------
const hierarchyItem = (m: Module, decl: any, text: string) => ({
  name: decl.name,
  kind: SYMBOL_KIND[decl.d] ?? 13,
  uri: uriOf(m.path),
  range: rangeOf(decl.loc),
  selectionRange: rangeOf(nameRange(text, decl, decl.name)),
});
function prepareHierarchy(uri: string, pos: Pos, want: 'func' | 'type'): any[] | null {
  const a = analysisOf(uri);
  const s = a && siteAt(a, uri, pos);
  if (!s?.site?.decl || s.site.kind !== want) return null;
  return [hierarchyItem(s.site.module, s.site.decl, textOf(a!, s.site.module))];
}
function moduleOfUri(uri: string): { a: Analysis; m: Module } | null {
  for (const a of lastGood.values()) {
    const m = a.run.modules.find((x) => uriOf(x.path) === uri);
    if (m) return { a, m };
  }
  return null;
}
function declContaining(m: Module, loc: Loc): any | null {
  return (
    m.decls.find(
      (d) =>
        d.loc && d.loc.sl <= loc.sl && loc.el <= d.loc.el && typeof (d as any).name === 'string',
    ) ?? null
  );
}
function incomingCalls(item: any): any[] {
  const found = moduleOfUri(item.uri);
  if (!found) return [];
  const { a } = found;
  const out: any[] = [];
  for (const m of a.run.modules) {
    const t = tablesOf(a, m);
    const text = textOf(a, m);
    const byCaller = new Map<any, Loc[]>();
    const visit = (x: any) => {
      if (!x || typeof x !== 'object') return;
      if (Array.isArray(x)) {
        x.forEach(visit);
        return;
      }
      if (isExpr(x) && x.e === 'call' && x.fn.e === 'name' && x.fn.loc) {
        const site = siteOfTarget(a, t.res.get(x.fn) ?? resolveIn(m.env, x.fn.name));
        if (
          site?.decl &&
          uriOf(site.module.path) === item.uri &&
          site.decl.loc!.sl === item.range.start.line
        ) {
          const caller = declContaining(m, x.fn.loc);
          if (caller) byCaller.set(caller, [...(byCaller.get(caller) ?? []), x.fn.loc]);
        }
      }
      for (const [k, v] of Object.entries(x))
        if (k !== 'loc' && v && typeof v === 'object') visit(v);
    };
    visit(m.decls);
    for (const [caller, locs] of byCaller)
      out.push({ from: hierarchyItem(m, caller, text), fromRanges: locs.map(rangeOf) });
  }
  return out;
}
function outgoingCalls(item: any): any[] {
  const found = moduleOfUri(item.uri);
  if (!found) return [];
  const { a, m } = found;
  const t = tablesOf(a, m);
  const decl: any = m.decls.find((d) => d.loc && d.loc.sl === item.range.start.line);
  if (!decl) return [];
  const byCallee = new Map<string, { to: any; locs: Loc[] }>();
  const visit = (x: any) => {
    if (!x || typeof x !== 'object') return;
    if (Array.isArray(x)) {
      x.forEach(visit);
      return;
    }
    if (isExpr(x) && x.e === 'call' && x.fn.e === 'name' && x.fn.loc) {
      const site = siteOfTarget(a, t.res.get(x.fn) ?? resolveIn(m.env, x.fn.name));
      if (site?.decl?.d === 'func') {
        const key = `${site.module.path}:${site.decl.loc!.sl}`;
        const e = byCallee.get(key) ?? {
          to: hierarchyItem(site.module, site.decl, textOf(a, site.module)),
          locs: [],
        };
        e.locs.push(x.fn.loc);
        byCallee.set(key, e);
      }
    }
    for (const [k, v] of Object.entries(x)) if (k !== 'loc' && v && typeof v === 'object') visit(v);
  };
  visit(decl);
  return [...byCallee.values()].map((e) => ({ to: e.to, fromRanges: e.locs.map(rangeOf) }));
}
function supertypes(item: any): any[] {
  const found = moduleOfUri(item.uri);
  if (!found) return [];
  const { a, m } = found;
  const decl: any = m.decls.find(
    (d) => d.d === 'type' && d.loc && d.loc.sl === item.range.start.line,
  );
  const base = decl?.type?.k === 'named' && decl.type.ext ? decl.type.name : null;
  if (!base) return [];
  const site = siteOfTarget(a, resolveIn(m.env, base));
  return site?.decl ? [hierarchyItem(site.module, site.decl, textOf(a, site.module))] : [];
}
function subtypes(item: any): any[] {
  const found = moduleOfUri(item.uri);
  if (!found) return [];
  const { a } = found;
  const out: any[] = [];
  for (const m of a.run.modules)
    for (const d of m.decls as any[]) {
      if (d.d !== 'type' || !d.loc || d.type?.k !== 'named' || !d.type.ext) continue;
      const site = siteOfTarget(a, resolveIn(m.env, d.type.name));
      if (
        site?.decl &&
        uriOf(site.module.path) === item.uri &&
        site.decl.loc!.sl === item.range.start.line
      )
        out.push(hierarchyItem(m, d, textOf(a, m)));
    }
  return out;
}

// ---------------- code actions ----------------
const placeholderFor = (rt: any): string => {
  const r = rt?.t === 'pred' ? rt.base : rt;
  if (!r) return 'null';
  if (r.t === 'prim')
    return r.name === 'string'
      ? '""'
      : r.name === 'int'
        ? '0'
        : r.name === 'float'
          ? '0.0'
          : r.name === 'bool'
            ? 'false'
            : 'null';
  if (r.t === 'lit') return typeof r.v === 'string' ? JSON.stringify(r.v) : String(r.v);
  if (r.t === 'range') return String(r.lo);
  if (r.t === 'rec') return '{ }';
  if (r.t === 'arr') return '[]';
  if (r.t === 'map') return '{}';
  if (r.t === 'union') return placeholderFor(r.arms[0]);
  return 'null';
};
function codeActions(uri: string, range: Range, diagnostics: any[]): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  const a = analysisOf(uri);
  const out: any[] = [];
  const { decls, errors } = parseSource(text);
  const lines = text.split('\n');
  if (a && !errors.length) {
    const m = moduleOf(a, pathOf(uri))!;
    const t = tablesOf(a, m);
    // the fixes of the diagnostics that touch the range (a client may send more)
    const touches = (d: any) =>
      d.range && !(d.range.end.line < range.start.line || d.range.start.line > range.end.line);
    for (const d of (diagnostics ?? []).filter(touches)) {
      let mm = /^unknown name ([A-Za-z_][A-Za-z0-9_]*)/.exec(d.message ?? '');
      if (mm) {
        const name = mm[1];
        for (const other of exportersOf(a, m, name)) {
          let spec = './' + require_rel(m.path, other.path);
          const existing: any = decls.find(
            (x) => x.d === 'import' && x.names && absPath(dirname(m.path), x.from) === other.path,
          );
          let edit: any;
          if (existing) {
            const line = lines[existing.loc.sl];
            const close = line.indexOf('}', existing.loc.sc);
            edit = {
              range: {
                start: { line: existing.loc.sl, character: close },
                end: { line: existing.loc.sl, character: close },
              },
              newText: `, ${name} `,
            };
            spec = existing.from;
          } else {
            const lastImport = [...decls]
              .reverse()
              .find((x) => x.d === 'import' || x.d === 're_export');
            const at = lastImport?.loc ? lastImport.loc.el + 1 : 0;
            edit = {
              range: { start: { line: at, character: 0 }, end: { line: at, character: 0 } },
              newText: `import { ${name} } from "${spec}"\n`,
            };
          }
          out.push({
            title: `import ${name} from "${spec}"`,
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: { changes: { [uri]: [edit] } },
          });
        }
      }
      mm = /^unknown name ([A-Za-z_][A-Za-z0-9_]*)/.exec(d.message ?? '');
      if (mm) {
        // a namespace import that exports it: qualify the name
        const name = mm[1];
        for (const [ns, entry] of m.env.namespaces) {
          if (!entry.exports.has(name)) continue;
          const hit = nodeAt(decls, d.range.start);
          const n =
            hit &&
            [hit.node, ...[...hit.parents].reverse()].find(
              (x) => isExpr(x) && x.e === 'name' && x.name === name,
            );
          if (n?.loc)
            out.push({
              title: `qualify as ${ns}.${name}`,
              kind: 'quickfix',
              diagnostics: [d],
              edit: { changes: { [uri]: [{ range: rangeOf(n.loc), newText: `${ns}.${name}` }] } },
            });
        }
      }
      mm = /^member ([A-Za-z_][A-Za-z0-9_]*) is not declared on ([A-Za-z_][A-Za-z0-9_]*)$/.exec(
        d.message ?? '',
      );
      if (mm) {
        // declare the member on the type, with the supplied value's inferred type
        const [, name, typeName] = mm;
        const site = siteOfTarget(a, resolveIn(m.env, typeName));
        const decl: any = site?.decl;
        const body =
          decl?.d === 'type'
            ? decl.type.k === 'record'
              ? decl.type
              : decl.type.k === 'named' && decl.type.ext?.k === 'record'
                ? decl.type.ext
                : null
            : null;
        if (site && body) {
          const hit = nodeAt(decls, d.range.start);
          const chain = hit ? [hit.node, ...[...hit.parents].reverse()] : [];
          let entry: any = chain
            .find((n) => isExpr(n) && n.e === 'obj')
            ?.entries.find((en: any) => en.key === name);
          if (!entry && hit) {
            const objs: any[] = [];
            findAll(hit.node, (n) => isExpr(n) && n.e === 'obj', objs);
            entry = objs.flatMap((o) => o.entries).find((en: any) => en.key === name);
          }
          const ty = entry ? t.types.get(entry.val) : undefined;
          // a literal's type widened to its primitive: a declaration wants `bool`, not `true`
          const widen = (rt: any): any =>
            rt?.t === 'lit'
              ? {
                  t: 'prim',
                  name:
                    typeof rt.v === 'string'
                      ? 'string'
                      : typeof rt.v === 'boolean'
                        ? 'bool'
                        : typeof rt.v === 'bigint'
                          ? 'int'
                          : typeof rt.v === 'number'
                            ? 'float'
                            : 'null',
                }
              : rt;
          const memberType = ty?.rt ? typeText(widen(ty.rt)) : 'any';
          const last = body.members[body.members.length - 1];
          const at = last?.loc
            ? { line: last.loc.el, character: last.loc.ec }
            : { line: body.loc.sl, character: body.loc.sc + 1 };
          out.push({
            title: `declare ${name}: ${memberType} on ${typeName}`,
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: {
              changes: {
                [uriOf(site.module.path)]: [
                  {
                    range: { start: at, end: at },
                    newText: last?.loc ? `\n    ${name}: ${memberType}` : ` ${name}: ${memberType}`,
                  },
                ],
              },
            },
          });
        }
      }
      if (/^member access on a maybe-absent expression/.test(d.message ?? '')) {
        const hit = nodeAt(decls, d.range.start);
        const n =
          hit &&
          [hit.node, ...[...hit.parents].reverse()].find(
            (x) => isExpr(x) && x.e === 'member' && !x.safe,
          );
        if (n?.loc) {
          const tok = memberTokenLoc(text, n);
          out.push({
            title: 'use ?.',
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: {
              changes: {
                [uri]: [
                  {
                    range: {
                      start: { line: tok.sl, character: tok.sc - 1 },
                      end: { line: tok.sl, character: tok.sc },
                    },
                    newText: '?.',
                  },
                ],
              },
            },
          });
        }
      }
      if (/^maybe-absent expression consumed/.test(d.message ?? '')) {
        const hit = nodeAt(decls, d.range.start);
        const n = hit?.node;
        if (n && isExpr(n) && n.loc) {
          const end = { line: n.loc.el, character: n.loc.ec };
          out.push({
            title: 'supply a fallback with ??',
            kind: 'quickfix',
            diagnostics: [d],
            edit: { changes: { [uri]: [{ range: { start: end, end }, newText: ' ?? null' }] } },
          });
        }
      }
      if (/^`\?\?` mixed with/.test(d.message ?? '')) {
        // parenthesize the `??` operand of the mixed expression
        const hit = nodeAt(decls, d.range.start);
        let target: any = null;
        const logical = (op: string) => op === '&&' || op === '||';
        const find = (x: any, parentOp: string | null) => {
          if (!x || typeof x !== 'object' || target) return;
          if (Array.isArray(x)) {
            x.forEach((y) => find(y, parentOp));
            return;
          }
          if (
            isExpr(x) &&
            x.e === 'bin' &&
            x.loc &&
            parentOp &&
            ((x.op === '??' && logical(parentOp)) || (logical(x.op) && parentOp === '??'))
          ) {
            target = x;
            return;
          }
          const op = isExpr(x) && x.e === 'bin' ? x.op : null;
          for (const [k, v] of Object.entries(x))
            if (k !== 'loc' && v && typeof v === 'object') find(v, op);
        };
        if (hit) find(hit.node, null);
        if (target)
          out.push({
            title: 'parenthesize the ?? expression',
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: {
              changes: {
                [uri]: [
                  {
                    range: {
                      start: { line: target.loc.sl, character: target.loc.sc },
                      end: { line: target.loc.sl, character: target.loc.sc },
                    },
                    newText: '(',
                  },
                  {
                    range: {
                      start: { line: target.loc.el, character: target.loc.ec },
                      end: { line: target.loc.el, character: target.loc.ec },
                    },
                    newText: ')',
                  },
                ],
              },
            },
          });
      }
      if (/^`match` is not exhaustive/.test(d.message ?? '')) {
        const hit = nodeAt(decls, d.range.start);
        const n =
          hit &&
          [hit.node, ...[...hit.parents].reverse()].find((x) => isExpr(x) && x.e === 'match');
        const subject = n ? t.types.get(n.subject)?.rt : null;
        const arms: string[] =
          subject?.t === 'union'
            ? subject.arms.filter((r: any) => r.t === 'rec' && r.name).map((r: any) => r.name)
            : [];
        const covered = new Set(
          n ? n.arms.map((arm: any) => (arm.type?.k === 'named' ? arm.type.name : '')) : [],
        );
        const missing = arms.filter((x) => !covered.has(x));
        if (n?.loc && missing.length) {
          const at = { line: n.loc.el, character: n.loc.ec - 1 };
          const indent = ' '.repeat((text.split('\n')[n.loc.sl].match(/^ */)?.[0].length ?? 0) + 4);
          out.push({
            title: `add the missing arm${missing.length > 1 ? 's' : ''}: ${missing.join(', ')}`,
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: {
              changes: {
                [uri]: [
                  {
                    range: { start: at, end: at },
                    newText:
                      missing.map((x) => `${indent}(v: ${x}) => null\n`).join('') + indent.slice(4),
                  },
                ],
              },
            },
          });
        }
      }
      mm = /^(\$[a-z]+) used without a context declaration in ([A-Za-z_][A-Za-z0-9_]*)$/.exec(
        d.message ?? '',
      );
      if (mm) {
        // declare the context variable on the type: `$parent: ref<{ ... }>`, `$root: ref<{ ... }>`, `$key: string`
        const [, variable, typeName] = mm;
        const site = siteOfTarget(a, resolveIn(m.env, typeName));
        const body = site?.decl ? recordBodyOf(site.decl) : null;
        if (site && body?.loc) {
          const bound = variable === '$key' ? 'string' : 'ref<{ ... }>';
          const first = body.members[0];
          const at = first?.loc
            ? { line: first.loc.sl, character: first.loc.sc }
            : { line: body.loc.sl, character: body.loc.sc + 1 };
          const indent = first?.loc ? '' : ' ';
          out.push({
            title: `declare ${variable}: ${bound} on ${typeName}`,
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: {
              changes: {
                [uriOf(site.module.path)]: [
                  {
                    range: { start: at, end: at },
                    newText:
                      first?.loc && first.loc.sl > body.loc.sl
                        ? `${variable}: ${bound}\n${' '.repeat(first.loc.sc)}`
                        : `${indent}${variable}: ${bound}, `,
                  },
                ],
              },
            },
          });
        }
      }
      mm = /^(\$[a-z]+) declaration must be ref<\.\.\.> \(([A-Za-z_][A-Za-z0-9_]*)\)$/.exec(
        d.message ?? '',
      );
      if (mm) {
        // the declared bound wrapped in ref<…>
        const [, variable, typeName] = mm;
        const site = siteOfTarget(a, resolveIn(m.env, typeName));
        const body = site?.decl ? recordBodyOf(site.decl) : null;
        const ctxm = body?.members.find((x: any) => x.m === 'context' && x.variable === variable);
        if (site && ctxm?.type?.loc) {
          const tl = ctxm.type.loc;
          out.push({
            title: `declare ${variable} as ref<${srcOf(textOf(a, site.module), tl)}>`,
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: {
              changes: {
                [uriOf(site.module.path)]: [
                  { range: rangeOf(tl), newText: `ref<${srcOf(textOf(a, site.module), tl)}>` },
                ],
              },
            },
          });
        }
      }
      mm =
        /^(?:illegal member-kind transition for|override widens inherited member) ([A-Za-z_][A-Za-z0-9_]*)[^(]*\(([A-Za-z_][A-Za-z0-9_]*)\)$/.exec(
          d.message ?? '',
        );
      if (mm) {
        // the override replaced by the parent's declaration of the member
        const [, member, typeName] = mm;
        const site = siteOfTarget(a, resolveIn(m.env, typeName));
        const decl: any = site?.decl;
        const body = recordBodyOf(decl);
        const own = body?.members.find((x: any) => x.name === member && x.loc);
        const parent =
          decl?.type?.k === 'named'
            ? memberSite(a, site!.module, safeResolve(site!.module, decl.type.name), member)
            : null;
        if (site && own && parent?.member?.loc) {
          const parentText = srcOf(textOf(a, parent.module), parent.member.loc).replace(
            /,\s*$/,
            '',
          );
          out.push({
            title: `use the parent's declaration: ${parentText}`,
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: {
              changes: {
                [uriOf(site.module.path)]: [{ range: rangeOf(own.loc), newText: parentText }],
              },
            },
          });
        }
      }
      mm = /^record union arms not discriminable in ([A-Za-z_][A-Za-z0-9_]*)$/.exec(
        d.message ?? '',
      );
      if (mm) {
        // a literal-typed `kind` member on every arm that is a local record type
        const site = siteOfTarget(a, resolveIn(m.env, mm[1]));
        const u: any = site?.decl;
        const arms: any[] = u?.type?.k === 'union' ? u.type.arms : [];
        const changes: Record<string, any[]> = {};
        let n = 0;
        for (const arm of arms) {
          if (arm.k !== 'named') continue;
          const as = siteOfTarget(a, resolveIn(site!.module.env, arm.name));
          const body = as?.decl ? recordBodyOf(as.decl) : null;
          const first = body?.members[0];
          if (!as || !body?.loc || body.members.some((x: any) => x.name === 'kind')) continue;
          const at = first?.loc
            ? { line: first.loc.sl, character: first.loc.sc }
            : { line: body.loc.sl, character: body.loc.sc + 1 };
          const multi = first?.loc && first.loc.sl > body.loc.sl;
          (changes[uriOf(as.module.path)] ??= []).push({
            range: { start: at, end: at },
            newText: multi
              ? `kind: "${arm.name}"\n${' '.repeat(first.loc.sc)}`
              : `${first?.loc ? '' : ' '}kind: "${arm.name}", `,
          });
          n++;
        }
        if (n)
          out.push({
            title: `add a discriminant \`kind\` to the arms of ${mm[1]}`,
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: { changes },
          });
      }
      mm = /^derived member ([A-Za-z_][A-Za-z0-9_]*) restated with a differing value/.exec(
        d.message ?? '',
      );
      if (mm) {
        // a document supplies it: make it defaulted (`x?: T = e`) where it is declared with a type
        const member = mm[1];
        for (const decl of m.decls as any[]) {
          const body = recordBodyOf(decl);
          const own = body?.members.find(
            (x: any) => x.m === 'derived' && x.name === member && x.type && x.loc,
          );
          if (!own) continue;
          const r = memberRange(text, own, member);
          out.push({
            title: `make ${decl.name}.${member} defaulted (x?: T = e)`,
            kind: 'quickfix',
            diagnostics: [d],
            edit: {
              changes: {
                [uri]: [
                  {
                    range: {
                      start: { line: r.el, character: r.ec },
                      end: { line: r.el, character: r.ec },
                    },
                    newText: '?',
                  },
                ],
              },
            },
          });
        }
      }
      mm = /^required member ([A-Za-z_][A-Za-z0-9_]*) missing/.exec(d.message ?? '');
      if (mm) {
        const name = mm[1];
        // the construction: the literal at the diagnostic, or the root's literal when the diagnostic names the declaration
        const hit = nodeAt(decls, d.range.start);
        const chain = hit ? [hit.node, ...[...hit.parents].reverse()] : [];
        const obj =
          chain.find((n) => isExpr(n) && n.e === 'obj') ??
          chain
            .filter(isDecl)
            .map((n: any) => (n.d === 'output' ? n.expr : n.d === 'input' ? n.fallback : n.expr))
            .find((e) => isExpr(e) && e.e === 'obj');
        if (obj) {
          // the literal's type: its declared position (a root's annotation), else what inference recorded
          const owner: any = chain.find(
            (n) => isDecl(n) && (n.d === 'output' || n.d === 'input') && n.type,
          );
          let rt: any;
          try {
            rt = owner ? m.env.resolve(owner.type) : (t.types.get(obj)?.rt ?? null);
          } catch {
            rt = null;
          }
          const mem = rt?.t === 'rec' ? rt.members.find((x: any) => x.name === name) : null;
          const value = placeholderFor(mem?.type);
          const last = obj.entries[obj.entries.length - 1];
          const edit = last?.val?.loc
            ? {
                range: {
                  start: { line: last.val.loc.el, character: last.val.loc.ec },
                  end: { line: last.val.loc.el, character: last.val.loc.ec },
                },
                newText: `, ${name}: ${value}`,
              }
            : {
                range: {
                  start: { line: obj.loc.sl, character: obj.loc.sc + 1 },
                  end: { line: obj.loc.sl, character: obj.loc.sc + 1 },
                },
                newText: ` ${name}: ${value}`,
              };
          out.push({
            title: `add ${name}: ${value}`,
            kind: 'quickfix',
            diagnostics: [d],
            isPreferred: true,
            edit: { changes: { [uri]: [edit] } },
          });
        }
      }
    }
    // assists at the range
    const hit = nodeAt(decls, range.start);
    const chain = hit ? [hit.node, ...[...hit.parents].reverse()] : [];
    const one = (title: string, kind: string, edits: any[], target = uri) =>
      out.push({ title, kind, edit: { changes: { [target]: edits } } });
    const insertAt = (p: Pos, newText: string) => ({ range: { start: p, end: p }, newText });
    // annotate an unannotated derived member or constant with its inferred type
    const unannotated = chain.find(
      (n) =>
        (isMember(n) && n.m === 'derived' && !n.type) || (isDecl(n) && n.d === 'const' && !n.type),
    );
    if (unannotated) {
      const ty = t.types.get(unannotated.expr);
      if (ty?.rt) {
        const r = isMember(unannotated)
          ? memberRange(text, unannotated, unannotated.name)
          : nameRange(text, unannotated, unannotated.name);
        one(`annotate: ${typeText(ty.rt)}`, 'refactor.rewrite', [
          insertAt(
            { line: r.el, character: r.ec + (unannotated.hidden ? 1 : 0) },
            `: ${typeText(ty.rt)}`,
          ),
        ]);
      }
    }
    // convert a member's kind: derived <-> hidden, defaulted <-> derived, optional <-> required
    const member = chain.find(
      (n) => isMember(n) && (n.m === 'derived' || n.m === 'value') && n.loc,
    );
    if (member) {
      const r = memberRange(text, member, member.name);
      const afterName = { line: r.el, character: r.ec };
      if (member.m === 'derived') {
        if (member.hidden)
          one('make visible (derived)', 'refactor.rewrite', [
            { range: { start: afterName, end: { line: r.el, character: r.ec + 1 } }, newText: '' },
          ]);
        else one('make hidden (x$)', 'refactor.rewrite', [insertAt(afterName, '$')]);
        if (member.type)
          one('make defaulted (x?: T = e)', 'refactor.rewrite', [insertAt(afterName, '?')]);
      } else if (member.dflt) {
        one('make derived (x: T = e)', 'refactor.rewrite', [
          { range: { start: afterName, end: { line: r.el, character: r.ec + 1 } }, newText: '' },
        ]);
      } else if (member.opt) {
        one('make required', 'refactor.rewrite', [
          { range: { start: afterName, end: { line: r.el, character: r.ec + 1 } }, newText: '' },
        ]);
      } else {
        one('make optional', 'refactor.rewrite', [insertAt(afterName, '?')]);
      }
    }
    // generate: export, an output or input skeleton for a type, the fixture header
    const decl = chain.find(isDecl);
    if (
      decl?.loc &&
      typeof decl.name === 'string' &&
      !decl.exported &&
      decl.d !== 'import' &&
      decl.d !== 're_export'
    )
      one(`export ${decl.name}`, 'refactor.rewrite', [
        insertAt({ line: decl.loc.sl, character: decl.loc.sc }, 'export '),
      ]);
    if (decl?.d === 'type') {
      let rt: any;
      try {
        rt = m.env.resolve(decl.type);
      } catch {
        rt = null;
      }
      if (rt?.t === 'rec') {
        const req = rt.members
          .filter((x: any) => x.kind === 'req')
          .map((x: any) => `${x.name}: ${placeholderFor(x.type)}`);
        const end = { line: lines.length - 1, character: lines[lines.length - 1].length };
        const lead = lines[lines.length - 1] === '' ? '' : '\n';
        const lower = decl.name[0].toLowerCase() + decl.name.slice(1);
        one(`generate an output of ${decl.name}`, 'refactor.rewrite', [
          insertAt(
            end,
            `${lead}output ${lower}: ${decl.name} = { ${req.join(', ')}${req.length ? ' ' : ''}}\n`,
          ),
        ]);
        one(`generate an input of ${decl.name}`, 'refactor.rewrite', [
          insertAt(end, `${lead}input ${lower}: ${decl.name}\n`),
        ]);
      }
    }
    if (diagnostics.length && !/^\/\/ @expect-/.test(lines[0] ?? '')) {
      const first = diagnostics.find((d) => d.severity === 1) ?? diagnostics[0];
      const code = String(first.code ?? '');
      const phase =
        code.startsWith('E1') || code.startsWith('E2')
          ? 'parsing'
          : code.startsWith('E5') || code.startsWith('E6') || /^[A-Z][A-Za-z0-9_]*\./.test(code)
            ? 'binding'
            : 'checking';
      one('generate the fixture header (@expect-phase / @expect-error)', 'refactor.rewrite', [
        insertAt(
          { line: 0, character: 0 },
          `// @expect-phase: ${phase}\n// @expect-error: ${code}\n`,
        ),
      ]);
    }
    // fill the missing required members of a literal
    const obj = chain.find((n) => isExpr(n) && n.e === 'obj');
    if (obj?.loc) {
      const owner: any = chain.find(
        (n) => isDecl(n) && (n.d === 'output' || n.d === 'input') && n.type,
      );
      let rt: any;
      try {
        rt =
          owner && owner.expr === obj ? m.env.resolve(owner.type) : (t.types.get(obj)?.rt ?? null);
      } catch {
        rt = null;
      }
      if (rt?.t === 'rec') {
        const have = new Set(obj.entries.map((en: any) => en.key));
        const missing = rt.members.filter((x: any) => x.kind === 'req' && !have.has(x.name));
        if (missing.length) {
          const last = obj.entries[obj.entries.length - 1];
          const fill = missing.map((x: any) => `${x.name}: ${placeholderFor(x.type)}`).join(', ');
          const edit = last?.val?.loc
            ? insertAt({ line: last.val.loc.el, character: last.val.loc.ec }, `, ${fill}`)
            : insertAt({ line: obj.loc.sl, character: obj.loc.sc + 1 }, ` ${fill}`);
          one(
            `fill the required members: ${missing.map((x: any) => x.name).join(', ')}`,
            'refactor.rewrite',
            [edit],
          );
        }
      }
    }
    // inline a constant: its expression at every use, the declaration gone
    if (decl?.d === 'const' && decl.loc && decl.expr?.loc) {
      const nr = nameRange(text, decl, decl.name);
      const refs = references(uri, { line: nr.sl, character: nr.sc }, false).filter(
        (r) => r.module.path === m.path,
      );
      if (refs.length) {
        const src = srcOf(text, decl.expr.loc);
        const edits = refs.map((r) => ({
          range: rangeOf(r.loc),
          newText:
            isExpr(decl.expr) &&
            ['name', 'lit', 'unitlit', 'call', 'member', 'paren'].includes(decl.expr.e)
              ? src
              : `(${src})`,
        }));
        edits.push({
          range: {
            start: { line: decl.loc.sl, character: 0 },
            end: { line: decl.loc.el + 1, character: 0 },
          },
          newText: '',
        });
        one(`inline ${decl.name}`, 'refactor.inline', edits);
      }
    }
    // extract an inline record type into a named type
    const ownBody = decl ? recordBodyOf(decl) : null;
    const inlineRecord =
      chain.find((n) => isType(n) && n.k === 'record' && n.loc && n !== ownBody) ??
      (() => {
        const mem = chain.find(
          (n) => isMember(n) && n.m === 'value' && n.type?.k === 'record' && n.type.loc,
        );
        return mem?.type;
      })();
    if (inlineRecord && decl?.loc) {
      one('extract to a named type', 'refactor.extract', [
        {
          range: {
            start: { line: decl.loc.sl, character: 0 },
            end: { line: decl.loc.sl, character: 0 },
          },
          newText: `type Extracted = ${srcOf(text, inlineRecord.loc)}\n`,
        },
        { range: rangeOf(inlineRecord.loc), newText: 'Extracted' },
      ]);
    }
    // a unit literal in its base unit
    const unitLit = chain.find((n) => isExpr(n) && n.e === 'unitlit' && n.loc);
    if (unitLit) {
      try {
        const u = m.env.unitInfo(unitLit.unit);
        const base = m.env.baseUnitOf.get(u.key) ?? u.key;
        if (base !== unitLit.unit)
          one(`convert to ${unitLit.num * u.toBase}${base}`, 'refactor.rewrite', [
            { range: rangeOf(unitLit.loc), newText: `${unitLit.num * u.toBase}${base}` },
          ]);
      } catch {
        /* an unknown unit is a diagnostic */
      }
    }
    // reorder a record's members into the canonical order
    const typeDecl = chain.find((n) => isDecl(n) && n.d === 'type');
    const body = typeDecl ? recordBodyOf(typeDecl) : null;
    if (body?.loc && body.members.length > 1 && body.members.every((x: any) => x.loc)) {
      const rank = (x: any) =>
        x.m === 'context'
          ? 0
          : x.m === 'value'
            ? x.dflt
              ? 3
              : x.opt
                ? 2
                : 1
            : x.m === 'derived'
              ? x.hidden
                ? 5
                : 4
              : 6;
      const sorted = [...body.members]
        .map((x: any, i: number) => ({ x, i }))
        .sort((p, q) => rank(p.x) - rank(q.x) || p.i - q.i);
      if (sorted.some((s, i) => s.i !== i)) {
        const indent = ' '.repeat((lines[body.loc.sl].match(/^ */)?.[0].length ?? 0) + 4);
        const close = ' '.repeat(lines[body.loc.sl].match(/^ */)?.[0].length ?? 0);
        const members = sorted.map((s) => indent + srcOf(text, s.x.loc).replace(/,\s*$/, ''));
        const trailing = body.open ? `\n${indent}...` : '';
        one('reorder the members canonically', 'refactor.rewrite', [
          { range: rangeOf(body.loc), newText: `{\n${members.join('\n')}${trailing}\n${close}}` },
        ]);
      }
    }
    // an assert's inline `else error …` into a diagnostic declaration, and back
    const assertMember = chain.find((n) => isMember(n) && n.m === 'assert' && n.loc);
    if (assertMember?.tail && typeDecl?.loc) {
      const tail = assertMember.tail;
      if (tail.t === 'inline') {
        // the names the template reads become the parameters, typed as inferred
        const names: string[] = [];
        const params: string[] = [];
        for (const part of tail.template) {
          if (typeof part === 'string') continue;
          const found: any[] = [];
          findAll(part, (n) => isExpr(n) && n.e === 'name', found);
          for (const n of found)
            if (!names.includes(n.name)) {
              names.push(n.name);
              const ty = t.types.get(n);
              params.push(`${n.name}: ${ty?.rt ? typeText(ty.rt) : 'any'}`);
            }
        }
        const template =
          '`' +
          tail.template
            .map((p: any) => (typeof p === 'string' ? p : '${' + exprText(p) + '}'))
            .join('') +
          '`';
        const src = srcOf(text, assertMember.loc);
        const elseAt = src.search(/\belse\b/);
        if (elseAt >= 0) {
          const tailStart = (() => {
            let line = assertMember.loc.sl,
              col = assertMember.loc.sc;
            for (let i = 0; i < elseAt; i++) {
              if (src[i] === '\n') {
                line++;
                col = 0;
              } else col++;
            }
            return { line, character: col };
          })();
          one(`declare a diagnostic for ${assertMember.name}`, 'refactor.extract', [
            {
              range: {
                start: { line: typeDecl.loc.sl, character: 0 },
                end: { line: typeDecl.loc.sl, character: 0 },
              },
              newText: `diagnostic ${assertMember.name}(${params.join(', ')}) {\n    severity = ${tail.severity}\n    message = ${template}\n}\n`,
            },
            {
              range: {
                start: tailStart,
                end: { line: assertMember.loc.el, character: assertMember.loc.ec },
              },
              newText: `else ${assertMember.name}(${names.join(', ')})`,
            },
          ]);
        }
      } else if (tail.t === 'ref') {
        const dd = m.env.diags.get(tail.name);
        const ddecl: any = m.decls.find((d) => d.d === 'diagnostic' && d.name === tail.name);
        if (dd && ddecl) {
          const argText = tail.args.map((a: any) => (a.loc ? srcOf(text, a.loc) : exprText(a)));
          const message =
            '`' +
            dd.template
              .map((p: any) => {
                if (typeof p === 'string') return p;
                const i = p.e === 'name' ? dd.params.findIndex((q: any) => q.name === p.name) : -1;
                return '${' + (i >= 0 ? argText[i] : exprText(p)) + '}';
              })
              .join('') +
            '`';
          const src = srcOf(text, assertMember.loc);
          const elseAt = src.search(/\belse\b/);
          if (elseAt >= 0) {
            const tailStart = (() => {
              let line = assertMember.loc.sl,
                col = assertMember.loc.sc;
              for (let i = 0; i < elseAt; i++) {
                if (src[i] === '\n') {
                  line++;
                  col = 0;
                } else col++;
              }
              return { line, character: col };
            })();
            one(`inline the diagnostic ${tail.name}`, 'refactor.inline', [
              {
                range: {
                  start: tailStart,
                  end: { line: assertMember.loc.el, character: assertMember.loc.ec },
                },
                newText: `else ${dd.severity} ${message}`,
              },
            ]);
          }
        }
      }
    }
    // an `if` chain over a discriminant into a `match`, and back
    const ifChain = chain.find((n) => isExpr(n) && n.e === 'if' && n.loc);
    if (ifChain) {
      // conditions `subject.member == "lit"` on one subject and member, the union's arms telling which record each literal picks
      const arms: { lit: string; body: any }[] = [];
      let subject: any = null,
        member: string | null = null,
        tailExpr: any = null,
        ok = true;
      for (let n: any = ifChain; ; n = n.f) {
        if (n.e !== 'if') {
          tailExpr = n;
          break;
        }
        const c = n.c;
        if (
          c.e === 'bin' &&
          c.op === '==' &&
          c.l.e === 'member' &&
          c.r.e === 'lit' &&
          typeof c.r.v === 'string'
        ) {
          if (!subject) {
            subject = c.l.x;
            member = c.l.name;
          } else if (exprText(subject) !== exprText(c.l.x) || member !== c.l.name) {
            ok = false;
            break;
          }
          arms.push({ lit: c.r.v, body: n.t });
        } else {
          ok = false;
          break;
        }
      }
      const srt = subject ? t.types.get(subject)?.rt : null;
      if (ok && arms.length && srt?.t === 'union') {
        const armName = (lit: string) =>
          srt.arms.find(
            (r: any) =>
              r.t === 'rec' &&
              r.members.some(
                (mm: any) => mm.name === member && mm.type?.t === 'lit' && mm.type.v === lit,
              ),
          )?.name;
        const names = arms.map((a) => armName(a.lit));
        if (names.every(Boolean)) {
          const indent = ' '.repeat((lines[ifChain.loc.sl].match(/^ */)?.[0].length ?? 0) + 4);
          const v = subject.e === 'name' ? subject.name[0] : 'v';
          const cases = arms.map(
            (a, i) =>
              `${indent}(${v}: ${names[i]}) => ${srcOf(text, a.body.loc).replace(new RegExp(`\\b${exprText(subject).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`, 'g'), v)}`,
          );
          if (tailExpr?.loc) cases.push(`${indent}(other) => ${srcOf(text, tailExpr.loc)}`);
          one('convert to match', 'refactor.rewrite', [
            {
              range: rangeOf(ifChain.loc),
              newText: `match ${exprText(subject)} {\n${cases.join('\n')}\n${indent.slice(4)}}`,
            },
          ]);
        }
      }
    }
    const matchExpr = chain.find((n) => isExpr(n) && n.e === 'match' && n.loc);
    if (matchExpr) {
      // arms typed by records that a literal member discriminates: `if subject.kind == "lit" then … else …`
      const srt = t.types.get(matchExpr.subject)?.rt;
      const recs: any[] = srt?.t === 'union' ? srt.arms.filter((r: any) => r.t === 'rec') : [];
      const disc = recs.length
        ? recs[0].members.find(
            (mm: any) =>
              mm.type?.t === 'lit' &&
              recs.every((r: any) =>
                r.members.some((x: any) => x.name === mm.name && x.type?.t === 'lit'),
              ),
          )
        : null;
      if (disc) {
        const parts: string[] = [];
        let fallback: string | null = null;
        let ok = true;
        for (const arm of matchExpr.arms) {
          const body = arm.body?.loc
            ? srcOf(text, arm.body.loc).replace(
                new RegExp(`\\b${arm.v}\\b`, 'g'),
                exprText(matchExpr.subject),
              )
            : null;
          if (!body) {
            ok = false;
            break;
          }
          const rec =
            arm.type?.k === 'named' ? recs.find((r: any) => r.name === arm.type.name) : null;
          const lit = rec?.members.find((x: any) => x.name === disc.name)?.type?.v;
          if (typeof lit === 'string')
            parts.push(
              `if ${exprText(matchExpr.subject)}.${disc.name} == ${JSON.stringify(lit)} then ${body}`,
            );
          else fallback = body;
        }
        if (ok && parts.length)
          one('convert to if', 'refactor.rewrite', [
            {
              range: rangeOf(matchExpr.loc),
              newText: `${parts.join(' else ')} else ${fallback ?? 'null'}`,
            },
          ]);
      }
    }
    // inline a derived member into its sibling uses
    const derived = chain.find((n) => isMember(n) && n.m === 'derived' && n.loc && n.expr?.loc);
    if (derived && body?.loc) {
      const uses: any[] = [];
      for (const other of body.members) {
        if (other === derived) continue;
        findAll(
          other,
          (n) => isExpr(n) && n.e === 'name' && n.name === derived.name && n.loc,
          uses,
        );
      }
      if (uses.length) {
        const src = srcOf(text, derived.expr.loc);
        const wrapped =
          isExpr(derived.expr) &&
          ['name', 'lit', 'unitlit', 'call', 'member', 'paren'].includes(derived.expr.e)
            ? src
            : `(${src})`;
        const edits = uses.map((u) => ({ range: rangeOf(u.loc), newText: wrapped }));
        edits.push({
          range: {
            start: { line: derived.loc.sl, character: 0 },
            end: { line: derived.loc.el + 1, character: 0 },
          },
          newText: '',
        });
        one(`inline ${derived.name}`, 'refactor.inline', edits);
      }
    }
    // flip the operands of a comparison
    const cmp = chain.find(
      (n) =>
        isExpr(n) &&
        n.e === 'bin' &&
        ['<', '>', '<=', '>=', '==', '!='].includes(n.op) &&
        n.l.loc &&
        n.r.loc,
    );
    if (cmp) {
      const flipped: Record<string, string> = {
        '<': '>',
        '>': '<',
        '<=': '>=',
        '>=': '<=',
        '==': '==',
        '!=': '!=',
      };
      one('flip the comparison', 'refactor.rewrite', [
        {
          range: rangeOf(cmp.loc),
          newText: `${srcOf(text, cmp.r.loc)} ${flipped[cmp.op]} ${srcOf(text, cmp.l.loc)}`,
        },
      ]);
    }
    // extract the selected expression: into a constant (a constant expression), or a derived member (inside a record body)
    const selected =
      range.start.line !== range.end.line || range.start.character !== range.end.character
        ? chain.find(
            (n) =>
              isExpr(n) &&
              n.loc &&
              n.loc.sl === range.start.line &&
              n.loc.sc === range.start.character &&
              n.loc.el === range.end.line &&
              n.loc.ec === range.end.character,
          )
        : null;
    if (selected && !(isExpr(selected) && selected.e === 'name')) {
      const mentionsName = (x: any): boolean =>
        !!x &&
        typeof x === 'object' &&
        (Array.isArray(x)
          ? x.some(mentionsName)
          : (isExpr(x) && (x.e === 'name' || x.e === 'ctx' || x.e === 'referrers')) ||
            Object.entries(x).some(([k, v]) => k !== 'loc' && mentionsName(v)));
      const src = srcOf(text, selected.loc);
      const enclosingMember = chain.find((n) => isMember(n) && n.loc);
      const enclosingDecl = chain.find(isDecl);
      if (!mentionsName(selected) && enclosingDecl?.loc)
        one('extract to a constant', 'refactor.extract', [
          insertAt({ line: enclosingDecl.loc.sl, character: 0 }, `const extracted = ${src}\n`),
          { range: rangeOf(selected.loc), newText: 'extracted' },
        ]);
      if (enclosingMember && enclosingMember.m !== 'context') {
        const indent = lines[enclosingMember.loc.sl].match(/^ */)?.[0] ?? '';
        one('extract to a derived member', 'refactor.extract', [
          insertAt({ line: enclosingMember.loc.sl, character: 0 }, `${indent}extracted = ${src}\n`),
          { range: rangeOf(selected.loc), newText: 'extracted' },
        ]);
      }
    }
  }
  return out;
}
// the modules that export a name: the universe's, the other open
// documents' universes, then the .decl files beside the module
function exportersOf(a: Analysis, m: Module, name: string): { path: string }[] {
  const out: { path: string }[] = [];
  const seen = new Set<string>([m.path]);
  const consider = (mod: Module) => {
    if (!seen.has(mod.path) && mod.exports.has(name)) {
      seen.add(mod.path);
      out.push({ path: mod.path });
    }
  };
  for (const mod of a.run.modules) consider(mod);
  for (const other of lastGood.values()) for (const mod of other.run.modules) consider(mod);
  let names: string[];
  try {
    names = require_readdir(dirname(m.path));
  } catch {
    names = [];
  }
  for (const f of names.sort()) {
    if (!f.endsWith('.decl')) continue;
    const p = absPath(dirname(m.path), f);
    if (seen.has(p)) continue;
    const text = overlay.get(p) ?? readText(p);
    const { decls, errors } = parseSource(text);
    if (errors.length) continue;
    if (decls.some((d: any) => d.exported && d.name === name && d.d !== 'import')) {
      seen.add(p);
      out.push({ path: p });
    }
  }
  return out;
}
function require_readdir(dir: string): string[] {
  return host.readDir(dir);
}
function safeResolve(m: Module, typeName: string): any {
  try {
    return m.env.resolve({ k: 'named', name: typeName, args: [] } as any);
  } catch {
    return null;
  }
}
function findAll(x: any, pred: (n: any) => boolean, out: any[]) {
  if (!x || typeof x !== 'object') return;
  if (Array.isArray(x)) {
    x.forEach((y) => findAll(y, pred, out));
    return;
  }
  if (pred(x)) out.push(x);
  for (const [k, v] of Object.entries(x))
    if (k !== 'loc' && v && typeof v === 'object') findAll(v, pred, out);
}
function require_rel(from: string, to: string): string {
  const rel = relativePath(dirname(from), to);
  return rel.startsWith('.') ? rel.replace(/^\.\//, '') : rel;
}
function relativePath(fromDir: string, to: string): string {
  const f = fromDir.split('/').filter(Boolean),
    t = to.split('/').filter(Boolean);
  let i = 0;
  while (i < f.length && i < t.length && f[i] === t[i]) i++;
  return [...f.slice(i).map(() => '..'), ...t.slice(i)].join('/');
}

// ---------------- local variables: linked editing, rename ----------------
// a comprehension variable, a lambda parameter, a match arm's variable, or
// a function parameter: its binding token and its uses in its scope
const bindsName = (n: any, name: string): boolean =>
  (isExpr(n) &&
    (((n.e === 'comp' || n.e === 'mapcomp') && n.clauses.some((c: any) => c.v === name)) ||
      (n.e === 'lambda' && n.params.includes(name)) ||
      (n.e === 'match' && n.arms.some((a: any) => a.v === name)))) ||
  (isDecl(n) && n.d === 'func' && n.params.some((p: any) => p.name === name));
function bindingLocs(text: string, scope: any, name: string): Loc[] {
  const src = srcOf(text, scope.loc);
  const lines = text.split('\n');
  const out: Loc[] = [];
  const at = (offset: number) => {
    // an offset in `src` -> a Loc
    let line = scope.loc.sl,
      col = scope.loc.sc;
    for (let i = 0; i < offset; i++) {
      if (src[i] === '\n') {
        line++;
        col = 0;
      } else col++;
    }
    return { sl: line, sc: col, el: line, ec: col + name.length };
  };
  const esc = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const patterns = isDecl(scope)
    ? [new RegExp(`\\(([^)]*)\\b(${esc})\\b`)]
    : scope.e === 'lambda'
      ? [new RegExp(`\\b(${esc})\\b(?=[^=]*=>)`)]
      : scope.e === 'match'
        ? [new RegExp(`\\(\\s*(${esc})\\b`, 'g')]
        : [new RegExp(`\\bfor\\s+(${esc})\\b`, 'g')];
  // every pattern ends with the name itself: the token is the tail of the match
  for (const re of patterns) {
    if (re.global) {
      let mm;
      while ((mm = re.exec(src))) out.push(at(mm.index + mm[0].length - name.length));
    } else {
      const mm = re.exec(src);
      if (mm) out.push(at(mm.index + mm[0].length - name.length));
    }
  }
  void lines;
  return out;
}
function localRanges(uri: string, pos: Pos): Loc[] | null {
  const text = docs.get(uri);
  if (text === undefined) return null;
  const { decls, errors } = parseSource(text);
  if (errors.length) return null;
  const hit = nodeAt(decls, pos);
  if (!hit) return null;
  let name: string | null = null,
    scope: any = null;
  if (isExpr(hit.node) && hit.node.e === 'name') {
    name = hit.node.name;
    scope = [...hit.parents].reverse().find((p) => bindsName(p, name!)) ?? null;
  } else {
    // on a binding token: the scope node itself, the name under the cursor
    const line = text.split('\n')[pos.line] ?? '';
    const w = /[A-Za-z_][A-Za-z0-9_]*/g;
    let mm;
    while ((mm = w.exec(line)))
      if (mm.index <= pos.character && pos.character <= mm.index + mm[0].length) {
        name = mm[0];
        break;
      }
    if (!name) return null;
    scope = [hit.node, ...[...hit.parents].reverse()].find((p) => bindsName(p, name!)) ?? null;
  }
  if (!name || !scope?.loc) return null;
  const locs = bindingLocs(text, scope, name);
  const uses = (x: any) => {
    if (!x || typeof x !== 'object') return;
    if (Array.isArray(x)) {
      x.forEach(uses);
      return;
    }
    if (x !== scope && bindsName(x, name)) return; // shadowed below
    if (isExpr(x) && x.e === 'name' && x.name === name && x.loc) locs.push(x.loc);
    for (const [k, v] of Object.entries(x)) if (k !== 'loc' && v && typeof v === 'object') uses(v);
  };
  for (const [k, v] of Object.entries(scope))
    if (k !== 'loc' && v && typeof v === 'object') uses(v);
  const key = (l: Loc) => `${l.sl}:${l.sc}`;
  const seen = new Set<string>();
  return locs
    .filter((l) => !seen.has(key(l)) && seen.add(key(l)))
    .sort((p, q) => p.sl - q.sl || p.sc - q.sc);
}
function linkedEditingRange(uri: string, pos: Pos): any {
  const locs = localRanges(uri, pos);
  return locs && locs.length
    ? { ranges: locs.map(rangeOf), wordPattern: '[A-Za-z_][A-Za-z0-9_]*' }
    : null;
}

// ---------------- on-type formatting ----------------
// `\n`: the new line takes the previous line's indentation, one level
// deeper after an opening bracket or a continuation point (§2.9); `}`:
// the line dedents to the opening brace's line
function onTypeFormatting(uri: string, pos: Pos, ch: string): any[] {
  const text = docs.get(uri);
  if (text === undefined) return [];
  const lines = text.split('\n');
  const indentOf = (s: string) => s.match(/^ */)?.[0].length ?? 0;
  if (ch === '\n') {
    const prev = lines[pos.line - 1] ?? '';
    const cur = lines[pos.line] ?? '';
    const body = prev.replace(/\/\/.*$/, '').trimEnd();
    let want = indentOf(prev);
    if (/[{[(]$/.test(body) || /(?:[+\-*/%<>=!&|?:,]|\bthen|\belse|\bin|\bwith|=>)$/.test(body))
      want += 4;
    if (/^\s*[}\])]/.test(cur)) want = Math.max(0, want - 4);
    const have = indentOf(cur);
    if (have === want) return [];
    return [
      {
        range: {
          start: { line: pos.line, character: 0 },
          end: { line: pos.line, character: have },
        },
        newText: ' '.repeat(want),
      },
    ];
  }
  if (ch === '}' || ch === ']' || ch === ')') {
    const cur = lines[pos.line] ?? '';
    if (cur.trim() !== ch) return [];
    // the opening bracket's line: scan back with a depth count
    const opens: Record<string, string> = { '}': '{', ']': '[', ')': '(' };
    let depth = 0;
    for (let l = pos.line; l >= 0; l--) {
      const s = lines[l];
      for (let i: number = (l === pos.line ? cur.indexOf(ch) : s.length) - 1; i >= 0; i--) {
        const c: string = s[i];
        if (c === ch) depth++;
        else if (c === opens[ch]) {
          if (depth === 0) {
            const want = indentOf(s);
            const have = indentOf(cur);
            return have === want
              ? []
              : [
                  {
                    range: {
                      start: { line: pos.line, character: 0 },
                      end: { line: pos.line, character: have },
                    },
                    newText: ' '.repeat(want),
                  },
                ];
          }
          depth--;
        }
      }
    }
    return [];
  }
  return [];
}

// ---------------- progress ----------------
// a client that accepts work-done progress sees an analysis of a large
// universe as a progress item (03_lsp.md §14)
let progressSupported = false;
let progressSeq = 0;
function withProgress<T>(title: string, f: () => T): T {
  if (!progressSupported) return f();
  // the request's id is the sequence number: an integer, which every client accepts (Neovim rejects a string)
  const seq = ++progressSeq;
  const token = `decl-${seq}`;
  send({ jsonrpc: '2.0', id: seq, method: 'window/workDoneProgress/create', params: { token } });
  notify('$/progress', { token, value: { kind: 'begin', title, cancellable: false } });
  try {
    return f();
  } finally {
    notify('$/progress', { token, value: { kind: 'end' } });
  }
}

// ---------------- the syntax tree ----------------
function syntaxTree(uri: string): any {
  const text = docs.get(uri);
  if (text === undefined) return null;
  const p = new Parser();
  p.setLanguage(getLanguage());
  return { tree: p.parse(text)!.rootNode.toString() };
}

// ---------------- request handling ----------------
async function handle(msg: any) {
  // a message without a method is the client's response to a request of
  // ours (window/workDoneProgress/create): nothing to answer
  if (msg.method === undefined) return;
  const { id, method, params } = msg;
  switch (method) {
    case 'initialize':
      await io.init(params?.initializationOptions);
      progressSupported = !!params?.capabilities?.window?.workDoneProgress;
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
          signatureHelpProvider: { triggerCharacters: ['(', ','] },
          workspaceSymbolProvider: true,
          selectionRangeProvider: true,
          semanticTokensProvider: {
            legend: { tokenTypes: TOKEN_TYPES, tokenModifiers: TOKEN_MODS },
            full: true,
          },
          inlayHintProvider: true,
          callHierarchyProvider: true,
          typeHierarchyProvider: true,
          codeActionProvider: {
            codeActionKinds: [
              'quickfix',
              'refactor.rewrite',
              'refactor.extract',
              'refactor.inline',
            ],
          },
          linkedEditingRangeProvider: true,
          documentOnTypeFormattingProvider: {
            firstTriggerCharacter: '\n',
            moreTriggerCharacter: ['}', ']', ')'],
          },
          executeCommandProvider: {
            commands: [
              'decl.evaluate',
              'decl.validate',
              'decl.trace',
              'decl.showSyntaxTree',
              'decl.reloadWorkspace',
            ],
          },
        },
        serverInfo: { name: 'decl-lsp', version: '0.3.0' },
      });
      break;
    case 'initialized':
      break;
    case 'decl/files': {
      // the browser client's workspace files (lsp-web.ts): the host's files
      for (const f of params?.files ?? []) host.writeFile(pathOf(f.uri), f.text);
      for (const u of params?.remove ?? []) {
        const p = pathOf(u);
        if ('files' in host) (host as any).files.delete(p);
      }
      analyses.clear();
      for (const u of docs.keys()) analyze(u);
      break;
    }
    case 'workspace/didChangeConfiguration':
      config.inputs = params?.settings?.decl?.inputs ?? {};
      for (const k of Object.keys(hints) as (keyof typeof hints)[]) {
        const v = params?.settings?.decl?.inlayHints?.[k];
        if (typeof v === 'boolean') hints[k] = v;
      }
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
    case 'textDocument/didSave':
      break;
    case 'textDocument/didClose':
      docs.delete(params.textDocument.uri);
      overlay.delete(pathOf(params.textDocument.uri));
      analyses.delete(params.textDocument.uri);
      lastGood.delete(params.textDocument.uri);
      notify('textDocument/publishDiagnostics', { uri: params.textDocument.uri, diagnostics: [] });
      break;
    case 'textDocument/hover':
      reply(id, hover(params.textDocument.uri, params.position));
      break;
    case 'textDocument/definition':
      reply(id, definition(params.textDocument.uri, params.position));
      break;
    case 'textDocument/typeDefinition':
      reply(id, typeDefinition(params.textDocument.uri, params.position));
      break;
    case 'textDocument/references':
      reply(
        id,
        references(
          params.textDocument.uri,
          params.position,
          !!params.context?.includeDeclaration,
        ).map((r) => location(r.module, r.loc)),
      );
      break;
    case 'textDocument/documentHighlight': {
      const path = pathOf(params.textDocument.uri);
      reply(
        id,
        references(params.textDocument.uri, params.position, true)
          .filter((r) => r.module.path === path)
          .map((r) => ({ range: rangeOf(r.loc), kind: 1 })),
      );
      break;
    }
    case 'textDocument/completion':
      reply(id, completion(params.textDocument.uri, params.position));
      break;
    case 'textDocument/documentSymbol':
      reply(id, documentSymbols(params.textDocument.uri));
      break;
    case 'textDocument/foldingRange':
      reply(id, foldingRanges(params.textDocument.uri));
      break;
    case 'textDocument/formatting':
      reply(id, formatting(params.textDocument.uri));
      break;
    case 'textDocument/prepareRename':
      reply(id, prepareRename(params.textDocument.uri, params.position));
      break;
    case 'textDocument/rename':
      reply(id, rename(params.textDocument.uri, params.position, params.newName));
      break;
    case 'textDocument/codeLens':
      reply(id, codeLenses(params.textDocument.uri));
      break;
    case 'textDocument/signatureHelp':
      reply(id, signatureHelp(params.textDocument.uri, params.position));
      break;
    case 'workspace/symbol':
      reply(id, workspaceSymbols(params.query ?? ''));
      break;
    case 'textDocument/selectionRange':
      reply(id, selectionRanges(params.textDocument.uri, params.positions ?? []));
      break;
    case 'textDocument/semanticTokens/full':
      reply(id, semanticTokens(params.textDocument.uri));
      break;
    case 'textDocument/inlayHint':
      reply(id, inlayHints(params.textDocument.uri, params.range));
      break;
    case 'textDocument/prepareCallHierarchy':
      reply(id, prepareHierarchy(params.textDocument.uri, params.position, 'func'));
      break;
    case 'callHierarchy/incomingCalls':
      reply(id, incomingCalls(params.item));
      break;
    case 'callHierarchy/outgoingCalls':
      reply(id, outgoingCalls(params.item));
      break;
    case 'textDocument/prepareTypeHierarchy':
      reply(id, prepareHierarchy(params.textDocument.uri, params.position, 'type'));
      break;
    case 'typeHierarchy/supertypes':
      reply(id, supertypes(params.item));
      break;
    case 'typeHierarchy/subtypes':
      reply(id, subtypes(params.item));
      break;
    case 'textDocument/codeAction':
      reply(
        id,
        codeActions(params.textDocument.uri, params.range, params.context?.diagnostics ?? []),
      );
      break;
    case 'textDocument/linkedEditingRange':
      reply(id, linkedEditingRange(params.textDocument.uri, params.position));
      break;
    case 'textDocument/onTypeFormatting':
      reply(id, onTypeFormatting(params.textDocument.uri, params.position, params.ch));
      break;
    case 'workspace/executeCommand': {
      // a refused command (an unknown root, an unreadable binding) answers null, never silence
      let result: any = null;
      try {
        result = executeCommand(params.command, params.arguments);
      } catch (e: any) {
        if (!(e instanceof SessionError)) throw e;
      }
      reply(id, result);
      break;
    }
    case 'shutdown':
      reply(id, null);
      break;
    case 'exit':
      io.exit();
      break;
    default:
      if (id !== undefined) reply(id, null);
  }
}
