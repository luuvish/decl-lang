// Lexer + parser for the spike subset. Throwaway (ROADMAP §0.6).

export type Tok = {
  kind: string;            // ident, int, float, unitlit, string, template, pattern, op, kw, eof
  v?: any;
  nl: boolean;             // a line break precedes this token
  pos: number;
};

const KEYWORDS = new Set([
  'type','const','func','output','input','export','import','from','as',
  'dimension','unit','diagnostic','assert','when',
  'if','then','else','match','for','in','matches','with','true','false','null',
]);

const OPS = [
  '...','..<','..','?.','??','=>','|>','<<','>>','&&','||','==','!=','<=','>=',
  '{','}','[',']','(',')','<','>',',',':','=','?','.','|','&','^','~','!','+','-','*','/','%','@',
];

export function lex(src: string): Tok[] {
  const toks: Tok[] = [];
  let i = 0, nl = false;
  const isIdStart = (c: string) => /[_A-Za-z]/.test(c);
  const isId = (c: string) => /[_A-Za-z0-9]/.test(c);
  const isDigit = (c: string) => c >= '0' && c <= '9';
  const prevMeaningful = () => toks.length ? toks[toks.length - 1] : null;

  while (i < src.length) {
    const c = src[i];
    if (c === '\n') { nl = true; i++; continue; }
    if (c === ' ' || c === '\t' || c === '\r') { i++; continue; }
    if (c === '/' && src[i + 1] === '/') { while (i < src.length && src[i] !== '\n') i++; continue; }
    if (c === '/' && src[i + 1] === '*') {
      let depth = 1; i += 2;
      while (i < src.length && depth > 0) {
        if (src[i] === '/' && src[i + 1] === '*') { depth++; i += 2; }
        else if (src[i] === '*' && src[i + 1] === '/') { depth--; i += 2; }
        else i++;
      }
      continue;
    }
    // pattern literal: '/' where a type/operand may begin
    if (c === '/') {
      const p = prevMeaningful();
      const patternPos = !p || p.nl === undefined ||
        (p.kind === 'op' && ['=','|','&',':',',','(','[','{'].includes(p.v)) ||
        (p.kind === 'kw' && p.v === 'matches') ||
        (p.kind === 'op' && p.v === '..');
      if (patternPos) {
        let j = i + 1, body = '';
        while (j < src.length && src[j] !== '/') {
          if (src[j] === '\\' && src[j + 1] === '/') { body += '/'; j += 2; }
          else { body += src[j]; j++; }
        }
        toks.push({ kind: 'pattern', v: body, nl, pos: i }); nl = false; i = j + 1; continue;
      }
    }
    if (isDigit(c)) {
      let j = i, isFloat = false, text = '';
      if (c === '0' && (src[i + 1] === 'x' || src[i + 1] === 'o' || src[i + 1] === 'b')) {
        const base = src[i + 1] === 'x' ? 16 : src[i + 1] === 'o' ? 8 : 2;
        j = i + 2; let digits = '';
        while (j < src.length && /[0-9a-fA-F_]/.test(src[j])) { if (src[j] !== '_') digits += src[j]; j++; }
        toks.push({ kind: 'int', v: BigInt((base === 16 ? '0x' : base === 8 ? '0o' : '0b') + digits), nl, pos: i });
        nl = false; i = j; continue;
      }
      while (j < src.length && /[0-9_]/.test(src[j])) { if (src[j] !== '_') text += src[j]; j++; }
      if (src[j] === '.' && isDigit(src[j + 1])) {
        isFloat = true; text += '.'; j++;
        while (j < src.length && /[0-9_]/.test(src[j])) { if (src[j] !== '_') text += src[j]; j++; }
      }
      if (src[j] === 'e' || src[j] === 'E') {
        isFloat = true; text += 'e'; j++;
        if (src[j] === '+' || src[j] === '-') { text += src[j]; j++; }
        while (j < src.length && isDigit(src[j])) { text += src[j]; j++; }
      }
      if (j < src.length && isIdStart(src[j])) {          // unit literal
        let u = ''; while (j < src.length && isId(src[j])) { u += src[j]; j++; }
        toks.push({ kind: 'unitlit', v: { num: isFloat ? parseFloat(text) : Number(text), unit: u }, nl, pos: i });
      } else {
        toks.push(isFloat ? { kind: 'float', v: parseFloat(text), nl, pos: i }
                          : { kind: 'int', v: BigInt(text), nl, pos: i });
      }
      nl = false; i = j; continue;
    }
    if (isIdStart(c)) {
      let j = i, w = '';
      while (j < src.length && isId(src[j])) { w += src[j]; j++; }
      toks.push({ kind: KEYWORDS.has(w) ? 'kw' : 'ident', v: w, nl, pos: i });
      nl = false; i = j; continue;
    }
    if (c === '$') {
      let j = i + 1, w = '';
      while (j < src.length && isId(src[j])) { w += src[j]; j++; }
      toks.push({ kind: 'ctx', v: '$' + w, nl, pos: i }); nl = false; i = j; continue;
    }
    if (c === '"') {
      let j = i + 1, s = '';
      while (j < src.length && src[j] !== '"') {
        if (src[j] === '\\') {
          const e = src[j + 1];
          s += e === 'n' ? '\n' : e === 't' ? '\t' : e === 'r' ? '\r' : e === 'b' ? '\b' : e === 'f' ? '\f' : e;
          j += 2;
        } else { s += src[j]; j++; }
      }
      toks.push({ kind: 'string', v: s, nl, pos: i }); nl = false; i = j + 1; continue;
    }
    if (c === '`') {
      // template: parts = string | Tok[] (interpolation)
      const parts: (string | Tok[])[] = [];
      let j = i + 1, text = '';
      while (j < src.length && src[j] !== '`') {
        if (src[j] === '\\') { text += src[j + 1]; j += 2; continue; }
        if (src[j] === '$' && src[j + 1] === '{') {
          if (text) { parts.push(text); text = ''; }
          let depth = 1, k = j + 2, inner = '';
          while (k < src.length && depth > 0) {
            if (src[k] === '{') depth++;
            if (src[k] === '}') { depth--; if (depth === 0) break; }
            inner += src[k]; k++;
          }
          parts.push(lex(inner)); j = k + 1; continue;
        }
        text += src[j]; j++;
      }
      if (text) parts.push(text);
      toks.push({ kind: 'template', v: parts, nl, pos: i }); nl = false; i = j + 1; continue;
    }
    const op = OPS.find(o => src.startsWith(o, i));
    if (op) { toks.push({ kind: 'op', v: op, nl, pos: i }); nl = false; i += op.length; continue; }
    throw new Error(`lex: unknown character '${c}' at ${i}`);
  }
  toks.push({ kind: 'eof', nl: true, pos: i });
  return toks;
}

// ---------------- AST ----------------

export type TypeAst =
  | { k: 'prim'; name: string }
  | { k: 'lit'; v: any }
  | { k: 'range'; lo: any; hi: any; excl: boolean }
  | { k: 'pattern'; re: string }
  | { k: 'record'; members: MemberAst[]; open: boolean }
  | { k: 'map'; key: TypeAst; val: TypeAst }
  | { k: 'array'; elem: TypeAst; lo?: number; hi?: number }
  | { k: 'union'; arms: TypeAst[] }
  | { k: 'isect'; arms: TypeAst[] }
  | { k: 'named'; name: string; args: TypeAst[]; ext?: TypeAst };

export type MemberAst =
  | { m: 'value'; name: string; opt: boolean; type: TypeAst; dflt?: Expr }
  | { m: 'derived'; name: string; type?: TypeAst; expr: Expr }
  | { m: 'assert'; name: string; cond: Expr; tail?: ElseTail }
  | { m: 'when'; cond: Expr; body: MemberAst[] };

export type ElseTail =
  | { t: 'inline'; severity: string; template: (string | Tok[])[] }
  | { t: 'ref'; name: string; args: Expr[] };

export type Expr =
  | { e: 'lit'; v: any }
  | { e: 'unitlit'; num: number; unit: string }
  | { e: 'template'; parts: (string | Expr)[] }
  | { e: 'name'; name: string }
  | { e: 'ctx'; name: string }
  | { e: 'referrers'; type: string; member: string }
  | { e: 'obj'; entries: { key: string; val: Expr }[]; spreads: never[] }
  | { e: 'arr'; items: ({ spread: boolean; expr: Expr })[] }
  | { e: 'comp'; head: Expr; clauses: { v: string; iter: Expr; filters: Expr[] }[] }
  | { e: 'bin'; op: string; l: Expr; r: Expr }
  | { e: 'un'; op: string; x: Expr }
  | { e: 'if'; c: Expr; t: Expr; f: Expr }
  | { e: 'lambda'; params: string[]; body: Expr }
  | { e: 'call'; fn: Expr; args: Expr[] }
  | { e: 'member'; x: Expr; name: string }
  | { e: 'index'; x: Expr; i: Expr }
  | { e: 'with'; base: Expr; patch: Expr };

export type Decl =
  | { d: 'type'; name: string; type: TypeAst; tail?: ElseTail }
  | { d: 'const'; name: string; type?: TypeAst; expr: Expr }
  | { d: 'output'; name: string; type: TypeAst; expr: Expr }
  | { d: 'input'; name: string; type: TypeAst; fallback?: Expr }
  | { d: 'diagnostic'; name: string; params: { name: string; type: TypeAst }[]; severity: string; template: (string | Tok[])[] };

export class Parser {
  toks: Tok[]; i = 0;
  pd = 0;                 // paren depth: inside ( … ) newlines are whitespace (§2.9)
  constructor(toks: Tok[]) { this.toks = toks; }
  peek(k = 0) { return this.toks[this.i + k]; }
  next() { return this.toks[this.i++]; }
  at(kind: string, v?: any) { const t = this.peek(); return t.kind === kind && (v === undefined || t.v === v); }
  eat(kind: string, v?: any): Tok {
    if (!this.at(kind, v)) throw new Error(`parse: expected ${kind} ${v ?? ''}, got ${this.peek().kind} '${this.peek().v}' at ${this.peek().pos}`);
    return this.next();
  }
  opt(kind: string, v?: any) { if (this.at(kind, v)) { this.next(); return true; } return false; }
  sep() {           // element separator: comma or significant newline
    if (this.opt('op', ',')) return true;
    if (this.peek().nl) return true;
    return false;
  }

  module(): Decl[] {
    const decls: Decl[] = [];
    while (!this.at('eof')) {
      this.opt('kw', 'export');
      if (this.at('kw', 'type')) decls.push(this.typeDecl());
      else if (this.at('kw', 'const')) decls.push(this.constDecl());
      else if (this.at('kw', 'output')) decls.push(this.rootDecl('output'));
      else if (this.at('kw', 'input')) decls.push(this.rootDecl('input'));
      else if (this.at('kw', 'diagnostic')) decls.push(this.diagDecl());
      else throw new Error(`parse: unexpected ${this.peek().kind} '${this.peek().v}' at module level (pos ${this.peek().pos})`);
    }
    return decls;
  }

  typeDecl(): Decl {
    this.eat('kw', 'type');
    const name = this.eat('ident').v;
    this.eat('op', '=');
    const type = this.type();
    let tail: ElseTail | undefined;
    if (this.at('kw', 'else')) tail = this.elseTail();
    return { d: 'type', name, type, tail };
  }
  constDecl(): Decl {
    this.eat('kw', 'const');
    const name = this.eat('ident').v;
    let type: TypeAst | undefined;
    if (this.opt('op', ':')) type = this.type();
    this.eat('op', '=');
    return { d: 'const', name, type, expr: this.expr() };
  }
  rootDecl(d: 'output' | 'input'): Decl {
    this.eat('kw', d);
    const name = this.eat('ident').v;
    this.eat('op', ':');
    const type = this.type();
    if (d === 'output') { this.eat('op', '='); return { d, name, type, expr: this.expr() }; }
    let fallback: Expr | undefined;
    if (this.opt('op', '=')) fallback = this.expr();
    return { d, name, type, fallback };
  }
  diagDecl(): Decl {
    this.eat('kw', 'diagnostic');
    const name = this.eat('ident').v;
    this.eat('op', '(');
    const params: { name: string; type: TypeAst }[] = [];
    while (!this.at('op', ')')) {
      const pn = this.eat('ident').v; this.eat('op', ':');
      params.push({ name: pn, type: this.type() });
      this.opt('op', ',');
    }
    this.eat('op', ')'); this.eat('op', '{');
    this.eat('ident', 'severity'); this.eat('op', '=');
    const sev = this.eat('ident').v;           // error/warn/info: contextual, lexed as ident
    this.opt('op', ',');
    this.eat('ident', 'message'); this.eat('op', '=');
    const tmpl = this.eat('template').v;
    this.opt('op', ','); this.eat('op', '}');
    return { d: 'diagnostic', name, params, severity: sev, template: tmpl };
  }
  elseTail(): ElseTail {
    this.eat('kw', 'else');
    if (this.at('ident') && ['error', 'warn', 'info'].includes(this.peek().v) && this.peek(1).kind === 'template') {
      const severity = this.next().v;
      return { t: 'inline', severity, template: this.eat('template').v };
    }
    const name = this.eat('ident').v;
    const args: Expr[] = [];
    if (this.opt('op', '(')) {
      while (!this.at('op', ')')) { args.push(this.expr()); this.opt('op', ','); }
      this.eat('op', ')');
    }
    return { t: 'ref', name, args };
  }

  // ---------------- types ----------------
  type(): TypeAst {
    let t = this.isectType();
    if (this.at('op', '|')) {
      const arms = [t];
      while (this.opt('op', '|')) arms.push(this.isectType());
      return { k: 'union', arms };
    }
    return t;
  }
  isectType(): TypeAst {
    let t = this.suffixType();
    if (this.at('op', '&')) {
      const arms = [t];
      while (this.opt('op', '&')) arms.push(this.suffixType());
      return { k: 'isect', arms };
    }
    return t;
  }
  suffixType(): TypeAst {
    let t = this.primaryType();
    for (;;) {
      if (this.at('op', '?') ) { this.next(); t = { k: 'union', arms: [t, { k: 'prim', name: 'null' }] }; continue; }
      if (this.at('op', '[') && !this.peek().nl) {
        this.next();
        if (this.opt('op', ']')) { t = { k: 'array', elem: t }; continue; }
        const lo = this.numLit();
        if (this.at('op', '..') || this.at('op', '..<')) {
          const excl = this.next().v === '..<';
          const hi = this.numLit();
          this.eat('op', ']');
          t = { k: 'array', elem: t, lo: Number(lo), hi: Number(hi) - (excl ? 1 : 0) };
        } else {
          this.eat('op', ']');
          t = { k: 'array', elem: t, lo: Number(lo), hi: Number(lo) };
        }
        continue;
      }
      break;
    }
    return t;
  }
  numLit(): any {
    let neg = this.opt('op', '-');
    const t = this.next();
    if (t.kind !== 'int' && t.kind !== 'float') throw new Error(`parse: expected number at ${t.pos}`);
    return neg ? (typeof t.v === 'bigint' ? -t.v : -t.v) : t.v;
  }
  primaryType(): TypeAst {
    const t = this.peek();
    if (t.kind === 'string') { this.next(); return { k: 'lit', v: t.v }; }
    if (t.kind === 'kw' && ['true', 'false', 'null'].includes(t.v)) {
      this.next(); return t.v === 'null' ? { k: 'prim', name: 'null' } : { k: 'lit', v: t.v === 'true' };
    }
    if (t.kind === 'pattern') { this.next(); return { k: 'pattern', re: t.v }; }
    if (t.kind === 'int' || t.kind === 'float' || (t.kind === 'op' && t.v === '-')) {
      const lo = this.numLit();
      if (this.at('op', '..') || this.at('op', '..<')) {
        const excl = this.next().v === '..<';
        const hi = this.numLit();
        return { k: 'range', lo, hi, excl };
      }
      return { k: 'lit', v: lo };
    }
    if (t.kind === 'op' && t.v === '{') {
      this.next();
      if (this.at('op', '[')) {          // map type
        this.next();
        const key = this.type();
        this.eat('op', ']'); this.eat('op', ':');
        const val = this.type();
        this.eat('op', '}');
        return { k: 'map', key, val };
      }
      return this.recordBody();
    }
    if (t.kind === 'op' && t.v === '(') { this.next(); const inner = this.type(); this.eat('op', ')'); return inner; }
    if (t.kind === 'ident') {
      const name = this.next().v;
      const args: TypeAst[] = [];
      if (this.at('op', '<')) {
        this.next();
        args.push(this.type());
        while (this.opt('op', ',')) args.push(this.type());
        this.eat('op', '>');
      }
      let ext: TypeAst | undefined;
      if (this.at('op', '{')) { this.next(); ext = this.recordBody(); }
      const prim = ['int', 'uint', 'float', 'bool', 'string'];
      if (prim.includes(name) && args.length === 0 && !ext) return { k: 'prim', name };
      return { k: 'named', name, args, ext };
    }
    throw new Error(`parse: unexpected ${t.kind} '${t.v}' in type at ${t.pos}`);
  }
  recordBody(): TypeAst {        // '{' already consumed
    const members: MemberAst[] = [];
    let open = false;
    while (!this.at('op', '}')) {
      if (this.at('op', '...')) { this.next(); open = true; this.opt('op', ','); continue; }
      members.push(this.member());
      this.opt('op', ',');
    }
    this.eat('op', '}');
    return { k: 'record', members, open };
  }
  member(): MemberAst {
    if (this.at('kw', 'const')) {
      this.next();
      const name = this.at('string') ? this.next().v : this.eat('ident').v;
      let type: TypeAst | undefined;
      if (this.opt('op', ':')) type = this.type();
      this.eat('op', '=');
      return { m: 'derived', name, type, expr: this.expr() };
    }
    if (this.at('kw', 'assert')) {
      this.next();
      const name = this.eat('ident').v;
      this.eat('op', ':');
      const cond = this.expr();
      let tail: ElseTail | undefined;
      if (this.at('kw', 'else')) tail = this.elseTail();
      return { m: 'assert', name, cond, tail };
    }
    if (this.at('kw', 'when')) {
      this.next();
      const cond = this.expr();
      this.eat('op', '{');
      const body: MemberAst[] = [];
      while (!this.at('op', '}')) { body.push(this.member()); this.opt('op', ','); }
      this.eat('op', '}');
      return { m: 'when', cond, body };
    }
    const name = this.at('string') ? this.next().v : this.eat('ident').v;
    const opt = this.opt('op', '?');
    this.eat('op', ':');
    const type = this.type();
    let dflt: Expr | undefined;
    if (this.opt('op', '=')) dflt = this.expr();
    return { m: 'value', name, opt, type, dflt };
  }

  // ---------------- expressions ----------------
  expr(): Expr {
    // lambda lookahead: '(' idents ')' '=>'
    if (this.at('op', '(')) {
      let j = this.i + 1, depth = 1;
      while (j < this.toks.length && depth > 0) {
        if (this.toks[j].kind === 'op' && this.toks[j].v === '(') depth++;
        if (this.toks[j].kind === 'op' && this.toks[j].v === ')') depth--;
        j++;
      }
      if (this.toks[j]?.kind === 'op' && this.toks[j].v === '=>') {
        this.next();
        const params: string[] = [];
        while (!this.at('op', ')')) { params.push(this.eat('ident').v); this.opt('op', ','); if (this.opt('op', ':')) this.type(); }
        this.eat('op', ')'); this.eat('op', '=>');
        return { e: 'lambda', params, body: this.expr() };
      }
    }
    if (this.at('kw', 'if')) {
      this.next();
      const c = this.expr(); this.eat('kw', 'then');
      const t = this.expr(); this.eat('kw', 'else');
      return { e: 'if', c, t, f: this.expr() };
    }
    return this.nullish();
  }
  bin(sub: () => Expr, ops: string[], kwOps: string[] = []): Expr {
    let l = sub.call(this);
    for (;;) {
      const t = this.peek();
      const isOp = t.kind === 'op' && ops.includes(t.v);
      const isKw = t.kind === 'kw' && kwOps.includes(t.v);
      if (!isOp && !isKw) break;
      if (t.nl && this.pd === 0) break;   // outside parens, a leading-operator line separates
      this.next();
      const r = sub.call(this);
      l = { e: 'bin', op: t.v, l, r };
    }
    return l;
  }
  nullish(): Expr { return this.bin(this.orE, ['??']); }
  orE(): Expr { return this.bin(this.andE, ['||']); }
  andE(): Expr { return this.bin(this.bitOr, ['&&']); }
  bitOr(): Expr { return this.bin(this.bitXor, ['|']); }
  bitXor(): Expr { return this.bin(this.bitAnd, ['^']); }
  bitAnd(): Expr { return this.bin(this.eqE, ['&']); }
  eqE(): Expr {
    let l = this.cmpE();
    const t = this.peek();
    if (t.kind === 'op' && ['==', '!='].includes(t.v)) { this.next(); return { e: 'bin', op: t.v, l, r: this.cmpE() }; }
    return l;
  }
  cmpE(): Expr {
    let l = this.rangeE();
    const t = this.peek();
    if (t.kind === 'op' && ['<', '<=', '>', '>='].includes(t.v)) { this.next(); return { e: 'bin', op: t.v, l, r: this.rangeE() }; }
    if (t.kind === 'kw' && t.v === 'in') { this.next(); return { e: 'bin', op: 'in', l, r: this.rangeE() }; }
    return l;
  }
  rangeE(): Expr {
    let l = this.shiftE();
    const t = this.peek();
    if (t.kind === 'op' && ['..', '..<'].includes(t.v)) { this.next(); return { e: 'bin', op: t.v, l, r: this.shiftE() }; }
    return l;
  }
  shiftE(): Expr { return this.bin(this.addE, ['<<', '>>']); }
  addE(): Expr {
    let l = this.mulE();
    for (;;) {
      const t = this.peek();
      if (t.kind === 'op' && ['+', '-'].includes(t.v) && (!t.nl || this.pd > 0)) {
        this.next(); l = { e: 'bin', op: t.v, l, r: this.mulE() }; continue;
      }
      break;
    }
    return l;
  }
  mulE(): Expr { return this.bin(this.unE, ['*', '/', '%']); }
  unE(): Expr {
    const t = this.peek();
    if (t.kind === 'op' && ['!', '-', '~'].includes(t.v)) { this.next(); return { e: 'un', op: t.v, x: this.unE() }; }
    return this.withE();
  }
  withE(): Expr {
    let x = this.postfix();
    while (this.at('kw', 'with')) { this.next(); x = { e: 'with', base: x, patch: this.postfix() }; }
    return x;
  }
  postfix(): Expr {
    let x = this.primary();
    for (;;) {
      if (this.at('op', '.') && !this.peek().nl) {
        this.next();
        const n = this.at('string') ? this.next().v : this.eat('ident').v;
        x = { e: 'member', x, name: n };
        continue;
      }
      if (this.at('op', '[') && !this.peek().nl) { this.next(); const i = this.expr(); this.eat('op', ']'); x = { e: 'index', x, i }; continue; }
      if (this.at('op', '(') && !this.peek().nl) {
        this.next(); this.pd++;
        const args: Expr[] = [];
        while (!this.at('op', ')')) { args.push(this.expr()); this.opt('op', ','); }
        this.pd--; this.eat('op', ')');
        x = { e: 'call', fn: x, args };
        continue;
      }
      break;
    }
    return x;
  }
  primary(): Expr {
    const t = this.peek();
    if (t.kind === 'int' || t.kind === 'float') { this.next(); return { e: 'lit', v: t.v }; }
    if (t.kind === 'string') { this.next(); return { e: 'lit', v: t.v }; }
    if (t.kind === 'unitlit') { this.next(); return { e: 'unitlit', num: t.v.num, unit: t.v.unit }; }
    if (t.kind === 'kw' && ['true', 'false', 'null'].includes(t.v)) {
      this.next(); return { e: 'lit', v: t.v === 'true' ? true : t.v === 'false' ? false : null };
    }
    if (t.kind === 'template') {
      this.next();
      const parts = (t.v as (string | Tok[])[]).map(p =>
        typeof p === 'string' ? p : new Parser(p.concat([{ kind: 'eof', nl: true, pos: 0 }])).expr());
      return { e: 'template', parts };
    }
    if (t.kind === 'ctx') {
      this.next();
      if (t.v === '$referrers') {
        this.eat('op', '(');
        const ty = this.eat('ident').v; this.eat('op', ',');
        const m = this.eat('string').v;
        this.eat('op', ')');
        return { e: 'referrers', type: ty, member: m };
      }
      return { e: 'ctx', name: t.v };
    }
    if (t.kind === 'ident') { this.next(); return { e: 'name', name: t.v }; }
    if (t.kind === 'op' && t.v === '(') {
      this.next(); this.pd++;
      const x = this.expr();
      this.pd--; this.eat('op', ')');
      return x;
    }
    if (t.kind === 'op' && t.v === '[') {
      this.next();
      if (this.at('op', ']')) { this.next(); return { e: 'arr', items: [] }; }
      const first = this.at('op', '...') ? (this.next(), { spread: true, expr: this.expr() }) : { spread: false, expr: this.expr() };
      if (!first.spread && this.at('kw', 'for')) {
        const clauses: { v: string; iter: Expr; filters: Expr[] }[] = [];
        while (this.at('kw', 'for')) {
          this.next();
          const v = this.eat('ident').v; this.eat('kw', 'in');
          const iter = this.expr();
          const filters: Expr[] = [];
          while (this.at('kw', 'if')) { this.next(); filters.push(this.expr()); }
          clauses.push({ v, iter, filters });
        }
        this.eat('op', ']');
        return { e: 'comp', head: first.expr, clauses };
      }
      const items = [first];
      while (this.sep() && !this.at('op', ']')) {
        if (this.at('op', ']')) break;
        items.push(this.at('op', '...') ? (this.next(), { spread: true, expr: this.expr() }) : { spread: false, expr: this.expr() });
      }
      this.eat('op', ']');
      return { e: 'arr', items };
    }
    if (t.kind === 'op' && t.v === '{') {
      this.next();
      const entries: { key: string; val: Expr }[] = [];
      while (!this.at('op', '}')) {
        const key = this.at('string') ? this.next().v : this.eat('ident').v;
        this.eat('op', ':');
        entries.push({ key, val: this.expr() });
        this.opt('op', ',');
      }
      this.eat('op', '}');
      return { e: 'obj', entries, spreads: [] as never[] };
    }
    throw new Error(`parse: unexpected ${t.kind} '${t.v}' in expression at ${t.pos}`);
  }
}

export function parseModule(src: string): Decl[] {
  return new Parser(lex(src)).module();
}
