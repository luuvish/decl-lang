// LSP server end-to-end over stdio (Phase 4 exit criterion:
// diagnostics displayed in an editor): initialize, open/change with
// publishDiagnostics, hover, and definition — including one import hop.
import { spawn } from 'node:child_process';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
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

const server = spawn('node', [join(root, 'decl-ts/src/lsp.ts')], {
  stdio: ['pipe', 'pipe', 'inherit'],
});
let buf = Buffer.alloc(0);
const pendingReplies = new Map<number, (r: any) => void>();
const notifications: any[] = [];
const notifyWaiters: ((m: any) => boolean)[] = [];
server.stdout.on('data', (c) => {
  buf = Buffer.concat([buf, c]);
  for (;;) {
    const he = buf.indexOf('\r\n\r\n');
    if (he < 0) return;
    const m = /Content-Length: (\d+)/i.exec(buf.subarray(0, he).toString())!;
    const len = parseInt(m[1], 10);
    if (buf.length < he + 4 + len) return;
    const msg = JSON.parse(buf.subarray(he + 4, he + 4 + len).toString());
    buf = buf.subarray(he + 4 + len);
    if (msg.id !== undefined && pendingReplies.has(msg.id)) {
      pendingReplies.get(msg.id)!(msg.result);
      pendingReplies.delete(msg.id);
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
const request = (method: string, params: any): Promise<any> =>
  new Promise((res) => {
    const id = nextId++;
    pendingReplies.set(id, res);
    send({ jsonrpc: '2.0', id, method, params });
  });
const notifyServer = (method: string, params: any) => send({ jsonrpc: '2.0', method, params });
const nextDiagnostics = (uri: string): Promise<any> =>
  new Promise((res) => {
    notifyWaiters.push((m) => {
      if (m.method === 'textDocument/publishDiagnostics' && m.params.uri === uri) {
        res(m.params);
        return true;
      }
      return false;
    });
  });

const dir = mkdtempSync(join(tmpdir(), 'decl-lsp-'));
const libPath = join(dir, 'lib.decl');
writeFileSync(
  libPath,
  'export type Service = { name: string, port?: 1..65535 = 8080 }\nexport const MAX = 16\nexport func cap(n: int): int = std.math.min(n, MAX)\nexport type Public = Service { public: bool }\nexport type Level = "low" | "high"\n',
);
const mainPath = join(dir, 'main.decl');
const mainUri = pathToFileURL(mainPath).toString();
writeFileSync(mainPath, '');

const init = await request('initialize', { processId: null, rootUri: null, capabilities: {} });
check(
  'initialize advertises capabilities',
  init.capabilities.hoverProvider === true && init.capabilities.definitionProvider === true,
);
notifyServer('initialized', {});

// syntax error diagnostics
{
  const p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didOpen', {
    textDocument: { uri: mainUri, languageId: 'decl', version: 1, text: 'const x = \n' },
  });
  const d = await p;
  check(
    'syntax error published',
    d.diagnostics.length > 0 && d.diagnostics[0].message === 'syntax error',
    JSON.stringify(d),
  );
}
// checker diagnostics with a useful anchor
{
  const p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 2 },
    contentChanges: [{ text: 'type Bad = 10..3\n' }],
  });
  const d = await p;
  check(
    'checker diagnostic published with code',
    d.diagnostics.some((x: any) => x.code === 'E4011'),
    JSON.stringify(d),
  );
  check(
    'diagnostic anchored to the name',
    d.diagnostics[0].range.start.line === 0 && d.diagnostics[0].range.start.character > 0,
    JSON.stringify(d.diagnostics[0].range),
  );
}
// clean file + import; hover and definition
const mainSrc =
  'import { Service, MAX as LIMIT, cap, Level } from "./lib.decl"\nconst top = LIMIT\nexport output s: Service = { name: "a" }\nexport output t: Service = {\n    name: "b"\n}\nconst first = s.name\nconst c = cap(top)\nconst d = 250ms\ntype Local = Service { extra = name, level?: Level = "low" }\n';
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
  const h = await request('textDocument/hover', {
    textDocument: { uri: mainUri },
    position: { line: 1, character: 7 },
  });
  check(
    'hover shows the declaration',
    h && h.contents.value.includes('const top = LIMIT'),
    JSON.stringify(h),
  );
  // hover over the renamed import LIMIT
  const h2 = await request('textDocument/hover', {
    textDocument: { uri: mainUri },
    position: { line: 1, character: 13 },
  });
  check(
    'hover follows a renamed import',
    h2 && h2.contents.value.includes('MAX = 16'),
    JSON.stringify(h2),
  );
}
{
  // definition of Service in the output annotation (line 2)
  const col = mainSrc.split('\n')[2].indexOf('Service') + 2;
  const def = await request('textDocument/definition', {
    textDocument: { uri: mainUri },
    position: { line: 2, character: col },
  });
  check(
    'definition jumps across the import',
    def && def.uri.endsWith('lib.decl') && def.range.start.line === 0,
    JSON.stringify(def),
  );
}

{
  const td = await request('textDocument/typeDefinition', {
    textDocument: { uri: mainUri },
    position: { line: 6, character: 14 },
  });
  check(
    'type definition of a value of type Service',
    td && td.uri.endsWith('lib.decl') && td.range.start.line === 0,
    JSON.stringify(td),
  );
  const tdm = await request('textDocument/typeDefinition', {
    textDocument: { uri: mainUri },
    position: { line: 9, character: 37 },
  });
  check(
    'type definition on a member typed by a literal-union alias',
    tdm && tdm.uri.endsWith('lib.decl') && tdm.range.start.line === 4,
    JSON.stringify(tdm),
  );
  const refs = await request('textDocument/references', {
    textDocument: { uri: mainUri },
    position: { line: 2, character: 19 },
    context: { includeDeclaration: true },
  });
  check(
    'references of Service: declaration, import item, annotations, extensions',
    refs.length === 6 && refs.filter((r: any) => r.uri.endsWith('lib.decl')).length === 2,
    JSON.stringify(refs),
  );
  const hl = await request('textDocument/documentHighlight', {
    textDocument: { uri: mainUri },
    position: { line: 6, character: 14 },
  });
  check(
    'highlight of s: its declaration and its use',
    hl.length === 2 && hl[0].range.start.line === 2 && hl[1].range.start.line === 6,
    JSON.stringify(hl),
  );
  const c1 = await request('textDocument/completion', {
    textDocument: { uri: mainUri },
    position: { line: 1, character: 15 },
  });
  check(
    'completion of a name prefix',
    c1.items.some((i: any) => i.label === 'LIMIT'),
    JSON.stringify(c1),
  );
  const broken = mainSrc + 'const e = s.\n';
  let p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 20 },
    contentChanges: [{ text: broken }],
  });
  await p;
  const c2 = await request('textDocument/completion', {
    textDocument: { uri: mainUri },
    position: { line: 10, character: 12 },
  });
  check(
    'member completion while the text does not parse',
    c2.items.map((i: any) => i.label).join(',') === 'name,port',
    JSON.stringify(c2),
  );
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 21 },
    contentChanges: [{ text: mainSrc }],
  });
  await p;
  const syms = await request('textDocument/documentSymbol', { textDocument: { uri: mainUri } });
  check(
    'document symbols',
    syms.map((s: any) => s.name).join(',') === 'top,s,t,first,c,d,Local',
    JSON.stringify(syms),
  );
  const folds = await request('textDocument/foldingRange', { textDocument: { uri: mainUri } });
  check(
    'folding of the multi-line output',
    folds.length === 1 && folds[0].startLine === 3 && folds[0].endLine === 5,
    JSON.stringify(folds),
  );
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 22 },
    contentChanges: [{ text: 'const x=1\n' }],
  });
  await p;
  const fmt = await request('textDocument/formatting', {
    textDocument: { uri: mainUri },
    options: { tabSize: 4, insertSpaces: true },
  });
  check(
    'formatting replaces the document with its canonical form',
    fmt.length === 1 && fmt[0].newText === 'const x = 1\n',
    JSON.stringify(fmt),
  );
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 23 },
    contentChanges: [{ text: mainSrc }],
  });
  await p;
  const pr = await request('textDocument/prepareRename', {
    textDocument: { uri: mainUri },
    position: { line: 1, character: 7 },
  });
  check(
    'prepare rename gives the name range',
    pr && pr.placeholder === 'top' && pr.range.start.character === 6,
    JSON.stringify(pr),
  );
  const rn = await request('textDocument/rename', {
    textDocument: { uri: mainUri },
    position: { line: 2, character: 19 },
    newName: 'Svc',
  });
  check(
    'rename edits every module',
    rn && Object.keys(rn.changes).length === 2 && rn.changes[mainUri].length === 4,
    JSON.stringify(rn),
  );
  const lenses = await request('textDocument/codeLens', { textDocument: { uri: mainUri } });
  check(
    'lenses on the outputs',
    lenses.length === 2 && lenses[0].command.command === 'decl.evaluate',
    JSON.stringify(lenses),
  );
  const ev = await request('workspace/executeCommand', {
    command: 'decl.evaluate',
    arguments: [mainUri, 's'],
  });
  check(
    'decl.evaluate returns the document',
    ev && ev.document === '{"name":"a","port":8080}' && ev.diagnostics.length === 0,
    JSON.stringify(ev),
  );
  const va = await request('workspace/executeCommand', {
    command: 'decl.validate',
    arguments: [mainUri, 's'],
  });
  check(
    'decl.validate returns the verdict',
    va && va.verdicts.length === 1 && va.verdicts[0].errors === 0,
    JSON.stringify(va),
  );
  const sh = await request('textDocument/signatureHelp', {
    textDocument: { uri: mainUri },
    position: { line: 7, character: 15 },
  });
  check(
    'signature help of a function call',
    sh && sh.signatures[0].label === 'cap(n: int): int' && sh.activeParameter === 0,
    JSON.stringify(sh),
  );
  const ws = await request('workspace/symbol', { query: 'ca' });
  check(
    'workspace symbols across the universe',
    ws.some((s: any) => s.name === 'cap' && s.location.uri.endsWith('lib.decl')),
    JSON.stringify(ws),
  );
  const sr = await request('textDocument/selectionRange', {
    textDocument: { uri: mainUri },
    positions: [{ line: 6, character: 16 }],
  });
  check(
    'selection ranges grow outward',
    sr[0].range.start.character === 14 && sr[0].parent.range.start.character === 0,
    JSON.stringify(sr),
  );
  const st = await request('textDocument/semanticTokens/full', { textDocument: { uri: mainUri } });
  check(
    'semantic tokens are encoded in fives',
    st.data.length > 0 && st.data.length % 5 === 0,
    JSON.stringify(st),
  );
  const ih = await request('textDocument/inlayHint', {
    textDocument: { uri: mainUri },
    range: { start: { line: 0, character: 0 }, end: { line: 20, character: 0 } },
  });
  check(
    'inlay hints: parameter name, unit base value, derived type',
    ih.some((h: any) => h.label === 'n:') &&
      ih.some((h: any) => h.label === '= 0.25 s') &&
      ih.some((h: any) => h.label === ': string'),
    JSON.stringify(ih),
  );
  const ch = await request('textDocument/prepareCallHierarchy', {
    textDocument: { uri: mainUri },
    position: { line: 7, character: 11 },
  });
  const inc = await request('callHierarchy/incomingCalls', { item: ch[0] });
  check(
    'call hierarchy: cap is called from c',
    ch[0].name === 'cap' && inc.length === 1 && inc[0].from.name === 'c',
    JSON.stringify(inc),
  );
  const th = await request('textDocument/prepareTypeHierarchy', {
    textDocument: { uri: mainUri },
    position: { line: 2, character: 19 },
  });
  const sub = await request('typeHierarchy/subtypes', { item: th[0] });
  check(
    'type hierarchy: Service has two subtypes',
    sub
      .map((s: any) => s.name)
      .sort()
      .join(',') === 'Local,Public',
    JSON.stringify(sub),
  );
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 40 },
    contentChanges: [{ text: 'const z = cap(1)\n' }],
  });
  const dz = await p;
  const ca = await request('textDocument/codeAction', {
    textDocument: { uri: mainUri },
    range: { start: { line: 0, character: 10 }, end: { line: 0, character: 13 } },
    context: { diagnostics: dz.diagnostics },
  });
  check(
    'code action: import the unknown name from the module beside',
    ca.some((x: any) => x.title === 'import cap from "./lib.decl"'),
    JSON.stringify(ca),
  );
  // linked editing and rename of a local variable; the member-kind conversions; flipping a comparison
  const actionsSrc =
    'type Pair = {\n    a: int,\n    b?: int,\n    c = a + 1\n}\nconst xs = [x * 2 for x in [1, 2] if x > 0]\nconst cmp = 3 < 4\n';
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 50 },
    contentChanges: [{ text: actionsSrc }],
  });
  await p;
  const le = await request('textDocument/linkedEditingRange', {
    textDocument: { uri: mainUri },
    position: { line: 5, character: 12 },
  });
  check(
    'linked editing of a comprehension variable',
    le && le.ranges.length === 3,
    JSON.stringify(le),
  );
  const lr = await request('textDocument/rename', {
    textDocument: { uri: mainUri },
    position: { line: 5, character: 12 },
    newName: 'y',
  });
  check('rename of a local variable', lr && lr.changes[mainUri].length === 3, JSON.stringify(lr));
  const conv = await request('textDocument/codeAction', {
    textDocument: { uri: mainUri },
    range: { start: { line: 3, character: 4 }, end: { line: 3, character: 4 } },
    context: { diagnostics: [] },
  });
  check(
    'assists on a derived member: annotate, hide, export',
    ['annotate: int', 'make hidden (x$)', 'export Pair'].every((t) =>
      conv.some((x: any) => x.title === t),
    ),
    JSON.stringify(conv.map((x: any) => x.title)),
  );
  const flip = await request('textDocument/codeAction', {
    textDocument: { uri: mainUri },
    range: { start: { line: 6, character: 14 }, end: { line: 6, character: 14 } },
    context: { diagnostics: [] },
  });
  check(
    'assist: flip the comparison',
    flip.some(
      (x: any) =>
        x.title === 'flip the comparison' && x.edit.changes[mainUri][0].newText === '4 > 3',
    ),
    JSON.stringify(flip),
  );
  // the remaining fixes and assists: the parent's declaration, the context variable, the discriminant, inline, extract, unit, reorder; the context hints
  const src2 =
    'type Parent = { name: string, port: 1..65535 }\ntype Child = Parent { port: int }\ntype Item = { label = $parent.name }\ntype A = { a: int }\ntype B = { b: int }\ntype U = A | B\nconst K = 2\nconst twice = K + K\ntype W = { inner: { q: int } }\nconst dur = 250ms\ntype R = {\n    d = 1,\n    a: int\n}\ntype Ctx = { $parent: ref<Parent>, tag = $parent.name }\n';
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 60 },
    contentChanges: [{ text: src2 }],
  });
  const d2 = await p;
  const at = (line: number, ch: number, diags: any[] = d2.diagnostics) =>
    request('textDocument/codeAction', {
      textDocument: { uri: mainUri },
      range: { start: { line, character: ch }, end: { line, character: ch } },
      context: { diagnostics: diags },
    });
  const titles = (xs: any[]) => xs.map((x: any) => x.title);
  check(
    "fix: the parent's declaration",
    titles(await at(1, 22)).includes("use the parent's declaration: port: 1..65535"),
    JSON.stringify(titles(await at(1, 22))),
  );
  check(
    'fix: declare the context variable',
    titles(await at(2, 24)).includes('declare $parent: ref<{ ... }> on Item'),
    JSON.stringify(titles(await at(2, 24))),
  );
  check(
    'fix: the discriminant',
    titles(await at(5, 5)).includes('add a discriminant `kind` to the arms of U'),
    JSON.stringify(titles(await at(5, 5))),
  );
  const inl = await at(6, 0, []);
  check(
    'assist: inline the constant',
    inl.some((x: any) => x.title === 'inline K' && x.edit.changes[mainUri].length === 3),
    JSON.stringify(titles(inl)),
  );
  check(
    'assist: extract the inline record type',
    titles(await at(8, 11, [])).includes('extract to a named type'),
    JSON.stringify(titles(await at(8, 11, []))),
  );
  check(
    'assist: the unit in its base unit',
    titles(await at(9, 12, [])).includes('convert to 0.25s'),
    JSON.stringify(titles(await at(9, 12, []))),
  );
  const ro = await at(10, 5, []);
  check(
    'assist: reorder the members',
    ro.some(
      (x: any) =>
        x.title === 'reorder the members canonically' &&
        x.edit.changes[mainUri][0].newText === '{\n    a: int\n    d = 1\n}',
    ),
    JSON.stringify(ro.filter((x: any) => x.title.startsWith('reorder'))),
  );
  notifyServer('workspace/didChangeConfiguration', {
    settings: { decl: { inlayHints: { contextVariables: true } } },
  });
  await nextDiagnostics(mainUri);
  const cvh = await request('textDocument/inlayHint', {
    textDocument: { uri: mainUri },
    range: { start: { line: 0, character: 0 }, end: { line: 20, character: 0 } },
  });
  check(
    "hint: the context variable's bound",
    cvh.some((h: any) => h.label === ': ref<Parent>'),
    JSON.stringify(cvh),
  );
  notifyServer('workspace/didChangeConfiguration', {
    settings: { decl: { inlayHints: { contextVariables: false } } },
  });
  await nextDiagnostics(mainUri);
  // the conversions, inlining a member, on-type formatting
  const src3 =
    'type Circle = { kind: "circle", r: int }\ntype Rect = { kind: "rect", w: int }\ninput shape: Circle | Rect\nconst area = if shape.kind == "circle" then shape.r else 0\ntype Box = {\n    w: int,\n    area = w * 2,\n    big = area > 10,\n    assert fits: w <= 100 else error `too wide: ${w}`\n}\n';
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 70 },
    contentChanges: [{ text: src3 }],
  });
  await p;
  const cm = await at(3, 15, []);
  check(
    'assist: convert an if chain over a discriminant to match',
    cm.some(
      (x: any) =>
        x.title === 'convert to match' &&
        x.edit.changes[mainUri][0].newText.startsWith('match shape {'),
    ),
    JSON.stringify(titles(cm)),
  );
  const im = await at(6, 6, []);
  check(
    'assist: inline a derived member into its sibling uses',
    im.some(
      (x: any) => x.title === 'inline area' && x.edit.changes[mainUri][0].newText === '(w * 2)',
    ),
    JSON.stringify(titles(im)),
  );
  const dg = await at(8, 12, []);
  check(
    'assist: declare a diagnostic for an inline else',
    dg.some(
      (x: any) =>
        x.title === 'declare a diagnostic for fits' &&
        x.edit.changes[mainUri][1].newText === 'else fits(w)',
    ),
    JSON.stringify(titles(dg)),
  );
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 71 },
    contentChanges: [{ text: 'type T = {\nx: int\n}\n' }],
  });
  await p;
  const ot = await request('textDocument/onTypeFormatting', {
    textDocument: { uri: mainUri },
    position: { line: 1, character: 0 },
    ch: '\n',
    options: { tabSize: 4, insertSpaces: true },
  });
  check(
    'on-type formatting indents after an opening brace',
    ot.length === 1 && ot[0].newText === '    ',
    JSON.stringify(ot),
  );
  p = nextDiagnostics(mainUri);
  notifyServer('textDocument/didChange', {
    textDocument: { uri: mainUri, version: 41 },
    contentChanges: [{ text: mainSrc }],
  });
  await p;
  const tree = await request('workspace/executeCommand', {
    command: 'decl.showSyntaxTree',
    arguments: [mainUri],
  });
  check(
    'decl.showSyntaxTree returns the tree',
    tree && tree.tree.startsWith('(module'),
    JSON.stringify(tree).slice(0, 120),
  );
}

await request('shutdown', {});
notifyServer('exit', {});
server.stdin.end();

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
