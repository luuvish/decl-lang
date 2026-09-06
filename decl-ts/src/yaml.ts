// YAML for documents (docs/tooling/05_render.md §2, §4): a reader of
// YAML 1.2 with the core schema into the JSON data model — the values
// readJson produces, so that a document written in YAML is
// indistinguishable from the same document written in JSON from the
// reader on — and a writer of the block-style form that every YAML 1.2
// reader (and no YAML 1.1 reader) reads back as the canonical JSON
// document. Beside them, the JSON layouts of §4.1. The reader accepts
// exactly what the document says and refuses the rest with the reason
// and the line, so that the three implementations refuse the same texts
// with the same words.

/** a document the reader refuses: `<reason> at line L` */
export class YamlError extends Error {
  line: number;
  constructor(reason: string, line: number) {
    super(`${reason} at line ${line}`);
    this.line = line;
  }
}

/** a document path names YAML by its extension (§2); anything else is JSON */
export const isYamlPath = (p: string): boolean => /\.ya?ml$/i.test(p);

// ---------------- the core schema (§2): what a plain scalar means ----------------
// null, bool, int (decimal, octal, hexadecimal), float — everything else
// is a string. YAML 1.1's spellings (yes/no/on/off, sexagesimals,
// timestamps, `1_000`) are strings.
const RE_INT = /^[-+]?[0-9]+$/;
const RE_OCT = /^0o[0-7]+$/;
const RE_HEX = /^0x[0-9a-fA-F]+$/;
const RE_FLOAT = /^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$/;
const RE_NONFINITE = /^([-+]?\.(inf|Inf|INF)|\.(nan|NaN|NAN))$/;
type Plain =
  | { k: 'null' }
  | { k: 'bool'; v: boolean }
  | { k: 'int'; v: bigint }
  | { k: 'float'; v: number }
  | { k: 'nonfinite' }
  | { k: 'string'; v: string };
export function resolvePlain(s: string): Plain {
  if (s === '' || s === '~' || s === 'null' || s === 'Null' || s === 'NULL') return { k: 'null' };
  if (s === 'true' || s === 'True' || s === 'TRUE') return { k: 'bool', v: true };
  if (s === 'false' || s === 'False' || s === 'FALSE') return { k: 'bool', v: false };
  if (RE_INT.test(s)) return { k: 'int', v: BigInt(s.startsWith('+') ? s.slice(1) : s) };
  if (RE_OCT.test(s)) return { k: 'int', v: BigInt(s) };
  if (RE_HEX.test(s)) return { k: 'int', v: BigInt(s) };
  if (RE_FLOAT.test(s)) return { k: 'float', v: parseFloat(s) };
  if (RE_NONFINITE.test(s)) return { k: 'nonfinite' };
  return { k: 'string', v: s };
}

// ---------------- the reader ----------------
const isSpace = (c: string | undefined) => c === ' ' || c === '\t';
const isBreakOrEnd = (c: string | undefined) => c === undefined || c === '\n';
const FLOW_END = new Set([',', '[', ']', '{', '}']);

class Reader {
  s: string;
  i = 0;
  anchors = new Map<string, any>();
  constructor(src: string) {
    this.s = src.replace(/^\uFEFF/, '').replace(/\r\n?/g, '\n');
  }
  // ---- positions ----
  line(at = this.i): number {
    let n = 1;
    for (let k = 0; k < at && k < this.s.length; k++) if (this.s[k] === '\n') n++;
    return n;
  }
  fail(reason: string, at = this.i): never {
    throw new YamlError(reason, this.line(at));
  }
  peek(o = 0): string | undefined {
    return this.s[this.i + o];
  }
  atEnd(k = this.i): boolean {
    return isBreakOrEnd(this.s[k]);
  }
  lineStart(k = this.i): number {
    while (k > 0 && this.s[k - 1] !== '\n') k--;
    return k;
  }
  col(k = this.i): number {
    return k - this.lineStart(k);
  }
  /** the indentation of the line holding k: its leading spaces (a tab there is refused) */
  indentOf(k: number): number {
    let j = this.lineStart(k),
      n = 0;
    while (this.s[j] === ' ') {
      j++;
      n++;
    }
    if (this.s[j] === '\t') this.fail('tab in indentation', j);
    return n;
  }
  /** skip spaces and a comment on the current line; stays before its break */
  skipInline(): void {
    while (isSpace(this.peek())) this.i++;
    if (this.peek() === '#') while (!this.atEnd()) this.i++;
  }
  /** the rest of the current line must be empty (a comment allowed) */
  endLine(what: string): void {
    this.skipInline();
    if (!this.atEnd()) this.fail(`unexpected content after ${what}`);
  }
  /**
   * advance over line breaks, blank lines, and comment lines to the next
   * content character, and return its column; -1 at the end. Idempotent:
   * at a content character with only indentation before it, stays.
   */
  nextContent(): number {
    for (;;) {
      if (this.peek() === undefined) return -1;
      if (this.peek() === '\n') {
        this.i++;
        continue;
      }
      const start = this.lineStart();
      let j = start;
      while (this.s[j] === ' ') j++;
      if (j < this.i) {
        // mid-line: the caller left content behind — it belongs to no node
        this.fail('unexpected content');
      }
      if (this.s[j] === '\t') this.fail('tab in indentation', j);
      this.i = j;
      const c = this.peek();
      if (c === undefined) return -1;
      if (c === '\n') continue;
      if (c === '#') {
        while (!this.atEnd()) this.i++;
        continue;
      }
      return j - start;
    }
  }
  /** is a `-`, `?`, or `:` at k an indicator (followed by a space or the line's end)? */
  indicatorAt(k: number): boolean {
    return isSpace(this.s[k + 1]) || isBreakOrEnd(this.s[k + 1]);
  }
  /** a document marker (`---` or `...`) at column 0 ends every node */
  atMarker(): boolean {
    return this.lineIs('---') || this.lineIs('...');
  }
  lineIs(text: string): boolean {
    return (
      this.s.startsWith(text, this.i) &&
      this.col() === 0 &&
      this.indicatorAt(this.i + text.length - 1)
    );
  }

  // ---- the stream ----
  document(): any {
    // directives and the start marker
    for (;;) {
      if (this.peek() === '%' && this.col() === 0) {
        const end = this.s.indexOf('\n', this.i);
        const text = this.s.slice(this.i, end < 0 ? this.s.length : end);
        if (/^%YAML[ \t]+1\.2[ \t]*(#.*)?$/.test(text)) {
          this.i += text.length;
          continue;
        }
        if (text.startsWith('%TAG')) this.fail('uses a tag');
        if (text.startsWith('%YAML')) this.fail('unsupported YAML version');
        this.fail(`unsupported directive ${text.split(/[ \t]/)[0]}`);
      }
      const n = this.nextContent();
      if (n === -1) return null;
      if (this.peek() === '%' && n === 0) continue;
      break;
    }
    let value: any;
    let parsed = false;
    if (this.lineIs('---')) {
      this.i += 3;
      this.skipInline();
      if (!this.atEnd()) {
        value = this.node(-1, 'seq', -1);
        parsed = true;
      }
    } else if (this.lineIs('...')) {
      this.fail('unexpected end marker');
    }
    if (!parsed) value = this.node(-1, 'none', -1);
    // the tail: blank lines and comments, an end marker, nothing else
    let ended = false;
    for (;;) {
      const n = this.nextContent();
      if (n === -1) return value;
      if (this.lineIs('...')) {
        this.i += 3;
        this.endLine('the end marker');
        ended = true;
        continue;
      }
      if (this.lineIs('---')) this.fail('stream holds more than one document');
      this.fail(ended ? 'unexpected content after the end marker' : 'unexpected content');
    }
  }

  // ---- block nodes ----
  /**
   * the node whose text starts at the cursor (inline: on the line of the
   * `- ` or `key: ` it follows) or on the following lines (indented more
   * than `parent`; a sequence may sit at `seqAt` = the parent mapping's
   * own indentation). `where` says what the inline position follows.
   * Returns null for an empty node, leaving the cursor where it was.
   */
  node(parent: number, where: 'seq' | 'map' | 'none', seqAt: number): any {
    this.skipInline();
    if (!this.atEnd()) return this.nodeAt(this.col(), where, parent);
    const save = this.i;
    const n = this.nextContent();
    if (n === -1) return null;
    const dash = this.peek() === '-' && this.indicatorAt(this.i);
    if ((n <= parent && !(dash && seqAt === n)) || this.atMarker()) {
      this.i = save;
      return null;
    }
    return this.nodeAt(n, 'none', parent);
  }
  nodeAt(ind: number, where: 'seq' | 'map' | 'none', parent: number): any {
    const c = this.peek();
    if (c === '&') {
      const name = this.anchorName();
      this.skipInline();
      let v: any;
      if (this.atEnd()) {
        v = this.node(parent, where, -1);
      } else v = this.nodeAt(this.col(), where, parent);
      this.anchors.set(name, v);
      return v;
    }
    if (c === '*') {
      const at = this.i;
      const name = this.anchorName();
      if (!this.anchors.has(name)) this.fail(`unknown alias *${name}`, at);
      this.endLine('an alias');
      return copyOf(this.anchors.get(name));
    }
    if (c === '!') this.fail('uses a tag');
    if (c === '@' || c === '`') this.fail(`reserved indicator ${c}`);
    if (c === '%') this.fail('unexpected directive');
    if (c === '[' || c === '{') {
      const v = this.flowNode();
      this.endLine('a flow collection');
      return v;
    }
    if (c === '|' || c === '>') return this.blockScalar(parent);
    if (c === '"' || c === "'") {
      const start = this.i;
      const text = this.quoted();
      while (isSpace(this.peek())) this.i++;
      if (this.peek() === ':' && this.indicatorAt(this.i)) {
        if (where === 'map') this.fail('unexpected mapping value', this.i);
        this.i = start;
        return this.mapping(ind);
      }
      this.endLine('a scalar');
      return text;
    }
    if (c === '-' && this.indicatorAt(this.i)) {
      if (where === 'map') this.fail('unexpected sequence');
      return this.sequence(ind);
    }
    if (c === '?' && this.indicatorAt(this.i)) this.fail('mapping key is not a string');
    if (c === ':' && this.indicatorAt(this.i)) this.fail("unexpected ':'");
    // a plain scalar — a mapping when `: ` follows it on the line
    if (this.plainIsKey()) {
      if (where === 'map') this.fail('unexpected mapping value');
      return this.mapping(ind);
    }
    return this.plainScalar(parent);
  }
  anchorName(): string {
    this.i++; // & or *
    const start = this.i;
    while (!this.atEnd() && !isSpace(this.peek()) && !FLOW_END.has(this.peek()!)) this.i++;
    if (this.i === start) this.fail('missing anchor name', start - 1);
    return this.s.slice(start, this.i);
  }
  /** does the plain text at the cursor end in a `: ` on this line (before any comment)? */
  plainIsKey(): boolean {
    let k = this.i;
    for (;;) {
      const c = this.s[k];
      if (isBreakOrEnd(c)) return false;
      if (c === '#' && k > this.i && isSpace(this.s[k - 1])) return false;
      if (c === ':' && this.indicatorAt(k)) return true;
      k++;
    }
  }
  /** the plain text on the current line up to a comment or the line's end (not a key) */
  plainLine(): string {
    const start = this.i;
    let end = this.i;
    for (;;) {
      const c = this.s[end];
      if (isBreakOrEnd(c)) break;
      if (c === '#' && end > start && isSpace(this.s[end - 1])) break;
      if (c === ':' && this.indicatorAt(end)) this.fail("unexpected ':'", end);
      end++;
    }
    this.i = end;
    const text = this.s.slice(start, end).replace(/[ \t]+$/, '');
    this.skipInline();
    return text;
  }
  plainScalar(parent: number): any {
    const at = this.i;
    let text = this.plainLine();
    // continuation lines: indented more than the parent, folded with a
    // space; blank lines between fold to line breaks
    for (;;) {
      const save = this.i;
      let blanks = 0;
      let k = this.i;
      if (this.s[k] !== '\n') break;
      let found = -1;
      while (this.s[k] === '\n') {
        k++;
        let j = k;
        while (this.s[j] === ' ' || this.s[j] === '\t') j++;
        if (this.s[j] === '\n') {
          blanks++;
          k = j;
          continue;
        }
        if (this.s[j] === undefined) break;
        found = j;
        break;
      }
      if (found < 0) break;
      const ind = this.indentOf(found);
      const c = this.s[found];
      if (ind <= parent || c === '#') break;
      if ((c === '-' || c === '?' || c === ':') && this.indicatorAt(found)) break;
      this.i = found;
      if (this.atMarker() || this.plainIsKey()) {
        this.i = save;
        break;
      }
      const more = this.plainLine();
      text += (blanks ? '\n'.repeat(blanks) : ' ') + more;
      if (this.i === save) break;
    }
    const r = resolvePlain(text);
    if (r.k === 'nonfinite') this.fail('non-finite float', at);
    return plainValue(r);
  }
  mapping(ind: number): any {
    const entries: [string, any][] = [];
    const seen = new Set<string>();
    for (;;) {
      const at = this.i;
      const key = this.key();
      if (seen.has(key)) this.fail(`mapping repeats the key ${JSON.stringify(key)}`, at);
      seen.add(key);
      const value = this.node(ind, 'map', ind);
      entries.push([key, value]);
      const n = this.nextContent();
      if (n === -1 || n < ind || this.atMarker()) break;
      if (n > ind) this.fail('bad indentation');
      if (this.peek() === '-' && this.indicatorAt(this.i)) this.fail('unexpected sequence');
    }
    return { __jobj: true, entries };
  }
  /** a mapping key at the cursor, and the `:` after it */
  key(): string {
    const c = this.peek();
    let key: string;
    if (c === '"' || c === "'") key = this.quoted();
    else if (c === '?' && this.indicatorAt(this.i)) this.fail('mapping key is not a string');
    else if (c === '&' || c === '*') this.fail('mapping key is not a string');
    else if (c === '!') this.fail('uses a tag');
    else if (c === '[' || c === '{') this.fail('mapping key is not a string');
    else {
      const start = this.i;
      let end = this.i;
      for (;;) {
        const ch = this.s[end];
        if (isBreakOrEnd(ch)) this.fail("missing ':' after a mapping key", start);
        if (ch === '#' && end > start && isSpace(this.s[end - 1]))
          this.fail("missing ':' after a mapping key", start);
        if (ch === ':' && this.indicatorAt(end)) break;
        end++;
      }
      const text = this.s.slice(start, end).replace(/[ \t]+$/, '');
      const r = resolvePlain(text);
      if (r.k !== 'string') this.fail('mapping key is not a string', start);
      key = text;
      this.i = end;
    }
    while (isSpace(this.peek())) this.i++;
    if (!(this.peek() === ':' && this.indicatorAt(this.i)))
      this.fail("missing ':' after a mapping key");
    this.i++;
    return key;
  }
  sequence(ind: number): any[] {
    const items: any[] = [];
    for (;;) {
      this.i++; // the dash
      items.push(this.node(ind, 'seq', -1));
      const n = this.nextContent();
      if (n === -1 || n < ind || this.atMarker()) break;
      if (n > ind) this.fail('bad indentation');
      if (!(this.peek() === '-' && this.indicatorAt(this.i))) break;
    }
    return items;
  }

  // ---- scalars ----
  /** a single- or double-quoted scalar at the cursor, folded over lines */
  quoted(): string {
    const q = this.peek()!;
    const at = this.i;
    this.i++;
    let out = '';
    for (;;) {
      const c = this.peek();
      if (c === undefined) this.fail('unterminated quoted scalar', at);
      if (c === q) {
        if (q === "'" && this.peek(1) === "'") {
          out += "'";
          this.i += 2;
          continue;
        }
        this.i++;
        return out;
      }
      if (c === '\n') {
        // folding: one break is a space, further breaks are kept
        let breaks = 0;
        while (this.peek() === '\n' || isSpace(this.peek())) {
          if (this.peek() === '\n') breaks++;
          this.i++;
        }
        out = out.replace(/[ \t]+$/, '') + (breaks > 1 ? '\n'.repeat(breaks - 1) : ' ');
        continue;
      }
      if (q === '"' && c === '\\') {
        this.i++;
        out += this.escape();
        continue;
      }
      out += c;
      this.i++;
    }
  }
  escape(): string {
    const c = this.peek();
    const at = this.i - 1;
    this.i++;
    switch (c) {
      case '0':
        return '\0';
      case 'a':
        return '\x07';
      case 'b':
        return '\b';
      case 't':
      case '\t':
        return '\t';
      case 'n':
        return '\n';
      case 'v':
        return '\v';
      case 'f':
        return '\f';
      case 'r':
        return '\r';
      case 'e':
        return '\x1b';
      case ' ':
        return ' ';
      case '"':
        return '"';
      case '/':
        return '/';
      case '\\':
        return '\\';
      case 'N':
        return '\x85';
      case '_':
        return '\xa0';
      case 'L':
        return '\u2028';
      case 'P':
        return '\u2029';
      case 'x':
      case 'u':
      case 'U': {
        const len = c === 'x' ? 2 : c === 'u' ? 4 : 8;
        const hex = this.s.slice(this.i, this.i + len);
        if (!new RegExp(`^[0-9a-fA-F]{${len}}$`).test(hex)) this.fail('bad escape', at);
        this.i += len;
        const cp = parseInt(hex, 16);
        if (cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) this.fail('bad escape', at);
        return String.fromCodePoint(cp);
      }
      case '\n': {
        // an escaped line break joins the lines; leading white space is dropped
        while (isSpace(this.peek())) this.i++;
        return '';
      }
      default:
        this.fail('bad escape', at);
    }
  }
  /** a block scalar (`|` or `>`) with its indicators; `parent` is the enclosing indentation */
  blockScalar(parent: number): string {
    const at = this.i;
    const folded = this.peek() === '>';
    this.i++;
    let chomp: 'clip' | 'strip' | 'keep' = 'clip';
    let explicit = 0;
    for (let k = 0; k < 2; k++) {
      const c = this.peek();
      if (c === '-' || c === '+') {
        if (chomp !== 'clip') this.fail('bad block scalar header', at);
        chomp = c === '-' ? 'strip' : 'keep';
        this.i++;
      } else if (c !== undefined && c >= '1' && c <= '9') {
        if (explicit) this.fail('bad block scalar header', at);
        explicit = c.charCodeAt(0) - 48;
        this.i++;
      }
    }
    this.endLine('a block scalar header');
    // the content lines: those indented at least the content indentation
    // (explicit, or the first non-blank line's), until a lesser one
    const lines: { text: string; blank: boolean }[] = [];
    let indent = explicit ? Math.max(parent, 0) + explicit : -1;
    let k = this.i;
    let endAt = this.i;
    while (this.s[k] === '\n') {
      const start = k + 1;
      let j = start;
      while (this.s[j] === ' ') j++;
      const blank = this.s[j] === '\n' || this.s[j] === undefined;
      const lineIndent = j - start;
      if (blank) {
        let e = j;
        while (this.s[e] !== '\n' && this.s[e] !== undefined) e++;
        lines.push({
          text: indent >= 0 && lineIndent > indent ? ' '.repeat(lineIndent - indent) : '',
          blank: true,
        });
        k = e;
        endAt = e;
        if (this.s[e] === undefined) break;
        continue;
      }
      if (indent < 0) {
        if (lineIndent <= parent) break;
        indent = lineIndent;
        // blank lines before the first content line carry no spaces
        for (const l of lines) l.text = '';
      }
      if (lineIndent < indent) break;
      if (this.s[j] === '\t' && lineIndent === indent) this.fail('tab in indentation', j);
      let e = j;
      while (this.s[e] !== '\n' && this.s[e] !== undefined) e++;
      lines.push({ text: this.s.slice(start + indent, e), blank: false });
      k = e;
      endAt = e;
      if (this.s[e] === undefined) break;
    }
    this.i = endAt;
    // trailing blank lines are the chomping's business
    let last = lines.length;
    while (last > 0 && lines[last - 1].blank) last--;
    const body = lines.slice(0, last);
    const trailing = lines.length - last;
    let text = '';
    if (!folded) text = body.map((l) => l.text).join('\n');
    else {
      // folding: a break between two normal lines is a space, blank lines
      // are kept as breaks, more-indented lines are kept as written
      for (let n = 0; n < body.length; n++) {
        const l = body[n];
        if (n === 0) {
          text = l.text;
          continue;
        }
        const prev = body[n - 1];
        const moreIndented = (x: { text: string; blank: boolean }) =>
          !x.blank && (x.text.startsWith(' ') || x.text.startsWith('\t'));
        if (l.blank) text += '\n' + l.text;
        else if (prev.blank || moreIndented(prev) || moreIndented(l)) text += '\n' + l.text;
        else text += ' ' + l.text;
      }
    }
    if (body.length === 0) return chomp === 'keep' ? '\n'.repeat(trailing) : '';
    if (chomp === 'strip') return text;
    if (chomp === 'clip') return text + '\n';
    return text + '\n'.repeat(trailing + 1);
  }

  // ---- flow nodes ----
  flowWs(): void {
    for (;;) {
      const c = this.peek();
      if (c === ' ' || c === '\t' || c === '\n') {
        this.i++;
        continue;
      }
      if (
        c === '#' &&
        (this.i === 0 || isSpace(this.s[this.i - 1]) || this.s[this.i - 1] === '\n')
      ) {
        while (!this.atEnd()) this.i++;
        continue;
      }
      return;
    }
  }
  flowNode(): any {
    this.flowWs();
    const c = this.peek();
    if (c === undefined) this.fail('unterminated flow collection');
    if (c === '&') {
      const name = this.anchorName();
      const v = this.flowNode();
      this.anchors.set(name, v);
      return v;
    }
    if (c === '*') {
      const at = this.i;
      const name = this.anchorName();
      if (!this.anchors.has(name)) this.fail(`unknown alias *${name}`, at);
      return copyOf(this.anchors.get(name));
    }
    if (c === '!') this.fail('uses a tag');
    if (c === '[') {
      const at = this.i;
      this.i++;
      const items: any[] = [];
      for (;;) {
        this.flowWs();
        if (this.peek() === ']') {
          this.i++;
          return items;
        }
        if (this.peek() === undefined) this.fail('unterminated flow collection', at);
        if (this.peek() === ',') this.fail("unexpected ','");
        items.push(this.flowNode());
        this.flowWs();
        if (this.peek() === ':') this.fail("unexpected ':'");
        if (this.peek() === ',') {
          this.i++;
          continue;
        }
        if (this.peek() === ']') continue;
        this.fail("expected ',' or ']'");
      }
    }
    if (c === '{') {
      const at = this.i;
      this.i++;
      const entries: [string, any][] = [];
      const seen = new Set<string>();
      for (;;) {
        this.flowWs();
        if (this.peek() === '}') {
          this.i++;
          return { __jobj: true, entries };
        }
        if (this.peek() === undefined) this.fail('unterminated flow collection', at);
        if (this.peek() === ',') this.fail("unexpected ','");
        const keyAt = this.i;
        const key = this.flowNode();
        if (typeof key !== 'string') this.fail('mapping key is not a string', keyAt);
        if (seen.has(key)) this.fail(`mapping repeats the key ${JSON.stringify(key)}`, keyAt);
        seen.add(key);
        this.flowWs();
        let value: any = null;
        if (this.peek() === ':') {
          this.i++;
          this.flowWs();
          if (this.peek() !== ',' && this.peek() !== '}') value = this.flowNode();
          this.flowWs();
        }
        entries.push([key, value]);
        if (this.peek() === ',') {
          this.i++;
          continue;
        }
        if (this.peek() === '}') continue;
        this.fail("expected ',' or '}'");
      }
    }
    if (c === '"' || c === "'") return this.quoted();
    if (c === ']' || c === '}') this.fail(`unexpected '${c}'`);
    // a plain scalar in flow context: ends at an indicator, folded over lines
    const at = this.i;
    let text = '';
    for (;;) {
      const start = this.i;
      let end = this.i;
      for (;;) {
        const ch = this.s[end];
        if (isBreakOrEnd(ch) || FLOW_END.has(ch)) break;
        if (ch === '#' && end > start && isSpace(this.s[end - 1])) break;
        if (
          ch === ':' &&
          (isSpace(this.s[end + 1]) ||
            isBreakOrEnd(this.s[end + 1]) ||
            FLOW_END.has(this.s[end + 1]))
        )
          break;
        end++;
      }
      text += this.s.slice(start, end).replace(/[ \t]+$/, '');
      this.i = end;
      // a line break inside the scalar folds to a space
      let k = end;
      while (isSpace(this.s[k])) k++;
      if (this.s[k] === '#') break;
      if (this.s[k] !== '\n') break;
      let blanks = 0;
      while (this.s[k] === '\n' || isSpace(this.s[k])) {
        if (this.s[k] === '\n') blanks++;
        k++;
      }
      const ch = this.s[k];
      if (
        ch === undefined ||
        FLOW_END.has(ch) ||
        ch === '#' ||
        (ch === ':' && this.indicatorAt(k))
      ) {
        this.i = k;
        break;
      }
      text += blanks > 1 ? '\n'.repeat(blanks - 1) : ' ';
      this.i = k;
    }
    if (text === '') this.fail('unexpected content', at);
    const r = resolvePlain(text);
    if (r.k === 'nonfinite') this.fail('non-finite float', at);
    return plainValue(r);
  }
}

function plainValue(r: Plain): any {
  switch (r.k) {
    case 'null':
      return null;
    case 'bool':
      return r.v;
    case 'int':
      return r.v;
    case 'float':
      return r.v;
    case 'string':
      return r.v;
    default:
      return undefined;
  }
}
// an alias is a copy of the anchored value
function copyOf(v: any): any {
  if (Array.isArray(v)) return v.map(copyOf);
  if (v && v.__jobj)
    return { __jobj: true, entries: v.entries.map(([k, x]: any) => [k, copyOf(x)]) };
  return v;
}

/** read one YAML document (§2) into the JSON data model; throws YamlError */
export function readYaml(src: string): any {
  return new Reader(src).document();
}

// ---------------- the writer (§4.2) ----------------
const fmtFloat = (n: number): string => {
  const s = String(n);
  return /[.eE]/.test(s) ? s : s + '.0';
};
// the YAML 1.1 spellings a 1.1 reader would take for a bool or a null
const YAML11_WORDS = new Set([
  'y',
  'Y',
  'yes',
  'Yes',
  'YES',
  'n',
  'N',
  'no',
  'No',
  'NO',
  'on',
  'On',
  'ON',
  'off',
  'Off',
  'OFF',
]);
/**
 * plain only when a YAML 1.2 reader reads it back as exactly this string
 * and a YAML 1.1 reader has nothing to reinterpret: it starts with a
 * letter or `_`, holds no indicator that could open a collection, an
 * anchor, a tag, or a comment, no `: `, no `#`, no break, tab, or
 * unprintable character, does not end in `:` or a space, and is not a
 * word either schema reads as a bool or a null
 */
export function plainSafe(s: string): boolean {
  if (!/^[A-Za-z_]/.test(s)) return false;
  if (YAML11_WORDS.has(s) || resolvePlain(s).k !== 'string') return false;
  if (s.endsWith(':') || s.endsWith(' ')) return false;
  if (s.includes(': ') || s.includes(' #')) return false;
  for (const ch of s) {
    const cp = ch.codePointAt(0)!;
    if ('[]{},&*!|>\'"%@`#'.includes(ch)) return false;
    if (cp < 0x20 || cp === 0x7f || (cp >= 0x80 && cp <= 0x9f)) return false;
    if (cp === 0xfeff || cp === 0xfffe || cp === 0xffff || (cp >= 0xd800 && cp <= 0xdfff))
      return false;
  }
  return true;
}
const yamlStr = (s: string): string => (plainSafe(s) ? s : JSON.stringify(s));
const isEmptyColl = (v: any) =>
  (Array.isArray(v) && v.length === 0) || (v && v.__jobj && v.entries.length === 0);
const isBlock = (v: any) => (Array.isArray(v) || (v && v.__jobj)) && !isEmptyColl(v);

function scalarText(v: any): string {
  if (v === null) return 'null';
  if (typeof v === 'boolean') return String(v);
  if (typeof v === 'bigint') return v.toString();
  if (typeof v === 'number') return fmtFloat(v);
  if (typeof v === 'string') return yamlStr(v);
  if (Array.isArray(v)) return '[]';
  if (v && v.__jobj) return '{}';
  throw new Error('toYaml: unexpected value');
}
// the lines of a block node: the first without its indentation (the
// caller places it after `- ` or on a line of its own), the rest with
// `ind` in front of them
function blockLines(v: any, ind: string, step: string): string[] {
  const out: string[] = [];
  if (Array.isArray(v)) {
    for (const item of v) {
      const sub = isBlock(item) ? blockLines(item, ind + '  ', step) : [scalarText(item)];
      out.push((out.length ? ind : '') + '- ' + sub[0], ...sub.slice(1));
    }
    return out;
  }
  for (const [k, x] of v.entries) {
    const key = yamlStr(k);
    if (!isBlock(x)) {
      out.push((out.length ? ind : '') + `${key}: ${scalarText(x)}`);
      continue;
    }
    out.push((out.length ? ind : '') + `${key}:`);
    const sub = blockLines(x, ind + step, step);
    out.push(ind + step + sub[0], ...sub.slice(1));
  }
  return out;
}
/** the YAML text of a JSON value (readJson's shape), block style, no trailing newline */
export function toYaml(v: any, indent = 2): string {
  const step = ' '.repeat(indent);
  return isBlock(v) ? blockLines(v, '', step).join('\n') : scalarText(v);
}

// ---------------- the JSON layouts (§4.1) ----------------
/** the JSON text of a value (readJson's shape): canonical for indent 0, laid out with `indent` spaces per level otherwise */
export function toJson(v: any, indent = 0): string {
  const go = (x: any, ind: string): string => {
    if (x === null) return 'null';
    if (typeof x === 'boolean') return String(x);
    if (typeof x === 'bigint') return x.toString();
    if (typeof x === 'number') return fmtFloat(x);
    if (typeof x === 'string') return JSON.stringify(x);
    const inner = ind + ' '.repeat(indent);
    if (Array.isArray(x)) {
      if (x.length === 0) return '[]';
      if (indent === 0) return `[${x.map((e) => go(e, ind)).join(',')}]`;
      return `[\n${x.map((e) => inner + go(e, inner)).join(',\n')}\n${ind}]`;
    }
    if (x && x.__jobj) {
      if (x.entries.length === 0) return '{}';
      if (indent === 0)
        return `{${x.entries.map(([k, e]: any) => `${JSON.stringify(k)}:${go(e, ind)}`).join(',')}}`;
      return `{\n${x.entries
        .map(([k, e]: any) => `${inner}${JSON.stringify(k)}: ${go(e, inner)}`)
        .join(',\n')}\n${ind}}`;
    }
    throw new Error('toJson: unexpected value');
  };
  return go(v, '');
}
