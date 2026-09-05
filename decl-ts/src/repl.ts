// The REPL (docs/tooling/02_repl.md): an interactive session over a
// universe — expressions evaluated partially, session outputs and
// declarations, documents bound and edited with exact undo, and the
// command-line verbs root for root. Everything it prints goes to
// standard output; a scripted session (`--script`) prints the transcript
// the terminal would show, so the three implementations can be diffed.
import { readFileSync, writeFileSync, appendFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { Session, SessionError, fmtDiag, prettyJson } from './session.ts';
import type { Op } from './session.ts';
import { parseDecl, parseExpr } from './session.ts';

export const COMMANDS: [string, string, string][] = [
  // the universe
  [':load file.decl', 'open the universe from an entry module (a new session)', 'universe'],
  [':reload', 're-read every module of the universe from disk', 'universe'],
  [':roots', 'the roots of the universe and of the session', 'universe'],
  // documents
  [':bind name=doc.json', 'bind a JSON file to an input', 'documents'],
  [':bind name { … }', 'bind an inline JSON document', 'documents'],
  [':bind name = expr', 'bind the value of an expression as the document', 'documents'],
  [':unbind name', 'drop the binding', 'documents'],
  [':create path = expr', 'add a member, entry, or element at a path of a document', 'documents'],
  [':update path = expr', 'replace the value at a path of a document', 'documents'],
  [':remove path', 'remove the value at a path of a document', 'documents'],
  [':diff name', 'the document against what it started from', 'documents'],
  [':save name=file', 'write the document of a root to a file', 'documents'],
  // session declarations
  [':drop name', 'remove a session declaration', 'declarations'],
  [':write file.decl', 'write the scratch module to a file', 'declarations'],
  [':session', 'the session\'s declarations and documents', 'declarations'],
  [':reset', 'drop every binding, edit, and declaration', 'declarations'],
  // evaluation and validation
  [':check', 'static diagnostics of every module', 'evaluation'],
  [':evaluate [root…]', 'full evaluation: the documents of the roots', 'evaluation'],
  [':validate [root…]', 'full validation: every diagnostic, then a verdict per root', 'evaluation'],
  [':fmt', 'the scratch module, canonically formatted', 'evaluation'],
  // inspection
  [':type expr', 'the static type of an expression', 'inspection'],
  [':doc name', 'a declaration and its documentation', 'inspection'],
  [':path expr', 'the canonical path of a place', 'inspection'],
  [':trace path', 'the derivation of a place, or its root cause', 'inspection'],
  [':complete text', 'the completions offered at the end of the text', 'inspection'],
  // history
  [':undo [n]', 'step the log back', 'history'],
  [':redo [n]', 'step forward again', 'history'],
  [':history [file]', 'the log, or write it as a session file', 'history'],
  // the session
  [':time', 'wall time of the last evaluation', 'session'],
  [':set pretty|compact', 'value printing', 'session'],
  [':help [command]', 'these commands', 'session'],
  [':quit', 'end the session', 'session'],
];
const COMMAND_NAMES = [...new Set(COMMANDS.map(c => c[0].split(' ')[0]))];

const DECL_HEAD = /^\s*(?:export\s+)?(type|const|func|output|input|diagnostic|dimension|unit|import)\b/;
const OUTPUT_HEAD = /^\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?::\s*([^=]+?))?\s*=(?!=)\s*([\s\S]+)$/;
const KEYWORDS = new Set(['if', 'then', 'else', 'for', 'in', 'match', 'with', 'matches', 'true', 'false', 'null', 'export']);

/** does the input so far leave an expression open (§2.9)? */
export function needsMore(text: string): boolean {
  let depth = 0, inStr: string | null = null;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inStr) { if (c === '\\') i++; else if (c === inStr) inStr = null; continue; }
    if (c === '"' || c === '`') inStr = c;
    else if (c === '/' && text[i + 1] === '/') { const nl = text.indexOf('\n', i); if (nl < 0) break; i = nl; }
    else if ('{[('.includes(c)) depth++;
    else if ('}])'.includes(c)) depth--;
  }
  if (depth > 0 || inStr === '`') return true;
  if (text.trimStart().startsWith(':')) return false;      // a command: only an open bracket continues it
  const tail = text.replace(/\/\/[^\n]*$/, '').trimEnd();
  return /(?:[+\-*/%<>=!&|?:,]|\bthen|\belse|\bin|\bwith|=>)$/.test(tail);
}

export class Repl {
  session: Session;
  compact = false;
  errors = 0;
  private out: (line: string) => void;
  private buffer: string[] = [];
  quitRequested = false;

  constructor(out: (line: string) => void, entry?: string) {
    this.out = out;
    this.session = new Session(entry);
  }

  /** feed one line; returns true when the input is complete and was handled */
  line(text: string): boolean {
    this.buffer.push(text);
    const whole = this.buffer.join('\n');
    if (needsMore(whole)) return false;
    this.buffer = [];
    this.input(whole);
    return true;
  }
  pending(): boolean { return this.buffer.length > 0; }
  /** drop the input being continued (Ctrl-C at a continuation prompt) */
  discard(): void { this.buffer = []; }

  private error(msg: string) { this.errors++; this.out(`error: ${msg}`); }
  private diag(d: any, inFile?: string) { this.out(fmtDiag(d, inFile)); }
  private value(json: string) { this.out(this.compact ? json : prettyJson(json)); }

  input(text: string) {
    const t = text.trim();
    if (!t || /^\/\/(?!\/)/.test(t)) return;
    try {
      if (t.startsWith(':')) this.command(t);
      else if (DECL_HEAD.test(t)) this.addDeclaration(t);
      else {
        const m = OUTPUT_HEAD.exec(t);
        if (m && !KEYWORDS.has(m[1])) this.sessionOutput(m[1], m[2]?.trim(), m[3].trim());
        else this.expression(t);
      }
    } catch (e: any) {
      if (e instanceof SessionError) this.error(e.message);
      else throw e;
    }
  }

  private expression(text: string) {
    parseExpr(text);
    const r = this.session.evaluateExpr(text);
    for (const d of r.diags) this.diag(d);
    if (r.error) { if (r.error.message) this.out(`error${r.error.code ? ` [${r.error.code}]` : ''}: ${r.error.message}`); this.out('(invalid)'); }
    else this.value(r.value!);
    this.out('(partial)');
  }
  private addDeclaration(text: string) {
    const { decl, name } = parseDecl(text);
    if (decl.d === 'output' || decl.d === 'input') {
      // a root declared in the session's scope, as written in a module
    }
    this.session.apply({ op: 'declare', name, text: text.trim() });
  }
  private sessionOutput(name: string, type: string | undefined, expr: string) {
    parseExpr(expr);
    if (type) parseDecl(`output ${name}: ${type} = 0`);
    this.session.apply({ op: 'output', name, type, expr });
  }

  private command(t: string) {
    const sp = t.search(/\s/);
    const cmd = sp < 0 ? t : t.slice(0, sp);
    const rest = sp < 0 ? '' : t.slice(sp + 1).trim();
    const s = this.session;
    const noArgs = () => { if (rest) throw new SessionError(`${cmd} takes no argument`); };
    const oneName = () => { if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(rest)) throw new SessionError(`${cmd} expects a name`); return rest; };
    switch (cmd) {
      case ':load': {
        if (!rest) throw new SessionError(':load expects a file');
        this.session = new Session(rest);
        return;
      }
      case ':reload': noArgs(); s.apply(s.reloadOp()); return;
      case ':roots': {
        noArgs();
        const rs = s.roots();
        if (!rs.length) { this.out('(no roots)'); return; }
        for (const r of rs) {
          const status = r.session ? 'session' : r.kind === 'output' ? (r.binding === 'detached' ? 'detached' : r.exported ? 'exported' : 'local') : r.binding;
          this.out(`${r.kind.padEnd(7)} ${r.name.padEnd(16)} ${status.padEnd(12)} ${r.module.padEnd(16)} ${r.detail}${r.edited ? ' (edited)' : ''}`.trimEnd());
        }
        return;
      }
      case ':bind': {
        let m = /^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([\s\S]+)$/.exec(rest);
        if (m && /^[\[{]/.test(m[2].trim()) === false && !/^\s/.test(rest.slice(m[1].length))) {
          // name=file (no spaces around =)
          const file = m[2].trim();
          let text: string;
          try { text = readFileSync(file, 'utf8'); } catch { throw new SessionError(`cannot read ${file}`); }
          s.apply({ op: 'bind', name: m[1], src: { kind: 'file', file, text } });
          return;
        }
        if (m) { s.apply({ op: 'bind', name: m[1], src: { kind: 'expr', text: m[2].trim() } }); return; }
        m = /^([A-Za-z_][A-Za-z0-9_]*)\s+([\[{][\s\S]*)$/.exec(rest);
        if (m) { s.apply({ op: 'bind', name: m[1], src: { kind: 'inline', text: m[2] } }); return; }
        throw new SessionError(':bind expects name=doc.json, name { … }, or name = expr');
      }
      case ':unbind': s.apply({ op: 'unbind', name: oneName() }); return;
      case ':create': case ':update': {
        const m = /^(\S+)\s*=\s*([\s\S]+)$/.exec(rest);
        if (!m) throw new SessionError(`${cmd} expects path = expr`);
        s.apply({ op: 'edit', kind: cmd.slice(1) as 'create' | 'update', path: m[1], expr: m[2].trim() });
        return;
      }
      case ':remove': {
        if (!rest || /\s/.test(rest)) throw new SessionError(':remove expects a path');
        s.apply({ op: 'edit', kind: 'remove', path: rest });
        return;
      }
      case ':diff': for (const l of s.diff(oneName())) this.out(l); return;
      case ':save': {
        const m = /^([A-Za-z_][A-Za-z0-9_]*)=(\S+)$/.exec(rest);
        if (!m) throw new SessionError(':save expects name=file');
        s.save(m[1], m[2]);
        return;
      }
      case ':drop': s.apply({ op: 'drop', name: oneName() }); return;
      case ':write': if (!rest) throw new SessionError(':write expects a file'); s.write(rest); return;
      case ':session': { noArgs(); const ls = s.sessionLines(); if (!ls.length) this.out('(empty session)'); ls.forEach(l => this.out(l)); return; }
      case ':reset': noArgs(); s.apply({ op: 'reset' }); return;
      case ':check': {
        noArgs();
        const cs = s.check();
        for (const c of cs) this.diag(c.diag, c.file === s.entryAbs ? undefined : c.file);
        if (!cs.length) this.out('ok');
        return;
      }
      case ':evaluate': {
        const names = rest ? rest.split(/[\s,]+/).filter(Boolean) : [];
        const { run, docs, exported } = s.evaluate(names);
        for (const d of run.loadDiags) this.diag(d);
        for (const c of run.checks) this.diag(c.diag, c.file === s.entryAbs ? undefined : c.file);
        for (const c of run.sessionChecks) this.diag(c);
        for (const d of run.diags) this.diag(d);
        if (!run.entry) return;
        if (!run.eng) { this.out('(not evaluated)'); return; }
        if (exported) {
          if (!run.eng) { this.out('(invalid)'); return; }
          if (docs.some(d => d.json === null)) { this.out('(invalid)'); return; }
          this.value(`{${docs.map(d => `${JSON.stringify(d.name)}:${d.json}`).join(',')}}`);
          return;
        }
        for (const d of docs) {
          if (docs.length > 1) this.out(`${d.name}:`);
          if (d.json === null) this.out('(invalid)'); else this.value(d.json);
        }
        return;
      }
      case ':validate': {
        const names = rest ? rest.split(/[\s,]+/).filter(Boolean) : [];
        const { run, verdicts, diags } = s.validate(names);
        for (const d of run.loadDiags) this.diag(d);
        for (const c of run.checks) this.diag(c.diag, c.file === s.entryAbs ? undefined : c.file);
        for (const c of run.sessionChecks) this.diag(c);
        if (!run.eng) { this.out('(not evaluated)'); return; }
        for (const d of diags) this.diag(d);
        if (!verdicts.length) this.out('(no roots)');
        for (const v of verdicts) {
          const n = (k: number, w: string) => `${k} ${w}${k === 1 ? '' : 's'}`;
          this.out(v.errors === 0 && v.warnings === 0 ? `${v.name}: ok` : `${v.name}: ${[v.errors ? n(v.errors, 'error') : '', v.warnings ? n(v.warnings, 'warning') : ''].filter(Boolean).join(', ')}`);
        }
        return;
      }
      case ':fmt': { noArgs(); const t = s.fmt(); if (t) this.out(t.replace(/\n$/, '')); else this.out('(empty session)'); return; }
      case ':type': {
        if (!rest) throw new SessionError(':type expects an expression');
        const r = s.typeOf(rest);
        for (const d of r.diags) this.diag(d);
        this.out(`${r.type}${r.maybeAbsent ? '  (maybe absent)' : ''}`);
        return;
      }
      case ':doc': { if (!rest) throw new SessionError(':doc expects a name'); s.docOf(rest).forEach(l => this.out(l)); return; }
      case ':path': { if (!rest) throw new SessionError(':path expects an expression'); this.out(s.pathOf(rest)); return; }
      case ':trace': { if (!rest || /\s/.test(rest)) throw new SessionError(':trace expects a path'); s.trace(rest).forEach(l => this.out(l)); return; }
      case ':complete': { const cs = s.complete(rest, COMMAND_NAMES); if (!cs.length) this.out('(no completions)'); cs.forEach(c => this.out(c)); return; }
      case ':undo': case ':redo': {
        const n = rest ? parseInt(rest, 10) : 1;
        if (!(n >= 1)) throw new SessionError(`${cmd} expects a count`);
        const k = cmd === ':undo' ? s.undo(n) : s.redo(n);
        if (k === 0) this.out(cmd === ':undo' ? 'nothing to undo' : 'nothing to redo');
        return;
      }
      case ':history': {
        if (rest) { try { writeFileSync(rest, s.scriptLines().join('\n') + '\n'); } catch { throw new SessionError(`cannot write ${rest}`); } return; }
        s.historyLines().forEach(l => this.out(l));
        return;
      }
      case ':time': {
        noArgs();
        const t = s.lastTiming;
        if (!t) { this.out('nothing evaluated yet'); return; }
        const ms = (x: number) => `${x.toFixed(1)} ms`;
        this.out(`total ${ms(t.total)} (load ${ms(t.load)}, check ${ms(t.check)}, bind ${ms(t.bind)}, evaluate ${ms(t.evaluate)})${t.recomputed !== undefined ? `, recomputed ${t.recomputed} of ${t.slots} slots` : ''}`);
        return;
      }
      case ':set': {
        if (rest === 'pretty') this.compact = false;
        else if (rest === 'compact') this.compact = true;
        else throw new SessionError(':set expects pretty or compact');
        return;
      }
      case ':help': {
        const rows = rest ? COMMANDS.filter(c => c[0].split(' ')[0] === (rest.startsWith(':') ? rest : ':' + rest)) : COMMANDS;
        if (!rows.length) throw new SessionError(`unknown command ${rest}`);
        let cat = '';
        for (const [form, what, c] of rows) {
          if (!rest && c !== cat) { cat = c; this.out(`${cat}:`); }
          this.out(`  ${form.padEnd(24)} ${what}`);
        }
        return;
      }
      case ':quit': noArgs(); this.quitRequested = true; return;
      default: throw new SessionError(`unknown command ${cmd}`);
    }
  }
}

// ---------------- the command ----------------
export async function runRepl(args: string[]): Promise<number> {
  let entry: string | undefined, script: string | undefined, compact = false;
  const inputs: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === '--script') script = args[++i];
    else if (a === '--input') inputs.push(args[++i]);
    else if (a === '--compact') compact = true;
    else if (a.startsWith('--')) { console.error(`unknown option ${a}`); return 2; }
    else if (entry === undefined) entry = a;
    else { console.error('decl repl takes one entry file'); return 2; }
  }
  if (script === undefined && entry === undefined && inputs.length) { console.error('--input needs an entry file'); return 2; }
  for (const spec of inputs) if (!spec.includes('=')) { console.error(`--input expects name=doc.json, got ${spec}`); return 2; }

  const lines: string[] = [];
  const out = (l: string) => { process.stdout.write(l + '\n'); };
  const repl = new Repl(out, entry);
  repl.compact = compact;
  for (const spec of inputs) repl.input(`:bind ${spec}`);

  if (script !== undefined) {
    let text: string;
    try { text = script === '-' ? readFileSync(0, 'utf8') : readFileSync(script, 'utf8'); }
    catch { console.error(`cannot read ${script}`); return 2; }
    for (const l of text.replace(/\n$/, '').split('\n')) {
      out(`${repl.pending() ? '. ' : '> '}${l}`);
      repl.line(l);
      if (repl.quitRequested) break;
    }
    if (repl.pending()) repl.line('');
    void lines;
    return repl.errors ? 1 : 0;
  }

  // interactive: the line editor, with history (kept across sessions in
  // ~/.decl_history) and completion
  const historyFile = join(homedir(), '.decl_history');
  let history: string[] = [];
  try { history = readFileSync(historyFile, 'utf8').split('\n').filter(Boolean).reverse().slice(0, 1000); } catch { /* none yet */ }
  const rl = createInterface({
    input: process.stdin, output: process.stdout, prompt: '> ',
    history, historySize: 1000,
    completer: (line: string) => {
      const cs = repl.session.complete(line, COMMAND_NAMES).map(c => c.split('  ')[0]);
      const m = /([A-Za-z_$:][A-Za-z0-9_$.\[\]"]*)$/.exec(line);
      const tok = m ? m[1] : '';
      const tail = tok.includes('.') ? tok.slice(tok.lastIndexOf('.') + 1) : tok;
      return [cs.filter(c => c.startsWith(tail)), tail];
    },
  });
  rl.prompt();
  rl.on('line', l => {
    if (l.trim()) { try { appendFileSync(historyFile, l + '\n'); } catch { /* best effort */ } }
    repl.line(l);
    if (repl.quitRequested) { rl.close(); return; }
    rl.setPrompt(repl.pending() ? '. ' : '> ');
    rl.prompt();
  });
  rl.on('SIGINT', () => { if (repl.pending()) { repl.discard(); } rl.write(null, { ctrl: true, name: 'u' }); out(''); rl.setPrompt('> '); rl.prompt(); });
  await new Promise<void>(res => rl.on('close', () => res()));
  return 0;
}

export type { Op };
