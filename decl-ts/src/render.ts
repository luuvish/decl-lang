// The renderer (docs/tooling/05_render.md): the form a module declares
// for an output with `@render` — a format and a layout, a template, a
// destination, a fan-out — read from the annotation (§3), the structured
// text of a document in that form (§4), and the templates (§5) and the
// fan-out (§6) that turn one evaluated root into text or files. The
// command line, the REPL, the library, and the editor preview all emit
// through here, so that the three implementations print the same bytes.
import type { Decl, Expr } from './ast.ts';
import { toJson, toYaml } from './yaml.ts';
import { parseExprText } from './parse.ts';
import { dirname, basename, resolvePath } from './host.ts';
import {
  ABSENT,
  EvalErr,
  isArr,
  isClo,
  isMap,
  isQ,
  isRec,
  isRef,
  mapKey,
  pathStr,
  readJson,
} from './semantics.ts';
import type { Diag, Env, RecInst, Seg } from './semantics.ts';
import type { Engine } from './engine.ts';

/** a template's delimiters (§5.2): each an opener and a closer */
export type Delimiters = {
  value: [string, string];
  statement: [string, string];
  comment: [string, string];
};
export const DEFAULT_DELIMITERS: Delimiters = {
  value: ['{=', '=}'],
  statement: ['{%', '%}'],
  comment: ['{#', '#}'],
};

/** the declared form of a root (§3): what `@render` says, every key optional */
export type Form = {
  format: 'json' | 'yaml';
  indent?: number;
  template?: string;
  file?: string;
  each?: string;
  delimiters?: Delimiters;
};
export const CANONICAL: Form = { format: 'json' };

const FORM_KEYS = ['format', 'indent', 'template', 'file', 'each', 'delimiters'];

/**
 * the form `@render` declares on a declaration (§3), or the E7004 message
 * naming what is wrong with it; a declaration without one is canonical JSON
 */
export function declaredForm(decl: Decl): Form | { error: string } {
  const anns = (decl.annotations ?? []).filter((a) => a.name === 'render');
  if (anns.length === 0) return CANONICAL;
  if (anns.length > 1) return { error: 'more than one @render' };
  const a = anns[0];
  if (a.args.length !== 1 || a.args[0].e !== 'obj')
    return { error: '@render takes one object literal' };
  const form: Form = { format: 'json' };
  const seen = new Set<string>();
  for (const { key, val } of a.args[0].entries) {
    if (!FORM_KEYS.includes(key)) return { error: `@render: unknown key ${key}` };
    if (seen.has(key)) return { error: `@render: key ${key} repeats` };
    seen.add(key);
    const lit = literal(val);
    switch (key) {
      case 'format':
        if (lit !== 'json' && lit !== 'yaml')
          return { error: '@render: format must be "json" or "yaml"' };
        form.format = lit;
        break;
      case 'indent':
        if (typeof lit !== 'bigint' || lit < 0n || lit > 16n)
          return { error: '@render: indent must be an integer in 0..16' };
        form.indent = Number(lit);
        break;
      case 'template':
      case 'file':
      case 'each':
        if (typeof lit !== 'string' || lit === '')
          return { error: `@render: ${key} must be a non-empty string` };
        form[key] = lit;
        break;
      case 'delimiters': {
        const d = delimiters(val);
        if (typeof d === 'string') return { error: `@render: ${d}` };
        form.delimiters = d;
        break;
      }
    }
  }
  return form;
}

// a literal value in an annotation argument: a string, an integer, a
// bool, or null (a negative integer is a unary minus over a literal)
function literal(e: Expr): string | bigint | boolean | null | undefined {
  if (e.e === 'lit') return e.v;
  if (e.e === 'un' && e.op === '-' && e.x.e === 'lit' && typeof e.x.v === 'bigint') return -e.x.v;
  return undefined;
}
function delimiters(e: Expr): Delimiters | string {
  if (e.e !== 'obj') return 'delimiters must be an object of three pairs';
  const out: Delimiters = { ...DEFAULT_DELIMITERS };
  const seen = new Set<string>();
  for (const { key, val } of e.entries) {
    if (key !== 'value' && key !== 'statement' && key !== 'comment')
      return `delimiters: unknown key ${key}`;
    if (seen.has(key)) return `delimiters: key ${key} repeats`;
    seen.add(key);
    if (val.e !== 'arr' || val.items.length !== 2 || val.items.some((it) => it.spread))
      return `delimiters: ${key} must be a pair of strings`;
    const pair = val.items.map((it) => literal(it.expr));
    if (pair.some((p) => typeof p !== 'string' || p === ''))
      return `delimiters: ${key} must be a pair of non-empty strings`;
    out[key] = [pair[0] as string, pair[1] as string];
  }
  const openers = [out.value[0], out.statement[0], out.comment[0]];
  if (new Set(openers).size !== 3) return 'delimiters: the three openers must differ';
  return out;
}

/** the structured text of a document (readJson's shape) in a form's format and layout (§4), one trailing newline */
export function layout(raw: any, form: { format: 'json' | 'yaml'; indent?: number }): string {
  if (form.format === 'yaml') return toYaml(raw, form.indent ?? 2) + '\n';
  return toJson(raw, form.indent ?? 0) + '\n';
}

// ---------------- templates (§5) ----------------
// A template is text with tags in it: `{= expr =}` places the text form of
// a Decl expression, `{% stmt %}` is a statement, `{# … #}` a comment,
// `{% raw %}…{% endraw %}` verbatim text. The dialect is fixed here and
// implemented three times; expressions are the language's, evaluated by
// its engine over the root's document (§5.4).

/** a rendering diagnostic: the code, the message, and where — `L:C` of the tag, or a document path */
export class RenderError extends Error {
  code: string;
  where: string;
  /** the file the diagnostic is reported against: the template's path as given, or the module's */
  file: string | null;
  constructor(code: string, message: string, where: string, file: string | null = null) {
    super(message);
    this.code = code;
    this.where = where;
    this.file = file;
  }
  diag(): Diag {
    return { severity: 'error', code: this.code, message: this.message, path: this.where };
  }
}

type Pos = { line: number; col: number };
type Node =
  | { t: 'text'; s: string }
  | { t: 'value'; expr: Expr; at: Pos }
  | { t: 'if'; arms: { cond: Expr | null; body: Node[]; at: Pos }[] }
  | {
      t: 'for';
      vars: string[];
      iter: Expr;
      filter: Expr | null;
      body: Node[];
      empty: Node[] | null;
      at: Pos;
    }
  | { t: 'set'; name: string; expr: Expr; at: Pos }
  | { t: 'include'; path: string; at: Pos };

/** a parsed template: its path as given (diagnostics), its directory (includes), its nodes */
export type Template = { path: string; dir: string; nodes: Node[] };

type Tag = { kind: 'value' | 'stmt'; text: string; at: Pos; left: string; right: string };

// the lexer: text and tags, with the whitespace rules of §5.2 applied —
// trim_blocks and lstrip_blocks on for statements, `-` and `+` overriding
function lex(src: string, path: string, d: Delimiters): (string | Tag)[] {
  const out: (string | Tag)[] = [];
  const openers: [string, 'value' | 'stmt' | 'comment'][] = [
    [d.value[0], 'value'],
    [d.statement[0], 'stmt'],
    [d.comment[0], 'comment'],
  ];
  openers.sort((a, b) => b[0].length - a[0].length); // longest first at every position
  const closerOf = { value: d.value[1], stmt: d.statement[1], comment: d.comment[1] };
  let i = 0;
  let text = ''; // the text since the last tag
  let after: 'none' | 'trim' | 'strip'; // what the last tag asks of the text after it
  let first = true; // no tag yet: the text starts the template
  const posOf = (k: number): Pos => {
    let line = 1,
      last = -1;
    for (let j = 0; j < k; j++)
      if (src[j] === '\n') {
        line++;
        last = j;
      }
    return { line, col: k - last };
  };
  const fail = (code: string, message: string, k: number): never => {
    const p = posOf(k);
    throw new RenderError(code, message, `${p.line}:${p.col}`, path);
  };
  const flushText = () => {
    if (text.length) out.push(text);
    text = '';
  };
  while (i < src.length) {
    let found: [string, 'value' | 'stmt' | 'comment'] | null = null;
    for (const o of openers)
      if (src.startsWith(o[0], i)) {
        found = o;
        break;
      }
    if (!found) {
      text += src[i];
      i++;
      continue;
    }
    const [opener, kind] = found;
    const start = i;
    let j = i + opener.length;
    // the modifier after the opener
    let left = '';
    if (kind !== 'comment' && (src[j] === '-' || src[j] === '+')) {
      left = src[j];
      j++;
    }
    const closer = closerOf[kind];
    // the tag's end: the closer, possibly preceded by a modifier
    let end = -1,
      right = '';
    for (let k = j; k <= src.length - closer.length; k++) {
      if (!src.startsWith(closer, k)) continue;
      if (kind !== 'comment' && k > j && (src[k - 1] === '-' || src[k - 1] === '+')) {
        right = src[k - 1];
        end = k - 1;
      } else end = k;
      break;
    }
    if (end < 0) fail('E7001', `unclosed ${opener} tag`, start);
    const body = src.slice(j, end);
    let next = end + right.length + closer.length;
    // the text before the tag: `-` strips all white space, a statement's
    // default strips the indentation of its line (lstrip_blocks), `+` keeps
    let before = text;
    if (left === '-') before = before.replace(/\s+$/, '');
    else if (kind === 'stmt' && left !== '+') {
      const m = /(^|\n)([ \t]*)$/.exec(before);
      if (m && (m[1] === '\n' || first)) before = before.slice(0, before.length - m[2].length);
    }
    text = before;
    flushText();
    first = false;
    if (kind === 'comment') {
      after = 'none';
    } else if (kind === 'stmt' && /^\s*raw\s*$/.test(body)) {
      // verbatim text to the matching endraw, which may carry modifiers
      const re = new RegExp(
        `${escapeRe(d.statement[0])}[-+]?\\s*endraw\\s*[-+]?${escapeRe(d.statement[1])}`,
      );
      const m = re.exec(src.slice(next)) ?? fail('E7001', 'unclosed {% raw %}', start);
      // the raw tag's own whitespace rules
      let raw = src.slice(next, next + m.index);
      if (right === '-') raw = raw.replace(/^\s+/, '');
      else if (right !== '+') raw = raw.replace(/^\r?\n/, '');
      const endTag = m[0];
      const endLeft = endTag[d.statement[0].length];
      const endRight = endTag[endTag.length - d.statement[1].length - 1];
      if (endLeft === '-') raw = raw.replace(/\s+$/, '');
      else if (endLeft !== '+') {
        const mm = /\n([ \t]*)$/.exec(raw);
        if (mm) raw = raw.slice(0, raw.length - mm[1].length);
      }
      out.push(raw);
      next += m.index + endTag.length;
      after = endRight === '-' ? 'strip' : endRight === '+' ? 'none' : 'trim';
    } else {
      out.push({ kind, text: body, at: posOf(start), left, right });
      after = right === '-' ? 'strip' : right === '+' ? 'none' : kind === 'stmt' ? 'trim' : 'none';
    }
    i = next;
    // the text after the tag: `-` strips all white space, a statement's
    // default drops the line break that follows it (trim_blocks)
    if (after === 'strip') {
      while (i < src.length && /\s/.test(src[i])) i++;
    } else if (after === 'trim') {
      if (src[i] === '\n') i++;
      else if (src[i] === '\r' && src[i + 1] === '\n') i += 2;
    }
  }
  flushText();
  return out;
}
const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// the parser: statements nest, every `if` and `for` closes
function parseNodes(src: string, path: string, d: Delimiters): Node[] {
  const tokens = lex(src, path, d);
  let k = 0;
  const fail = (message: string, at: Pos): never => {
    throw new RenderError('E7001', message, `${at.line}:${at.col}`, path);
  };
  const expr = (text: string, at: Pos): Expr =>
    parseExprText(text) ?? fail(`expression does not parse: ${text.trim()}`, at);
  // `for x in e if c`: the filter is the last top-level `if` whose two
  // sides both parse; an `if` inside brackets or a string is the expression's
  const iterAndFilter = (text: string, at: Pos): [Expr, Expr | null] => {
    const cands: number[] = [];
    let depth = 0,
      quote: string | null = null;
    for (let i = 0; i < text.length; i++) {
      const c = text[i];
      if (quote) {
        if (c === '\\') i++;
        else if (c === quote) quote = null;
        continue;
      }
      if (c === '"' || c === "'" || c === '`') quote = c;
      else if ('([{'.includes(c)) depth++;
      else if (')]}'.includes(c)) depth--;
      else if (
        depth === 0 &&
        /\s/.test(text[i - 1] ?? ' ') &&
        text.startsWith('if', i) &&
        /\s/.test(text[i + 2] ?? '')
      )
        cands.push(i);
    }
    for (const i of cands.reverse()) {
      const a = parseExprText(text.slice(0, i)),
        b = parseExprText(text.slice(i + 2));
      if (a && b) return [a, b];
    }
    return [expr(text, at), null];
  };
  const body = (closers: string[], opened: string, at: Pos): [Node[], string, Pos, string] => {
    const nodes: Node[] = [];
    while (k < tokens.length) {
      const tok = tokens[k++];
      if (typeof tok === 'string') {
        nodes.push({ t: 'text', s: tok });
        continue;
      }
      if (tok.kind === 'value') {
        nodes.push({ t: 'value', expr: expr(tok.text, tok.at), at: tok.at });
        continue;
      }
      const m = /^\s*([A-Za-z_][A-Za-z0-9_]*)\s*([\s\S]*?)\s*$/.exec(tok.text);
      if (!m) fail('empty statement', tok.at);
      const [, word, rest] = m!;
      if (closers.includes(word)) return [nodes, word, tok.at, rest];
      switch (word) {
        case 'if': {
          const arms: { cond: Expr | null; body: Node[]; at: Pos }[] = [];
          let cond: Expr | null = expr(rest, tok.at);
          let at = tok.at;
          for (;;) {
            const [b, closer, cAt, cRest] = body(['elif', 'else', 'endif'], 'if', at);
            arms.push({ cond, body: b, at });
            if (closer === 'endif') {
              if (cRest) fail('{% endif %} takes nothing', cAt);
              break;
            }
            if (closer === 'else') {
              if (cRest) fail('{% else %} takes nothing', cAt);
              if (cond === null) fail('{% else %} after {% else %}', cAt);
              cond = null;
            } else {
              if (cond === null) fail('{% elif %} after {% else %}', cAt);
              cond = expr(cRest, cAt);
            }
            at = cAt;
          }
          nodes.push({ t: 'if', arms });
          continue;
        }
        case 'for': {
          const fm =
            /^([A-Za-z_][A-Za-z0-9_]*)\s*(?:,\s*([A-Za-z_][A-Za-z0-9_]*))?\s+in\s+([\s\S]+)$/.exec(
              rest,
            );
          if (!fm) fail('{% for %} expects `x in e` or `k, v in e`', tok.at);
          const vars = fm![2] ? [fm![1], fm![2]] : [fm![1]];
          const [iter, filter] = iterAndFilter(fm![3], tok.at);
          const [b, closer, cAt, cRest] = body(['else', 'endfor'], 'for', tok.at);
          if (cRest) fail(`{% ${closer} %} takes nothing`, cAt);
          let empty: Node[] | null = null;
          if (closer === 'else') {
            const [e, c2, c2At, c2Rest] = body(['endfor'], 'for', cAt);
            if (c2Rest) fail(`{% ${c2} %} takes nothing`, c2At);
            empty = e;
          }
          nodes.push({ t: 'for', vars, iter, filter, body: b, empty, at: tok.at });
          continue;
        }
        case 'set': {
          const sm = /^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([\s\S]+)$/.exec(rest);
          if (!sm) fail('{% set %} expects `x = e`', tok.at);
          nodes.push({ t: 'set', name: sm![1], expr: expr(sm![2], tok.at), at: tok.at });
          continue;
        }
        case 'include': {
          const im = /^"((?:[^"\\]|\\.)*)"$/.exec(rest);
          if (!im) fail('{% include %} expects a quoted path', tok.at);
          nodes.push({ t: 'include', path: JSON.parse(`"${im![1]}"`), at: tok.at });
          continue;
        }
        case 'elif':
        case 'else':
        case 'endif':
        case 'endfor':
        case 'endraw':
          fail(
            `{% ${word} %} without ${word === 'endfor' ? '{% for %}' : word === 'endraw' ? '{% raw %}' : '{% if %}'}`,
            tok.at,
          );
          break;
        default:
          fail(`unknown tag {% ${word} %}`, tok.at);
      }
    }
    if (opened) fail(`unclosed {% ${opened} %}`, at);
    return [nodes, '', at, ''];
  };
  return body([], '', { line: 1, col: 1 })[0];
}

/**
 * parse a template's text (§5.2–5.3); E7001 for what does not parse. `path`
 * is the template's path as given (its diagnostics name it); `dir` is where
 * its includes resolve, the absolute directory it was read from
 */
export function parseTemplate(
  text: string,
  path: string,
  delimiters = DEFAULT_DELIMITERS,
  dir?: string,
): Template {
  return {
    path,
    dir: dir ?? dirname(resolvePath(path)),
    nodes: parseNodes(text, path, delimiters),
  };
}

/** what a template renders over (§5.4) */
export type Context = {
  eng: Engine;
  /** the entry module's environment: its consts and funcs are in scope */
  menv: Env;
  rootName: string;
  /** the root's document, bound to the root's name */
  root: any;
  /** a fan-out element and its key (§6), when rendering one element */
  item?: { value: any; key: any };
  /** the text of another template file by absolute path, or null when it cannot be read */
  readTemplate: (abs: string) => string | null;
  delimiters: Delimiters;
};

/** the text form of a value (§5.5); E7002 when it has none */
export function textForm(eng: Engine, v: any, rootName: string): string {
  if (typeof v === 'string') return v;
  if (typeof v === 'bigint') return v.toString();
  if (typeof v === 'number') return fmtFloat(v);
  if (typeof v === 'boolean') return String(v);
  if (v === null) return 'null';
  if (v === ABSENT || v === undefined)
    throw new RenderError('E7002', 'value has no text form: absent', '');
  if (isClo(v) || v.__nat || v.__std || v.__nsref)
    throw new RenderError('E7002', 'value has no text form: a function', '');
  if (isQ(v)) return `${fmtFloat(v.value)} ${eng.env.baseUnitOf.get(v.dim) ?? v.dim}`;
  if (isRef(v)) return pathStr(v.segs, rootName);
  if (isArr(v) || isMap(v) || isRec(v)) return eng.serialize(v, rootName);
  throw new RenderError('E7002', 'value has no text form', '');
}
const fmtFloat = (n: number): string => {
  const s = String(n);
  return /[.eE]/.test(s) ? s : s + '.0';
};

// the `render` namespace (§5.6): json, yaml, indent
function renderNamespace(eng: Engine, rootName: string): any {
  const raw = (v: any) => readJson(eng.serialize(v, rootName));
  const indentArg = (a: any[], i: number): number => {
    if (a.length <= i) return -1;
    if (typeof a[i] !== 'bigint' || a[i] < 0n || a[i] > 16n)
      throw new EvalErr('render: indent must be an integer in 0..16');
    return Number(a[i]);
  };
  const nat = (f: (a: any[]) => any) => ({ __nat: f });
  const json = (a: any[]) => {
    if (!a.length) throw new EvalErr('render.json expects a value');
    return toJson(raw(a[0]), Math.max(indentArg(a, 1), 0));
  };
  const yaml = (a: any[]) => {
    if (!a.length) throw new EvalErr('render.yaml expects a value');
    const n = indentArg(a, 1);
    return toYaml(raw(a[0]), n < 0 ? 2 : n);
  };
  const indent = (a: any[]) => {
    if (typeof a[0] !== 'string' || typeof a[1] !== 'bigint' || a[1] < 0n)
      throw new EvalErr('render.indent expects a string and a count');
    return a[0].replace(/\n/g, '\n' + ' '.repeat(Number(a[1])));
  };
  return {
    __pre: 'obj',
    entries: [
      ['json', nat(json)],
      ['yaml', nat(yaml)],
      ['indent', nat(indent)],
    ],
  };
}

// the members of a record in canonical order (§7.2), as the serializer walks them
function recordEntries(eng: Engine, inst: RecInst): [string, any][] {
  const out: [string, any][] = [];
  const done = new Set<string>();
  for (const n of inst.entryOrder) {
    done.add(n);
    if (inst.extras.has(n)) continue;
    const s = inst.slots.get(n);
    if (!s || s.hidden || s.state === 'invalid' || s.state === 'absent' || s.kind === 'der')
      continue;
    out.push([n, eng.access(inst, n)]);
  }
  for (const m of inst.rt.members) {
    if (done.has(m.name) && m.kind !== 'der') continue;
    const s = inst.slots.get(m.name);
    if (!s || s.hidden || s.state === 'invalid' || s.state === 'absent' || s.state === 'unforced')
      continue;
    out.push([m.name, eng.access(inst, m.name)]);
  }
  return out;
}

// the language's code for an evaluation failure inside a template
const codeOf = (e: EvalErr): string =>
  e.code ?? (e.message.startsWith('unknown name') ? 'E3003' : 'E4001');

/** render a parsed template over a context (§5); a RenderError carries the diagnostic */
export function renderTemplate(tpl: Template, cx: Context): string {
  const locals = new Map<string, any>();
  locals.set(cx.rootName, cx.root);
  if (cx.item) {
    locals.set('item', cx.item.value);
    locals.set('key', cx.item.key);
    if (isRec(cx.item.value))
      for (const [n, v] of recordEntries(cx.eng, cx.item.value)) locals.set(n, v);
    for (const [n, s] of isRec(cx.item.value) ? cx.item.value.slots : [])
      if (s.state === 'absent' && !locals.has(n)) locals.set(n, ABSENT);
  } else if (isRec(cx.root)) {
    for (const [n, v] of recordEntries(cx.eng, cx.root)) locals.set(n, v);
    for (const [n, s] of cx.root.slots)
      if (s.state === 'absent' && !locals.has(n)) locals.set(n, ABSENT);
  }
  locals.set('render', renderNamespace(cx.eng, cx.rootName));
  const parsed = new Map<string, Template>();
  const stack: string[] = [resolvePath(tpl.dir, basename(tpl.path))];
  return renderNodes(tpl, tpl.nodes, locals, cx, parsed, stack);
}

function renderNodes(
  tpl: Template,
  nodes: Node[],
  locals: Map<string, any>,
  cx: Context,
  parsed: Map<string, Template>,
  stack: string[],
): string {
  const { eng } = cx;
  const at = (p: Pos) => `${p.line}:${p.col}`;
  const evalAt = (e: Expr, p: Pos): any => {
    const sc: any = { inst: null, locals, rootName: cx.rootName, menv: cx.menv };
    try {
      let v = eng.ev(e, sc);
      v = eng.materialize(v, ['_'], null, sc);
      eng.forceAll(v, true);
      return v;
    } catch (err: any) {
      if (err instanceof EvalErr) throw new RenderError(codeOf(err), err.message, at(p), tpl.path);
      if (err instanceof RenderError) {
        if (!err.where) throw new RenderError(err.code, err.message, at(p), tpl.path);
        throw err;
      }
      throw new RenderError('E4001', 'expression cannot be evaluated', at(p), tpl.path);
    }
  };
  const declare = (name: string, p: Pos) => {
    if (locals.has(name) || cx.menv.consts.has(name) || cx.menv.funcs.has(name))
      throw new RenderError('E3019', `${name} shadows a name in scope`, at(p), tpl.path);
  };
  let out = '';
  for (const n of nodes) {
    switch (n.t) {
      case 'text':
        out += n.s;
        break;
      case 'value': {
        const v = evalAt(n.expr, n.at);
        try {
          out += textForm(eng, v, cx.rootName);
        } catch (err: any) {
          if (err instanceof RenderError)
            throw new RenderError(err.code, err.message, at(n.at), tpl.path);
          throw err;
        }
        break;
      }
      case 'if': {
        for (const arm of n.arms) {
          if (arm.cond === null) {
            out += renderNodes(tpl, arm.body, new Map(locals), cx, parsed, stack);
            break;
          }
          const c = evalAt(arm.cond, arm.at);
          if (typeof c !== 'boolean')
            throw new RenderError('E4001', 'condition is not a bool', at(arm.at), tpl.path);
          if (c) {
            out += renderNodes(tpl, arm.body, new Map(locals), cx, parsed, stack);
            break;
          }
        }
        break;
      }
      case 'for': {
        for (const v of n.vars) declare(v, n.at);
        if (n.vars.length === 2 && n.vars[0] === n.vars[1])
          throw new RenderError(
            'E3019',
            `${n.vars[0]} shadows a name in scope`,
            at(n.at),
            tpl.path,
          );
        const coll = evalAt(n.iter, n.at);
        let pairs: [any, any][];
        if (n.vars.length === 1) {
          let items: any[];
          try {
            items = eng.iterate(coll);
          } catch {
            throw new RenderError(
              'E4001',
              'for over a value that is not an array',
              at(n.at),
              tpl.path,
            );
          }
          pairs = items.map((x) => [x, undefined]);
        } else if (isRec(coll)) pairs = recordEntries(eng, coll);
        else if (isMap(coll)) pairs = [...coll.entries.entries()];
        else
          throw new RenderError(
            'E4001',
            'for k, v over a value that is not an object or a map',
            at(n.at),
            tpl.path,
          );
        if (n.filter) {
          const kept: [any, any][] = [];
          for (const [a, b] of pairs) {
            const l2 = new Map(locals);
            l2.set(n.vars[0], a);
            if (n.vars.length === 2) l2.set(n.vars[1], b);
            const sc: any = { inst: null, locals: l2, rootName: cx.rootName, menv: cx.menv };
            let c: any;
            try {
              c = eng.ev(n.filter, sc);
            } catch (err: any) {
              if (err instanceof EvalErr)
                throw new RenderError(codeOf(err), err.message, at(n.at), tpl.path);
              throw err;
            }
            if (typeof c !== 'boolean')
              throw new RenderError('E4001', 'filter is not a bool', at(n.at), tpl.path);
            if (c) kept.push([a, b]);
          }
          pairs = kept;
        }
        if (pairs.length === 0) {
          if (n.empty) out += renderNodes(tpl, n.empty, new Map(locals), cx, parsed, stack);
          break;
        }
        pairs.forEach(([a, b], i) => {
          const l2 = new Map(locals);
          l2.set(n.vars[0], a);
          if (n.vars.length === 2) l2.set(n.vars[1], b);
          l2.set('loop', {
            __pre: 'obj',
            entries: [
              ['index', BigInt(i + 1)],
              ['index0', BigInt(i)],
              ['first', i === 0],
              ['last', i === pairs.length - 1],
              ['length', BigInt(pairs.length)],
            ],
          });
          out += renderNodes(tpl, n.body, l2, cx, parsed, stack);
        });
        break;
      }
      case 'set': {
        declare(n.name, n.at);
        if (n.name === 'loop')
          throw new RenderError('E3019', 'loop cannot be assigned', at(n.at), tpl.path);
        locals.set(n.name, evalAt(n.expr, n.at));
        break;
      }
      case 'include': {
        const abs = resolvePath(tpl.dir, n.path);
        if (stack.includes(abs))
          throw new RenderError(
            'E7001',
            `include cycle: ${[...stack, abs].map((p) => basename(p)).join(' -> ')}`,
            at(n.at),
            tpl.path,
          );
        let sub = parsed.get(abs);
        if (!sub) {
          const text = cx.readTemplate(abs);
          if (text === null)
            throw new RenderError(
              'E7003',
              `template cannot be read: ${n.path}`,
              at(n.at),
              tpl.path,
            );
          sub = parseTemplate(text, n.path, cx.delimiters, dirname(abs));
          parsed.set(abs, sub);
        }
        stack.push(abs);
        try {
          out += renderNodes(sub, sub.nodes, new Map(locals), cx, parsed, stack);
        } finally {
          stack.pop();
        }
        break;
      }
    }
  }
  return out;
}

// ---------------- emission: one root in its form (§3, §6) ----------------

/** the text a root is emitted as: one text, or the files of a fan-out (§6), path by path */
export type Emitted = { kind: 'one'; text: string } | { kind: 'many'; files: [string, string][] };

/** what emits one root: its value, its form with the invocation's overrides, and the template's text when there is one */
export type Emission = {
  eng: Engine;
  menv: Env;
  rootName: string;
  value: any;
  form: Form;
  /** the overrides (§3.4) */
  format?: 'json' | 'yaml';
  indent?: number;
  /** the template's text, its path as given, and the absolute directory its includes resolve from, when the root has one (declared or overridden) */
  template?: { path: string; text: string; dir: string };
  readTemplate: (abs: string) => string | null;
};

// a fan-out element's file path (§6): a string, relative, `/`-separated,
// not leaving the directory, distinct — else E7005 at the element's path
function fanOutPath(each: string, elem: any, key: any, at: string, seen: Set<string>): string {
  let p: any;
  if (each === '$key') {
    if (typeof key !== 'string')
      throw new RenderError('E7005', 'fan-out path: $key names no key (the root is an array)', at);
    p = key;
  } else {
    if (!isRec(elem) || !elem.slots.has(each))
      throw new RenderError('E7005', `fan-out path: the element has no member ${each}`, at);
    p = elem.slots.get(each)!.state === 'absent' ? ABSENT : elem.slots.get(each)!.value;
  }
  if (typeof p !== 'string') throw new RenderError('E7005', 'fan-out path is not a string', at);
  if (p === '') throw new RenderError('E7005', 'fan-out path is empty', at);
  if (p.startsWith('/')) throw new RenderError('E7005', `fan-out path is absolute: ${p}`, at);
  if (p.includes('\\')) throw new RenderError('E7005', `fan-out path uses \\: ${p}`, at);
  if (p.split('/').some((s) => s === '..' || s === '.' || s === ''))
    throw new RenderError('E7005', `fan-out path leaves the destination directory: ${p}`, at);
  if (seen.has(p)) throw new RenderError('E7005', `fan-out path repeats: ${p}`, at);
  seen.add(p);
  return p;
}

/** emit one root (§3.1): its structured text or its template's text, as one text or one file per element */
export function emitRoot(e: Emission): Emitted {
  const format = e.format ?? e.form.format;
  const indent = e.indent ?? e.form.indent;
  const raw = (v: any) => readJson(e.eng.serialize(v, e.rootName));
  const tpl = e.template
    ? parseTemplate(
        e.template.text,
        e.template.path,
        e.form.delimiters ?? DEFAULT_DELIMITERS,
        e.template.dir,
      )
    : null;
  const cx = (item?: { value: any; key: any }): Context => ({
    eng: e.eng,
    menv: e.menv,
    rootName: e.rootName,
    root: e.value,
    item,
    readTemplate: e.readTemplate,
    delimiters: e.form.delimiters ?? DEFAULT_DELIMITERS,
  });
  if (!e.form.each) {
    if (tpl) return { kind: 'one', text: renderTemplate(tpl, cx()) };
    return { kind: 'one', text: layout(raw(e.value), { format, indent }) };
  }
  // fan-out: every element of the array or map to its own file
  let elems: [any, any, Seg][];
  if (isArr(e.value)) elems = e.value.items.map((v: any, i: number) => [v, BigInt(i), i]);
  else if (isMap(e.value))
    elems = [...e.value.entries.entries()].map(([k, v]: any) => [v, k, mapKey(k)]);
  else
    throw new RenderError(
      'E7004',
      `@render: each on a root that is neither an array nor a map`,
      e.rootName,
    );
  const seen = new Set<string>();
  const paths = elems.map(([v, k, seg]) =>
    fanOutPath(e.form.each!, v, k, pathStr([e.rootName, seg]), seen),
  );
  const files: [string, string][] = [];
  elems.forEach(([v, k], i) => {
    const text = tpl
      ? renderTemplate(tpl, cx({ value: v, key: k }))
      : layout(raw(v), { format, indent });
    files.push([paths[i], text]);
  });
  return { kind: 'many', files };
}
