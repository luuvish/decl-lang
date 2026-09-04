// The session object (docs/tooling/02_repl.md §1): a universe — the
// modules loaded from an entry file, their texts taken as a snapshot —
// plus an operation log (bindings, document edits, session declarations,
// reloads). The state is the universe with the log applied, recomputed
// deterministically from the snapshot, which is what makes `:undo` exact
// and a scripted session reproducible. The REPL (repl.ts) and the
// language server drive it; nothing here prints, and every answer is the
// same checker, inference, and engine the command line runs.
import { readFileSync, writeFileSync, readdirSync, statSync } from 'node:fs';
import { resolve as absPath, basename, dirname, relative } from 'node:path';
import { parseSource } from './parse.ts';
import { checkModule } from './checker.ts';
import { loadModules } from './module.ts';
import type { Module } from './module.ts';
import { openPackageUniverse, verifyLock } from './package.ts';
import { Engine } from './engine.ts';
import { Env, EvalErr, Taint, readJson, pathStr, parsePath, mapKey, segText, isRec, isArr, isMap, isRef, isClo, isQ, ABSENT } from './semantics.ts';
import type { Diag, Seg, RT } from './semantics.ts';
import { makeCtx, infer, typeText, STD } from './infer.ts';
import type { Decl, Expr, TypeAst } from './ast.ts';
import { format } from './fmt.ts';

// ---------------- operations ----------------
export type BindSource =
  | { kind: 'file'; file: string; text: string }
  | { kind: 'inline'; text: string }
  | { kind: 'expr'; text: string };

export type Op =
  | { op: 'bind'; name: string; src: BindSource }
  | { op: 'unbind'; name: string }
  | { op: 'edit'; kind: 'create' | 'update' | 'remove'; path: string; expr?: string }
  | { op: 'declare'; name: string; text: string }
  | { op: 'output'; name: string; type?: string; expr: string }
  | { op: 'drop'; name: string }
  | { op: 'reload'; snapshot: Map<string, string> }
  | { op: 'reset' };

/** the document a root is built from, as the session holds it */
export type Document = {
  origin: 'file' | 'inline' | 'expr' | 'fallback' | 'detached';
  file?: string;
  doc: any;              // readJson's shape
  base: any;             // what it started from (`:diff`)
  edited: boolean;
};

type State = {
  snapshot: Map<string, string>;
  decls: Map<string, string>;                          // session declarations, in order
  outputs: Map<string, { type?: string; expr: string }>; // session outputs `x = e`
  documents: Map<string, Document>;
};

export class SessionError extends Error {}

export type Timing = { load: number; check: number; bind: number; evaluate: number; total: number };

export type Run = {
  modules: Module[];
  entry: Module | null;
  loadDiags: Diag[];
  checks: { file: string; diag: Diag }[];
  sessionChecks: Diag[];          // session outputs whose expressions do not check (path: the output)
  eng: Engine | null;
  diags: Diag[];
  timing: Timing;
};

export type RootInfo = {
  kind: 'output' | 'input';
  name: string;
  module: string;                 // '' for a session root
  exported: boolean;
  session: boolean;
  binding: string;                // for inputs and detached outputs
  detail: string;                 // the bound file, when there is one
  edited: boolean;
};

const now = () => (typeof performance !== 'undefined' ? performance.now() : Date.now());
const isRootDiag = (d: Diag, root: string) =>
  d.path === root || d.path.startsWith(root + '.') || d.path.startsWith(root + '[');

/** parse one expression: the text is wrapped in a constant declaration */
export function parseExpr(text: string): Expr {
  const { decls, errors } = parseSource(`const __e = ${text}\n`);
  if (errors.length || decls.length !== 1 || decls[0].d !== 'const') throw new SessionError(`cannot parse expression: ${text.trim()}`);
  return (decls[0] as any).expr;
}

/** parse one module-level declaration; returns it with its name */
export function parseDecl(text: string): { decl: Decl; name: string } {
  const { decls, errors } = parseSource(text.trim() + '\n');
  if (errors.length || decls.length !== 1) throw new SessionError(`cannot parse declaration: ${text.trim().split('\n')[0]}`);
  const d: any = decls[0];
  const name = typeof d.name === 'string' ? d.name : d.d === 'import' ? `import ${d.from}` : `${d.d} ${d.from}`;
  return { decl: decls[0], name };
}

function parseDoc(text: string, what: string): any {
  try { return readJson(text); }
  catch { throw new SessionError(`${what} is not well-formed JSON`); }
}

// ---------------- JSON documents (readJson's shape) ----------------
export function docJson(v: any): string {
  if (v === null) return 'null';
  if (typeof v === 'boolean') return String(v);
  if (typeof v === 'bigint') return v.toString();
  if (typeof v === 'number') { const s = String(v); return /[.eE]/.test(s) ? s : s + '.0'; }
  if (typeof v === 'string') return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(docJson).join(',')}]`;
  if (v && v.__jobj) return `{${v.entries.map(([k, x]: any) => `${JSON.stringify(k)}:${docJson(x)}`).join(',')}}`;
  throw new Error('docJson');
}
const docClone = (v: any): any => readJson(docJson(v));

function docStep(v: any, seg: Seg): any {
  const k = segText(seg);
  if (v && v.__jobj) { const e = v.entries.find(([kk]: any) => kk === k); return e ? e[1] : undefined; }
  if (Array.isArray(v) && typeof k === 'number') return v[k];
  return undefined;
}

// ---------------- pretty printing ----------------
/** canonical JSON, re-indented (numbers and strings untouched) */
export function prettyJson(compact: string): string {
  let out = '', depth = 0, i = 0;
  const pad = () => '  '.repeat(depth);
  while (i < compact.length) {
    const c = compact[i];
    if (c === '"') {
      let j = i + 1;
      while (compact[j] !== '"') { if (compact[j] === '\\') j++; j++; }
      out += compact.slice(i, j + 1); i = j + 1; continue;
    }
    if (c === '{' || c === '[') {
      const close = c === '{' ? '}' : ']';
      if (compact[i + 1] === close) { out += c + close; i += 2; continue; }
      depth++; out += c + '\n' + pad(); i++; continue;
    }
    if (c === '}' || c === ']') { depth--; out += '\n' + pad() + c; i++; continue; }
    if (c === ',') { out += ',\n' + pad(); i++; continue; }
    if (c === ':') { out += ': '; i++; continue; }
    out += c; i++;
  }
  return out;
}

// ---------------- the session ----------------
export class Session {
  static readonly SCRATCH = '<session>';
  readonly entryPath: string | null;
  readonly log: Op[] = [];
  cursor = 0;
  lastTiming: Timing | null = null;
  private snapshot0: Map<string, string>;
  private state: State;

  /** texts that override the disk (the language server's open buffers), by absolute path */
  readonly overlay: Map<string, string>;

  constructor(entry?: string, overlay?: Map<string, string>) {
    this.entryPath = entry ? absPath(entry) : null;
    this.overlay = overlay ?? new Map();
    this.snapshot0 = this.snapshotFromDisk();
    this.state = this.initialState();
  }

  get entryAbs(): string { return this.entryPath ?? absPath(Session.SCRATCH); }
  get entryName(): string { return this.entryPath ? basename(this.entryPath) : Session.SCRATCH; }

  // the universe's texts as they are on disk now: the entry and every
  // module reachable from it (a module that cannot be read is absent and
  // reported on use, as the command line reports it)
  private snapshotFromDisk(): Map<string, string> {
    const snap = new Map<string, string>();
    if (!this.entryPath) return snap;
    const pkg = openPackageUniverse(this.entryPath);
    const { modules } = loadModules(this.entryPath, pkg?.resolver, this.overlay);
    for (const p of new Set([this.entryPath, ...modules.map(m => m.path)])) {
      if (this.overlay.has(p)) { snap.set(p, this.overlay.get(p)!); continue; }
      try { snap.set(p, readFileSync(p, 'utf8')); } catch { /* absent */ }
    }
    return snap;
  }
  private initialState(): State {
    return { snapshot: this.snapshot0, decls: new Map(), outputs: new Map(), documents: new Map() };
  }

  // ---- the log ----
  apply(op: Op): void {
    this.log.length = this.cursor;          // a new operation after :undo discards what was undone
    this.applyTo(this.state, op);           // a refused operation throws and is not logged
    this.log.push(op);
    this.cursor++;
  }
  undo(n = 1): number {
    const to = Math.max(0, this.cursor - n);
    const stepped = this.cursor - to;
    this.cursor = to; this.replay();
    return stepped;
  }
  redo(n = 1): number {
    const to = Math.min(this.log.length, this.cursor + n);
    const stepped = to - this.cursor;
    this.cursor = to; this.replay();
    return stepped;
  }
  private replay() {
    this.state = this.initialState();
    for (const op of this.log.slice(0, this.cursor)) this.applyTo(this.state, op);
  }
  reloadOp(): Op { return { op: 'reload', snapshot: this.snapshotFromDisk() }; }

  private applyTo(st: State, op: Op) {
    switch (op.op) {
      case 'bind': {
        const { modules } = this.build(st);
        if (!modules.some(m => m.env.inputs.has(op.name))) throw new SessionError(`no input named ${op.name}`);
        const doc = op.src.kind === 'expr' ? this.evalToDoc(st, op.src.text)
          : parseDoc(op.src.text, op.src.kind === 'file' ? op.src.file : 'the document');
        st.documents.set(op.name, { origin: op.src.kind, file: op.src.kind === 'file' ? op.src.file : undefined, doc, base: docClone(doc), edited: false });
        return;
      }
      case 'unbind':
        if (!st.documents.has(op.name)) throw new SessionError(`${op.name} is not bound`);
        st.documents.delete(op.name);
        return;
      case 'edit': this.edit(st, op); return;
      case 'declare':
        st.decls.delete(op.name); st.outputs.delete(op.name);
        st.decls.set(op.name, op.text);
        return;
      case 'output':
        st.decls.delete(op.name); st.outputs.delete(op.name);
        st.outputs.set(op.name, { type: op.type, expr: op.expr });
        return;
      case 'drop':
        if (!st.decls.delete(op.name) && !st.outputs.delete(op.name)) throw new SessionError(`no session declaration named ${op.name}`);
        return;
      case 'reload': st.snapshot = op.snapshot; return;
      case 'reset': st.decls.clear(); st.outputs.clear(); st.documents.clear(); return;
    }
  }

  // ---- documents and edits (§3) ----
  private evalToDoc(st: State, exprText: string): any {
    const expr = parseExpr(exprText);
    const r = this.run(st, 'lazy');
    if (!r.eng || !r.entry) throw new SessionError(this.loadFailure(r));
    const sc: any = { inst: null, locals: new Map(), rootName: '', menv: r.entry.env };
    try {
      let v = r.eng.ev(expr, sc);
      v = r.eng.materialize(v, ['_'], null, sc);
      r.eng.forceAll(v, true);
      const text = r.eng.serialize(v, '');
      if (text === undefined) throw new SessionError('the value is not data');
      return readJson(text);
    } catch (e: any) {
      if (e instanceof SessionError) throw e;
      if (e instanceof EvalErr) throw new SessionError(e.message);
      if (e instanceof Taint) throw new SessionError('the value is invalid');
      throw e;
    }
  }

  private edit(st: State, op: Extract<Op, { op: 'edit' }>) {
    let segs: Seg[];
    try { segs = parsePath(op.path, ''); } catch { throw new SessionError(`bad path ${op.path}`); }
    if (typeof segs[0] !== 'string' || segs[0] === '') throw new SessionError(`bad path ${op.path}`);
    const root = segs[0];
    if (segs.length < 2) throw new SessionError(`a path below a root is required, got ${op.path}`);
    const value = op.kind === 'remove' ? undefined : this.evalToDoc(st, op.expr!);
    const doc = this.documentOf(st, root);
    let parent = doc.doc;
    for (const s of segs.slice(1, -1)) {
      parent = docStep(parent, s);
      if (parent === undefined) throw new SessionError(`nothing at ${pathStr(segs.slice(0, segs.indexOf(s) + 1))}`);
    }
    const last = segs[segs.length - 1];
    const k = segText(last);
    if (parent && parent.__jobj) {
      const idx = parent.entries.findIndex(([kk]: any) => kk === k);
      if (op.kind === 'create') {
        if (idx >= 0) throw new SessionError(`${op.path} already holds a value`);
        parent.entries.push([String(k), value]);
      } else if (idx < 0) throw new SessionError(`nothing at ${op.path}`);
      else if (op.kind === 'update') parent.entries[idx][1] = value;
      else parent.entries.splice(idx, 1);
    } else if (Array.isArray(parent) && typeof k === 'number') {
      if (op.kind === 'create') {
        if (k < parent.length) throw new SessionError(`${op.path} already holds a value`);
        if (k > parent.length) throw new SessionError(`${op.path} is past the end of the array`);
        parent.push(value);
      } else if (k >= parent.length) throw new SessionError(`nothing at ${op.path}`);
      else if (op.kind === 'update') parent[k] = value;
      else parent.splice(k, 1);
    } else throw new SessionError(`${pathStr(segs.slice(0, -1))} is not a record, map, or array`);
    doc.edited = true;
  }

  // the document of a root, made if the root has none yet: an unbound
  // input's fallback, or an output detached into its settable projection
  private documentOf(st: State, root: string): Document {
    const have = st.documents.get(root);
    if (have) return have;
    const b = this.build(st);
    const inputMod = b.modules.find(m => m.env.inputs.has(root));
    const outputMod = b.modules.find(m => m.env.outputs.some(o => o.name === root));
    if (!inputMod && !outputMod) throw new SessionError(st.outputs.has(root) ? `${root} is a session output; edit the roots it reads` : `no root named ${root}`);
    const r = this.run(st, 'full');
    if (!r.eng || !r.entry) throw new SessionError(this.loadFailure(r));
    const v = r.entry.env.roots.get(root);
    if (v === undefined || r.diags.some(d => d.severity === 'error' && isRootDiag(d, root)))
      throw new SessionError(`${root} is invalid; fix it before editing`);
    const text = r.eng.serialize(v, root, true);
    const doc = readJson(text);
    const d: Document = { origin: inputMod ? 'fallback' : 'detached', doc, base: docClone(doc), edited: false };
    st.documents.set(root, d);
    return d;
  }

  // ---- building the universe ----
  private build(st: State): { modules: Module[]; entry: Module | null; diags: Diag[] } {
    const entryAbs = this.entryAbs;
    const overlay = new Map(st.snapshot);
    let text = st.snapshot.get(entryAbs) ?? (this.entryPath ? undefined : '');
    if (text !== undefined) {
      text = detachOutputs(text, [...st.documents.entries()].filter(([, d]) => d.origin === 'detached').map(([n]) => n));
      const extra = [...st.decls.values()];
      if (extra.length) text = text.replace(/\n?$/, '\n') + extra.join('\n') + '\n';
      overlay.set(entryAbs, text);
    }
    const pkg = this.entryPath ? openPackageUniverse(entryAbs) : null;
    const pre = pkg ? [...pkg.diags, ...verifyLock(pkg)] : [];
    const r = loadModules(entryAbs, pkg?.resolver, overlay);
    return { modules: r.modules, entry: r.entry, diags: [...pre, ...r.diags] };
  }

  private loadFailure(r: Run): string {
    const d = r.loadDiags[0];
    return d ? `${d.code ? `[${d.code}] ` : ''}${d.message}` : 'the universe did not load';
  }

  /** load, check, and (unless `mode` says otherwise) evaluate the state */
  run(st: State = this.state, mode: 'check' | 'lazy' | 'full' = 'full'): Run {
    const t0 = now();
    const b = this.build(st);
    const t1 = now();
    const out: Run = { modules: b.modules, entry: b.entry, loadDiags: b.diags, checks: [], sessionChecks: [], eng: null, diags: [],
      timing: { load: t1 - t0, check: 0, bind: 0, evaluate: 0, total: 0 } };
    const finish = () => { out.timing.total = now() - t0; this.lastTiming = out.timing; return out; };
    if (b.diags.length || !b.entry) return finish();
    const entry = b.entry;
    for (const m of b.modules)
      for (const d of checkModule(m.decls, m.env)) out.checks.push({ file: m.path, diag: d });
    // session outputs: their expressions are inferred where a declared
    // output's would be checked; the inferred type is the root's type
    const sessionRoots: { name: string; expr: Expr; rt: RT }[] = [];
    for (const [name, o] of st.outputs) {
      const taken = b.modules.some(m => m.env.inputs.has(name) || m.env.outputs.some(x => x.name === name));
      if (taken) { out.sessionChecks.push({ severity: 'error', code: 'E3018', message: `root ${name} is already declared by the universe`, path: name }); continue; }
      let expr: Expr, rt: RT;
      try {
        expr = parseExpr(o.expr);
        const before = out.sessionChecks.length;
        const cx = this.sessionCtx(st, entry.env, (code, msg) => out.sessionChecks.push({ severity: 'error', code, message: msg, path: name }), name);
        const ty = infer(cx, expr);
        if (out.sessionChecks.length > before) continue;
        if (o.type) {
          const t = parseDecl(`output ${name}: ${o.type} = 0`).decl as any;
          rt = entry.env.resolve(t.type);
        } else rt = ty.rt ?? { t: 'any' };
      } catch (e: any) {
        out.sessionChecks.push({ severity: 'error', message: e.message, path: name });
        continue;
      }
      sessionRoots.push({ name, expr, rt });
    }
    out.timing.check = now() - t1;
    // a static error in a module stops full evaluation as it stops `decl
    // evaluate`; a session output that does not check is left out, and a
    // bare expression (lazy) evaluates over what loaded regardless
    if (mode === 'check' || (mode === 'full' && out.checks.some(c => c.diag.severity === 'error'))) return finish();

    const t2 = now();
    const eng = new Engine(entry.env);
    for (const m of b.modules) {
      m.env.constEval = (n: string) => eng.forceConstIn(m.env, n, '');
      m.env.exprEval = (e: any) => eng.ev(e, { inst: null, locals: new Map(), rootName: '', menv: m.env } as any);
    }
    // documents first (an output may read an input, §5.5), then the
    // modules' outputs, then the session's
    for (const [name, d] of st.documents) {
      const m = b.modules.find(x => x.env.inputs.has(name)) ?? entry;
      const rt = m.env.resolve(m.env.inputs.get(name)!.type);
      const sc: any = { inst: null, locals: new Map(), rootName: name, menv: m.env };
      eng.bindRoot(name, d.doc, rt, sc, false);
    }
    for (const m of b.modules) for (const o of m.env.outputs) {
      const sc: any = { inst: null, locals: new Map(), rootName: o.name, menv: m.env };
      eng.bindRoot(o.name, o.expr, m.env.resolve(o.type), sc, true);
    }
    for (const s of sessionRoots) {
      const sc: any = { inst: null, locals: new Map(), rootName: s.name, menv: entry.env };
      eng.bindRoot(s.name, s.expr, s.rt, sc, true);
    }
    out.eng = eng;
    out.timing.bind = now() - t2;
    if (mode === 'lazy') { out.diags = entry.env.diagnostics; return finish(); }
    const t3 = now();
    for (const v of entry.env.roots.values()) eng.forceAll(v, false);
    eng.phase = 2;
    for (let i = 0; i < eng.deferredSlots.length; i++) eng.forceSlotSafe(eng.deferredSlots[i].inst, eng.deferredSlots[i].name);
    for (const v of entry.env.roots.values()) eng.forceAll(v, true);
    eng.validateAll('');
    out.diags = entry.env.diagnostics;
    out.timing.evaluate = now() - t3;
    return finish();
  }

  // an inference context over the entry's scope in which the session's
  // outputs are variables of their inferred types, in declaration order
  private sessionCtx(st: State, env: Env, report: (code: string, msg: string) => void, upTo?: string) {
    const cx = makeCtx(env, report);
    for (const [name, o] of st.outputs) {
      if (name === upTo) break;
      try {
        const expr = parseExpr(o.expr);
        const quiet = makeCtx(env, () => {}); quiet.vars = new Map(cx.vars);
        let rt: RT | null = infer(quiet, expr).rt;
        if (o.type) { const t = parseDecl(`output ${name}: ${o.type} = 0`).decl as any; rt = env.resolve(t.type); }
        cx.vars.set(name, { rt, abs: false });
      } catch { /* a session output that does not parse is not in scope */ }
    }
    return cx;
  }

  // ---- questions ----
  /** partial evaluation of one expression (§2.1) */
  evaluateExpr(text: string): { value: string | null; diags: Diag[]; error?: { code?: string; message: string } } {
    const expr = parseExpr(text);
    const r = this.run(this.state, 'lazy');
    if (!r.eng || !r.entry) return { value: null, diags: r.loadDiags, error: { message: '' } };
    const sc: any = { inst: null, locals: new Map(), rootName: '', menv: r.entry.env };
    // binding the roots may already have reported (a root whose top-level
    // expression fails); the expression's own diagnostics are the ones
    // that arise from here on, plus the failed roots it names
    const all = r.entry.env.diagnostics;
    const from = all.length;
    const named = new Set((text.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? []));
    const arising = () => [...all.slice(0, from).filter(d => named.has(d.path)), ...all.slice(from)];
    try {
      let v = r.eng.ev(expr, sc);
      v = r.eng.materialize(v, ['_'], null, sc);
      r.eng.phase = 2;
      r.eng.forceAll(v, true);
      return { value: this.valueText(r.eng, v), diags: arising() };
    } catch (e: any) {
      if (e instanceof EvalErr) return { value: null, diags: arising(), error: { code: e.code, message: e.message } };
      if (e instanceof Taint) return { value: null, diags: arising(), error: { message: '' } };
      throw e;
    }
  }
  private valueText(eng: Engine, v: any): string {
    if (v === ABSENT) return 'absent';
    if (v === undefined) return 'absent';
    if (isClo(v) || (v && (v.__nat || v.__std))) return '<function>';
    if (v && v.__nsref) return '<namespace>';
    if (v && v.__pat) return `/${v.re}/`;
    return eng.serialize(v, '');
  }

  /** the roots of the universe and of the session (`:roots`) */
  roots(): RootInfo[] {
    const b = this.build(this.state);
    const out: RootInfo[] = [];
    const rel = (p: string) => p === this.entryAbs ? this.entryName : relative(dirname(this.entryAbs), p);
    for (const m of b.modules) {
      // the module's roots in declaration order, from its text as loaded
      // (a detached output is blanked from the universe but still a root)
      const decls = m.path === this.entryAbs ? parseSource(this.state.snapshot.get(m.path) ?? '').decls : m.decls;
      for (const decl of decls) {
        if (decl.d === 'output') {
          const d = this.state.documents.get(decl.name);
          out.push({ kind: 'output', name: decl.name, module: rel(m.path), exported: !!decl.exported, session: false,
            binding: d?.origin === 'detached' ? 'detached' : '', detail: '', edited: !!d?.edited });
        } else if (decl.d === 'input') {
          const d = this.state.documents.get(decl.name);
          const binding = d ? (d.origin === 'fallback' ? 'fallback' : 'bound') : decl.fallback ? 'fallback' : 'unbound';
          const detail = d ? (d.origin === 'file' ? d.file! : d.origin === 'inline' ? '(inline)' : d.origin === 'expr' ? '(expression)' : '') : '';
          out.push({ kind: 'input', name: decl.name, module: rel(m.path), exported: false, session: false, binding, detail, edited: !!d?.edited });
        }
      }
    }
    for (const name of this.state.outputs.keys())
      out.push({ kind: 'output', name, module: '', exported: false, session: true, binding: '', detail: '', edited: false });
    return out;
  }
  allRootNames(): string[] { return this.roots().map(r => r.name); }
  hasRoot(name: string): boolean { return this.allRootNames().includes(name); }

  /** static diagnostics of every module, with the file each is reported against */
  check(): { file: string; diag: Diag }[] {
    const r = this.run(this.state, 'check');
    return [...r.loadDiags.map(d => ({ file: this.entryAbs, diag: d })), ...r.checks, ...r.sessionChecks.map(d => ({ file: this.entryAbs, diag: d }))];
  }

  /** full evaluation of the named roots (`:evaluate`), or of the exported outputs */
  evaluate(names: string[]): { run: Run; docs: { name: string; json: string | null }[]; exported: boolean } {
    const r = this.run(this.state, 'full');
    const docs: { name: string; json: string | null }[] = [];
    if (!r.entry) return { run: r, docs, exported: names.length === 0 };
    const want = names.length ? names : r.entry.decls.filter(d => d.d === 'output' && d.exported).map(d => (d as any).name as string);
    for (const n of names) if (!this.hasRoot(n)) throw new SessionError(`no root named ${n}`);
    if (!r.eng) return { run: r, docs: want.map(name => ({ name, json: null })), exported: names.length === 0 };
    for (const name of want) {
      const v = r.entry.env.roots.get(name);
      const bad = v === undefined || r.diags.some(d => d.severity === 'error' && isRootDiag(d, name));
      docs.push({ name, json: bad ? null : r.eng.serialize(v, name) });
    }
    return { run: r, docs, exported: names.length === 0 };
  }

  /** whole-document validation of the named roots (`:validate`), or of every root */
  validate(names: string[]): { run: Run; verdicts: { name: string; errors: number; warnings: number }[]; diags: Diag[] } {
    for (const n of names) if (!this.hasRoot(n)) throw new SessionError(`no root named ${n}`);
    const r = this.run(this.state, 'full');
    const want = names.length ? names : r.entry ? [...r.entry.env.roots.keys()] : [];
    const diags = r.diags.filter(d => want.some(n => isRootDiag(d, n)) || (!d.path && names.length === 0));
    const verdicts = want.map(name => ({
      name,
      errors: r.diags.filter(d => d.severity === 'error' && isRootDiag(d, name)).length + (r.entry?.env.roots.has(name) ? 0 : r.eng ? 1 : 0),
      warnings: r.diags.filter(d => d.severity === 'warning' && isRootDiag(d, name)).length,
    }));
    return { run: r, verdicts, diags };
  }

  /** the static type of an expression (`:type`) */
  typeOf(text: string): { type: string; maybeAbsent: boolean; diags: Diag[] } {
    const expr = parseExpr(text);
    const b = this.build(this.state);
    if (!b.entry) throw new SessionError(b.diags[0]?.message ?? 'the universe did not load');
    const diags: Diag[] = [];
    const cx = this.sessionCtx(this.state, b.entry.env, (code, message) => diags.push({ severity: 'error', code, message, path: '' }));
    const ty = infer(cx, expr);
    return { type: typeText(ty.rt), maybeAbsent: ty.abs, diags };
  }

  /** the canonical path of the place a navigation names (`:path`) */
  pathOf(text: string): string {
    const expr = parseExpr(text);
    const r = this.run(this.state, 'lazy');
    if (!r.eng || !r.entry) throw new SessionError(this.loadFailure(r));
    const sc: any = { inst: null, locals: new Map(), rootName: '', menv: r.entry.env };
    try {
      let segs = r.eng.evalPlace(expr, sc);
      // a scalar member or element is a place too: its container's place, one step down
      if (!segs && (expr.e === 'member' || expr.e === 'index')) {
        const base = r.eng.evalPlace((expr as any).x, sc);
        if (base) {
          const step = expr.e === 'member' ? expr.name : (() => { const i = r.eng!.ev((expr as any).i, sc); return typeof i === 'bigint' ? Number(i) : mapKey(i); })();
          segs = [...base, step];
        }
      }
      if (!segs && expr.e === 'name' && r.entry.env.roots.has(expr.name)) segs = [expr.name];
      if (!segs) throw new SessionError('the expression does not name a place');
      return pathStr(segs);
    } catch (e: any) {
      if (e instanceof EvalErr) throw new SessionError(e.message);
      if (e instanceof Taint) throw new SessionError('the place is invalid');
      throw e;
    }
  }

  /** the declaration a name resolves to, with its documentation (`:doc`) */
  docOf(name: string): string[] {
    const [head, member] = name.split('.');
    // a session declaration first
    if (!member && this.state.decls.has(head)) return this.state.decls.get(head)!.split('\n');
    if (!member && this.state.outputs.has(head)) { const o = this.state.outputs.get(head)!; return [`${head}${o.type ? `: ${o.type}` : ''} = ${o.expr}`]; }
    const b = this.build(this.state);
    if (!b.entry) throw new SessionError(b.diags[0]?.message ?? 'the universe did not load');
    let mod: Module | null = b.entry, target = head;
    if (!b.entry.decls.some(d => (d as any).name === head)) {
      const im = b.entry.env.imports.get(head);
      if (im) { mod = b.modules.find(m => m.env === im.env) ?? null; target = im.name; }
      else mod = null;
    }
    const decl = mod?.decls.find(d => (d as any).name === target && d.loc);
    if (!mod || !decl) throw new SessionError(`no declaration named ${head}`);
    const text = this.state.snapshot.get(mod.path) ?? '';
    const lines = text.split('\n');
    const loc = decl.loc!;
    let from = loc.sl;
    const docLines: string[] = [];
    while (from > 0 && /^\s*\/\/\//.test(lines[from - 1])) { from--; docLines.unshift(lines[from]); }
    const body = lines.slice(loc.sl, loc.el + 1);
    if (member) {
      const re = new RegExp(`^\\s*(?:///.*|${member}\\$?\\??\\s*[:=].*)$`);
      const picked: string[] = [];
      body.forEach((l, i) => { if (new RegExp(`^\\s*${member}\\$?\\??\\s*[:=]`).test(l)) { let j = i; const ds: string[] = []; while (j > 0 && /^\s*\/\/\//.test(body[j - 1])) { j--; ds.unshift(body[j].trim()); } picked.push(...ds, l.trim()); } });
      void re;
      if (!picked.length) throw new SessionError(`${head} has no member ${member}`);
      return picked;
    }
    return [...docLines, ...body];
  }

  /** the derivation of a valid place, or the root cause of an invalid one (`:trace`) */
  trace(pathText: string): string[] {
    let segs: Seg[];
    try { segs = parsePath(pathText, ''); } catch { throw new SessionError(`bad path ${pathText}`); }
    const root = segs[0] as string;
    if (!this.hasRoot(root)) throw new SessionError(`no root named ${root}`);
    const r = this.run(this.state, 'full');
    if (!r.eng || !r.entry) throw new SessionError(this.loadFailure(r));
    const eng = r.eng;
    const lines: string[] = [];
    const seen = new Set<string>();
    const short = (v: any) => { const t = this.valueText(eng, v); return t.length > 60 ? t.slice(0, 57) + '...' : t; };
    const walk = (segs: Seg[], depth: number) => {
      const path = pathStr(segs);
      const ind = '  '.repeat(depth);
      if (seen.has(path)) { lines.push(`${ind}${path}  (above)`); return; }
      seen.add(path);
      const own = r.diags.filter(d => d.path === path);
      const parent = segs.length > 1 ? this.valueAt(eng, r.entry!, segs.slice(0, -1)) : null;
      const last = segs[segs.length - 1];
      const slot = parent && isRec(parent) && typeof last === 'string' ? parent.slots.get(last) : undefined;
      if (slot) {
        const kind = slot.kind === 'der' ? 'derived' : slot.kind === 'dflt' ? 'defaulted' : slot.kind === 'opt' ? 'optional' : 'required';
        const m = parent.rt.members.find((x: any) => x.name === last);
        const supplied = slot.kind === 'req' || slot.kind === 'opt' || (slot.kind === 'dflt' && parent.entryOrder.includes(last));
        if (slot.state === 'invalid') {
          lines.push(`${ind}${path}  (invalid)`);
          for (const d of own) lines.push(`${ind}  ${fmtDiag(d)}`);
          if (!own.length && m?.expr) for (const rd of readsOf(m.expr)) { const s = this.readSegs(eng, parent, rd, r.entry!); if (s) walk(s, depth + 1); }
          return;
        }
        if (slot.state === 'absent') { lines.push(`${ind}${path}  absent`); return; }
        lines.push(`${ind}${path} = ${short(slot.value)}  (${supplied ? 'supplied' : kind}${m?.expr && !supplied ? `: ${exprText(m.expr)}` : ''})`);
        if (!supplied && m?.expr && depth < 6) for (const rd of readsOf(m.expr)) { const s = this.readSegs(eng, parent, rd, r.entry!); if (s) walk(s, depth + 1); else lines.push(`${ind}  ${exprText(rd)}  (not a place)`); }
        return;
      }
      const v = this.valueAt(eng, r.entry!, segs);
      if (v === undefined) {
        if (r.diags.some(d => d.severity === 'error' && isRootDiag(d, path))) {
          lines.push(`${ind}${path}  (invalid)`);
          for (const d of r.diags.filter(d => isRootDiag(d, path))) lines.push(`${ind}  ${fmtDiag(d)}`);
        } else lines.push(`${ind}${path}  nothing there`);
        return;
      }
      lines.push(`${ind}${path} = ${short(v)}  (${segs.length === 1 ? (this.state.documents.has(root) ? 'document' : 'root literal') : 'supplied'})`);
      for (const d of own) lines.push(`${ind}  ${fmtDiag(d)}`);
    };
    walk(segs, 0);
    return lines;
  }
  private valueAt(eng: Engine, entry: Module, segs: Seg[]): any {
    try {
      let v: any = entry.env.roots.get(segs[0] as string);
      for (const s of segs.slice(1)) {
        v = eng.deref(v);
        if (isRec(v)) { const st = eng.forceState(v, s as string); v = st === 'ok' ? v.slots.get(s as string)!.value : undefined; }
        else if (isArr(v)) v = v.items[s as number];
        else if (isMap(v)) v = v.entries.get(segText(s));
        else return undefined;
        if (v === undefined || v === ABSENT) return undefined;
      }
      return v;
    } catch { return undefined; }
  }
  private readSegs(eng: Engine, inst: any, rd: Expr, entry: Module): Seg[] | null {
    // a bare name read inside a record is a sibling member (§4.4's scope
    // chain), else a root; a chain is navigated to the place it names
    const sibling = (n: string): any => { let i = inst; while (i) { if (i.slots.has(n)) return i; i = i.parent; } return null; };
    if (rd.e === 'name') {
      const owner = sibling(rd.name);
      if (owner) return [...owner.path, rd.name];
      return entry.env.roots.has(rd.name) ? [rd.name] : null;
    }
    const sc: any = { inst, locals: new Map(), rootName: inst.path[0], menv: entry.env };
    try { return eng.evalPlace(rd, sc); } catch { return null; }
  }

  /** the candidates completion offers at the end of `text` (`:complete`) */
  complete(text: string, commands: string[]): string[] {
    const uniq = (xs: string[]) => [...new Set(xs)].sort();
    if (text.startsWith(':')) {
      const sp = text.indexOf(' ');
      if (sp < 0) return uniq(commands.filter(c => c.startsWith(text)));
      const cmd = text.slice(0, sp), rest = text.slice(sp + 1);
      const last = rest.split(/[\s,=]+/).pop() ?? '';
      const roots = () => this.allRootNames();
      const by = (xs: string[]) => uniq(xs.filter(x => x.startsWith(last)));
      switch (cmd) {
        case ':save': case ':bind': return rest.includes('=') ? completeFile(rest.slice(rest.indexOf('=') + 1)) : by(roots());
        case ':evaluate': case ':validate': case ':unbind': case ':diff': return by(roots());
        case ':drop': return by([...this.state.decls.keys(), ...this.state.outputs.keys()]);
        case ':set': return by(['pretty', 'compact']);
        case ':help': return by(commands);
        case ':trace': case ':path': case ':create': case ':update': case ':remove': return this.completePath(last);
        case ':load': case ':write': case ':history': return completeFile(rest);
        default: return [];
      }
    }
    const m = /([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_][A-Za-z0-9_$]*|\[[^\]]*\])*)\.([A-Za-z_]*)$/.exec(text);
    if (m) {
      const prefix = m[2];
      if (m[1] === 'std' || m[1].startsWith('std.')) {
        const ns = m[1] === 'std' ? '' : m[1].slice(4) + '.';
        return uniq(Object.keys(STD).filter(k => k.startsWith(ns)).map(k => k.slice(ns.length).split('.')[0]).filter(k => k.startsWith(prefix)));
      }
      let rt: RT | null = null;
      try {
        const b = this.build(this.state);
        if (b.entry) { const cx = this.sessionCtx(this.state, b.entry.env, () => {}); rt = infer(cx, parseExpr(m[1])).rt; }
      } catch { return []; }
      const members = (t: RT | null): any[] | null => {
        if (!t) return null;
        if (t.t === 'rec') return t.members;
        if (t.t === 'union') { const sets = t.arms.map(members); if (sets.some((s: any) => !s)) return null; return sets[0].filter((m: any) => sets.every((s: any) => s.some((x: any) => x.name === m.name))); }
        if (t.t === 'pred') return members(t.base);
        return null;
      };
      const ms = members(rt) ?? [];
      return uniq(ms.filter((x: any) => x.name.startsWith(prefix)).map((x: any) => `${x.name}${x.hidden ? '$' : ''}  ${x.kind === 'der' ? 'derived' : x.kind === 'dflt' ? 'defaulted' : x.kind === 'opt' ? 'optional' : 'required'}${x.type ? `: ${typeText(x.type)}` : ''}`));
    }
    const w = /([A-Za-z_$][A-Za-z0-9_$]*)$/.exec(text);
    const prefix = w ? w[1] : '';
    if (prefix.startsWith('$')) return uniq(['$this', '$parent', '$root', '$key', '$path', '$referrers'].filter(x => x.startsWith(prefix)));
    const names: string[] = ['std'];
    const b = this.build(this.state);
    if (b.entry) {
      const e = b.entry.env;
      names.push(...e.typeAsts.keys(), ...e.consts.keys(), ...e.funcs.keys(), ...e.inputs.keys(), ...e.outputs.map(o => o.name), ...e.imports.keys(), ...e.namespaces.keys(), ...e.diags.keys());
    }
    names.push(...this.state.outputs.keys());
    const kw = ['if', 'then', 'else', 'for', 'in', 'match', 'with', 'matches', 'true', 'false', 'null'];
    return uniq([...names, ...kw].filter(n => n.startsWith(prefix)));
  }
  private completePath(partial: string): string[] {
    const m = /^(.*?)(?:\.([A-Za-z_][A-Za-z0-9_]*)?|\[("?)([^\]]*)?)?$/.exec(partial);
    const base = m ? m[1] : partial;
    if (!m || (!partial.includes('.') && !partial.includes('['))) return this.allRootNames().filter(n => n.startsWith(partial)).sort();
    const r = this.run(this.state, 'full');
    if (!r.eng || !r.entry) return [];
    let segs: Seg[];
    try { segs = parsePath(base, ''); } catch { return []; }
    const v = r.eng.deref(this.valueAt(r.eng, r.entry, segs));
    const out: string[] = [];
    if (isRec(v)) for (const [n, s] of v.slots) { if (s.hidden) continue; out.push(`${base}.${n}`); }
    if (isMap(v)) for (const k of v.entries.keys()) out.push(`${base}[${JSON.stringify(k)}]`);
    if (isArr(v)) v.items.forEach((_: any, i: number) => out.push(`${base}[${i}]`));
    return out.filter(x => x.startsWith(partial)).sort();
  }

  // ---- the scratch module (§4) ----
  scratchText(): string {
    const parts: string[] = [];
    for (const t of this.state.decls.values()) parts.push(t.trim());
    for (const [n, o] of this.state.outputs) parts.push(`output ${n}: ${o.type ?? this.inferredTypeText(o.expr)} = ${o.expr}`);
    return parts.length ? parts.join('\n') + '\n' : '';
  }
  private inferredTypeText(expr: string): string {
    try { return this.typeOf(expr).type; } catch { return 'any'; }
  }
  /** the scratch module as a file: imports of the entry's exports it uses, then the declarations */
  moduleText(): string {
    const body = this.scratchText();
    const b = this.build(this.state);
    const used = new Set(body.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? []);
    const names = b.entry ? [...b.entry.exports.keys()].filter(n => used.has(n)).sort() : [];
    const header = names.length && this.entryPath ? `import { ${names.join(', ')} } from "./${basename(this.entryPath)}"\n\n` : '';
    return header + body;
  }
  fmt(): string { const t = this.scratchText(); return t ? format(t) : ''; }
  write(file: string): void {
    try { writeFileSync(file, this.moduleText()); } catch { throw new SessionError(`cannot write ${file}`); }
  }

  // ---- documents out (§3) ----
  documentText(name: string): string {
    const d = this.state.documents.get(name);
    if (d) return docJson(d.doc);
    if (!this.hasRoot(name)) throw new SessionError(`no root named ${name}`);
    const { docs } = this.evaluate([name]);
    if (docs[0].json === null) throw new SessionError(`${name} is invalid`);
    return docs[0].json;
  }
  save(name: string, file: string): void {
    const text = this.documentText(name);
    try { writeFileSync(file, text + '\n'); } catch { throw new SessionError(`cannot write ${file}`); }
  }
  diff(name: string): string[] {
    const d = this.state.documents.get(name);
    if (!d) throw new SessionError(this.hasRoot(name) ? `${name} holds no document` : `no root named ${name}`);
    return lineDiff(prettyJson(docJson(d.base)).split('\n'), prettyJson(docJson(d.doc)).split('\n'));
  }

  // ---- introspection ----
  sessionLines(): string[] {
    const out: string[] = [];
    for (const [n, t] of this.state.decls) out.push(`declaration  ${n.padEnd(16)} ${t.trim().split('\n')[0]}`);
    for (const [n, o] of this.state.outputs) out.push(`output       ${n.padEnd(16)} ${n}${o.type ? `: ${o.type}` : ''} = ${o.expr}`);
    for (const [n, d] of this.state.documents) out.push(`document     ${n.padEnd(16)} ${d.origin}${d.file ? ` ${d.file}` : ''}${d.edited ? ' (edited)' : ''}`);
    return out;
  }
  historyLines(): string[] {
    const out = [`${this.cursor === 0 ? '*' : ' '} 0  (start)`];
    this.log.forEach((op, i) => out.push(`${this.cursor === i + 1 ? '*' : ' '} ${i + 1}  ${opText(op)}`));
    return out;
  }
  scriptLines(): string[] { return this.log.slice(0, this.cursor).map(opText); }
}

// ---------------- helpers ----------------
export function fmtDiag(d: Diag, inFile?: string): string {
  return `${d.severity}${d.code ? ` [${d.code}]` : ''}${d.id ? ` ${d.id}` : ''}${d.path ? ` at ${d.path}` : ''}: ${d.message}${inFile ? ` (in ${inFile})` : ''}`;
}

export function opText(op: Op): string {
  switch (op.op) {
    case 'bind': return op.src.kind === 'file' ? `:bind ${op.name}=${op.src.file}` : op.src.kind === 'inline' ? `:bind ${op.name} ${docJson(readJson(op.src.text))}` : `:bind ${op.name} = ${op.src.text.trim()}`;
    case 'unbind': return `:unbind ${op.name}`;
    case 'edit': return `:${op.kind} ${op.path}${op.expr !== undefined ? ` = ${op.expr.trim()}` : ''}`;
    case 'declare': return op.text.trim();
    case 'output': return `${op.name}${op.type ? `: ${op.type}` : ''} = ${op.expr.trim()}`;
    case 'drop': return `:drop ${op.name}`;
    case 'reload': return ':reload';
    case 'reset': return ':reset';
  }
}

// a detached output (§3): its declaration becomes `input name: T` in the
// session's copy of the module — the name stays declared, the checker
// sees a root of the same type, and the session binds the projected
// document to it; line numbers are kept
function detachOutputs(text: string, names: string[]): string {
  if (!names.length) return text;
  const { decls } = parseSource(text);
  const lines = text.split('\n');
  for (const d of decls) {
    if (d.d !== 'output' || !names.includes(d.name) || !d.loc) continue;
    const src = lines.slice(d.loc.sl, d.loc.el + 1).join('\n');
    const colon = src.indexOf(':', src.indexOf(d.name));
    // the type text: from the colon to the `=` at bracket depth 0
    let depth = 0, eq = -1;
    for (let i = colon + 1; i < src.length; i++) {
      const c = src[i];
      if ('{[(<'.includes(c)) depth++;
      else if ('}])>'.includes(c)) depth--;
      else if (c === '=' && depth === 0 && src[i + 1] !== '=' && src[i - 1] !== '!' && src[i - 1] !== '<' && src[i - 1] !== '>') { eq = i; break; }
    }
    const typeText = eq < 0 ? src.slice(colon + 1).trim() : src.slice(colon + 1, eq).trim();
    lines[d.loc.sl] = `input ${d.name}: ${typeText.replace(/\s*\n\s*/g, ' ')}`;
    for (let i = d.loc.sl + 1; i <= d.loc.el; i++) lines[i] = '';
  }
  return lines.join('\n');
}

// the places an expression reads, as navigation chains (a static
// approximation of the engine's read set: names, members, indexes)
function readsOf(e: Expr): Expr[] {
  const out: Expr[] = [];
  const isChain = (x: any): boolean => x.e === 'name' || x.e === 'ctx' || ((x.e === 'member' || x.e === 'index') && isChain(x.x));
  const go = (x: any) => {
    if (!x || typeof x !== 'object') return;
    if (isChain(x) && (x.e === 'member' || x.e === 'index')) { out.push(x); if (x.e === 'index') go(x.i); return; }
    if (x.e === 'name') { out.push(x); return; }
    for (const v of Object.values(x)) {
      if (Array.isArray(v)) v.forEach(go);
      else if (v && typeof v === 'object' && ('e' in v || 'k' in v)) go(v);
      else if (v && typeof v === 'object') go(v);
    }
  };
  go(e);
  return out.filter(x => x.e !== 'name' || !['true', 'false', 'null'].includes((x as any).name));
}

/** an expression's text, for chains and simple forms (the trace view) */
export function exprText(e: any): string {
  switch (e.e) {
    case 'lit': return typeof e.v === 'bigint' ? e.v.toString() : typeof e.v === 'string' ? JSON.stringify(e.v) : String(e.v);
    case 'unitlit': return `${e.num}${e.unit}`;
    case 'name': return e.name;
    case 'ctx': return e.name;
    case 'member': return `${exprText(e.x)}${e.safe ? '?.' : '.'}${e.name}`;
    case 'index': return `${exprText(e.x)}[${exprText(e.i)}]`;
    case 'paren': return `(${exprText(e.x)})`;
    case 'bin': return `${exprText(e.l)} ${e.op} ${exprText(e.r)}`;
    case 'un': return `${e.op}${exprText(e.x)}`;
    case 'call': return `${exprText(e.fn)}(${e.args.map(exprText).join(', ')})`;
    case 'if': return `if ${exprText(e.c)} then ${exprText(e.t)} else ${exprText(e.f)}`;
    case 'referrers': return `$referrers(${e.type}, ${JSON.stringify(e.member)})`;
    case 'template': return '`' + e.parts.map((p: any) => typeof p === 'string' ? p : '${' + exprText(p) + '}').join('') + '`';
    case 'obj': return `{ ${e.entries.map((en: any) => `${en.key}: ${exprText(en.val)}`).join(', ')} }`;
    case 'arr': return `[${e.items.map((it: any) => (it.spread ? '...' : '') + exprText(it.expr)).join(', ')}]`;
    case 'comp': return `[for ${e.clauses.map((c: any) => `${c.v} in ${exprText(c.iter)}`).join(', ')} … ]`;
    case 'lambda': return `(${e.params.join(', ')}) => …`;
    case 'with': return `${exprText(e.base)} with …`;
    case 'match': return `match ${exprText(e.subject)} { … }`;
    default: return '…';
  }
}

// files of the working directory for a file argument: `.decl` and `.json` first
function completeFile(partial: string): string[] {
  const slash = partial.lastIndexOf('/');
  const dir = slash >= 0 ? partial.slice(0, slash + 1) : '';
  const base = slash >= 0 ? partial.slice(slash + 1) : partial;
  let names: string[];
  try { names = readdirSync(dir || '.'); } catch { return []; }
  const isDir = (n: string) => { try { return statSync(dir + n).isDirectory(); } catch { return false; } };
  const rank = (n: string) => /\.decl$/.test(n) ? 0 : /\.json$/.test(n) ? 1 : isDir(n) ? 2 : 3;
  return names.filter(n => n.startsWith(base) && !n.startsWith('.'))
    .sort((a, b) => rank(a) - rank(b) || (a < b ? -1 : a > b ? 1 : 0))
    .map(n => dir + n + (isDir(n) ? '/' : ''));
}

// a minimal line diff (longest common subsequence)
function lineDiff(a: string[], b: string[]): string[] {
  const n = a.length, m = b.length;
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--)
    dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const out: string[] = [];
  let i = 0, j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) { out.push(`  ${a[i]}`); i++; j++; }
    else if (dp[i + 1][j] >= dp[i][j + 1]) { out.push(`- ${a[i]}`); i++; }
    else { out.push(`+ ${b[j]}`); j++; }
  }
  while (i < n) out.push(`- ${a[i++]}`);
  while (j < m) out.push(`+ ${b[j++]}`);
  return out;
}

export { typeText, isQ, isRef, Env };
