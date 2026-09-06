// CST -> AST: lower the canonical tree-sitter parse tree into the AST
// of ast.ts. This is the reference implementation's only parser front
// end (ROADMAP: tree-sitter is the single canonical parser).
import { Parser, Language, Node } from 'web-tree-sitter';
import type {
  Annotation,
  Decl,
  ElseTail,
  Expr,
  Loc,
  MemberAst,
  TemplateParts,
  TypeAst,
} from './ast.ts';

// This module is platform-neutral: it does not know where the grammar
// wasm lives. Node callers use node.ts (which locates it on disk);
// browsers pass URLs. Everything else here runs anywhere.
let language: Language | null = null;
/** `grammar`: the tree-sitter-decl.wasm path, URL, or bytes; `runtime`: web-tree-sitter's own tree-sitter.wasm (a path or URL, or its bytes) */
export type ParserOptions = { grammar: string | Uint8Array; runtime?: string | Uint8Array };
export async function initParser(opts: ParserOptions): Promise<void> {
  if (language) return;
  const rt = opts.runtime;
  await Parser.init(
    rt === undefined
      ? undefined
      : typeof rt === 'string'
        ? { locateFile: () => rt }
        : { wasmBinary: rt },
  );
  language = await Language.load(opts.grammar);
}
/** the loaded grammar (the formatter and the language server parse with it too) */
export function getLanguage(): Language {
  if (!language) throw new Error('call initParser() first');
  return language;
}

export type ParseResult = { decls: Decl[]; errors: { row: number; col: number }[] };

// the same text parses to the same result: the session and the language
// server re-load the unchanged modules of a universe on every question,
// and the AST is never mutated after lowering (a small bounded cache)
const parseCache = new Map<string, ParseResult>();
export function parseSource(src: string): ParseResult {
  const hit = parseCache.get(src);
  if (hit) return hit;
  const r = parseSourceUncached(src);
  if (parseCache.size >= 64) parseCache.delete(parseCache.keys().next().value!);
  parseCache.set(src, r);
  return r;
}
function parseSourceUncached(src: string): ParseResult {
  if (!language) throw new Error('call initParser() first');
  const parser = new Parser();
  parser.setLanguage(language);
  const tree = parser.parse(src)!;
  const errors: { row: number; col: number }[] = [];
  collectErrors(tree.rootNode, errors);
  const decls: Decl[] = [];
  // annotations precede the declaration they attach to as its siblings (§5.10)
  let pending: Annotation[] = [];
  for (const c of tree.rootNode.namedChildren) {
    if (!c || c.type === 'ERROR') continue;
    let d: Decl | null;
    try {
      if (c.type === 'annotation') {
        pending.push(lowerAnnotation(c));
        continue;
      }
      d = lowerDecl(c);
    } catch {
      if (!errors.length) errors.push({ row: c.startPosition.row, col: c.startPosition.column });
      pending = [];
      continue;
    }
    if (d) {
      if (c.previousSibling?.text === 'export' || d.d === 're_export') d.exported = true;
      if (pending.length) d.annotations = pending;
      pending = [];
      // the declaration's source range (Phase 6 foundations): the `export`
      // keyword, when present, is the previous sibling and is included
      const start =
        c.previousSibling?.text === 'export' ? c.previousSibling.startPosition : c.startPosition;
      d.loc = { sl: start.row, sc: start.column, el: c.endPosition.row, ec: c.endPosition.column };
      decls.push(d);
    }
  }
  return { decls, errors };
}

function collectErrors(n: Node, out: { row: number; col: number }[]) {
  if (n.type === 'ERROR' || n.isMissing)
    out.push({ row: n.startPosition.row, col: n.startPosition.column });
  if (n.hasError) for (const c of n.children) if (c) collectErrors(c, out);
}

const field = (n: Node, name: string): Node | null => n.childForFieldName(name);
// `true` / `false` / `null` are anonymous keyword tokens in the grammar:
// an operand position may hold one, so operands are the named children
// plus those literals (never the operator or punctuation tokens)
// a keyword literal (true / false / null) is an anonymous node that is still an operand
const isLitKeyword = (c: Node | null): boolean =>
  !!c && !c.isNamed && ['true', 'false', 'null'].includes(c.text);
const operands = (n: Node): Node[] =>
  n.children.filter((c): c is Node => !!c && (c.isNamed || isLitKeyword(c)));
const req = (n: Node, name: string): Node => {
  const c = field(n, name);
  if (!c) throw new Error(`lower: ${n.type} missing field ${name}`);
  return c;
};
const kids = (n: Node, type: string): Node[] =>
  n.namedChildren.filter((c): c is Node => !!c && c.type === type);
const kid = (n: Node, type: string): Node | null =>
  n.namedChildren.find((c): c is Node => !!c && c.type === type) ?? null;

// ---------------- annotations ----------------
function lowerAnnotation(n: Node): Annotation {
  return {
    name: req(n, 'name').text,
    args: operands(n)
      .slice(1)
      .map((c) => lowerExpr(c)),
    loc: locOf(n),
  };
}

// ---------------- declarations ----------------
function lowerDecl(n: Node): Decl | null {
  switch (n.type) {
    case 'type_declaration': {
      const params = kid(n, 'type_parameters');
      return {
        d: 'type',
        name: req(n, 'name').text,
        params: params
          ? kids(params, 'type_parameter').map((p) => ({
              name: p.namedChildren[0].text,
              type: p.namedChildren[1] ? lowerType(p.namedChildren[1]) : undefined,
            }))
          : undefined,
        type: lowerType(req(n, 'type')),
        tail: maybeTail(n),
      };
    }
    case 'const_declaration':
      return {
        d: 'const',
        name: req(n, 'name').text,
        type: field(n, 'type') ? lowerType(req(n, 'type')) : undefined,
        expr: lowerExpr(req(n, 'value')),
      };
    case 'func_declaration':
      return {
        d: 'func',
        name: req(n, 'name').text,
        params: kids(n, 'parameter').map((p) => ({
          name: p.namedChildren[0].text,
          type: lowerType(p.namedChildren[1]),
        })),
        ret: field(n, 'return_type') ? lowerType(req(n, 'return_type')) : undefined,
        body: lowerExpr(req(n, 'body')),
      };
    case 'output_declaration':
      return {
        d: 'output',
        name: req(n, 'name').text,
        type: lowerType(req(n, 'type')),
        expr: lowerExpr(req(n, 'value')),
      };
    case 'input_declaration':
      return {
        d: 'input',
        name: req(n, 'name').text,
        type: lowerType(req(n, 'type')),
        fallback: field(n, 'fallback') ? lowerExpr(req(n, 'fallback')) : undefined,
      };
    case 'diagnostic_declaration': {
      const sev = kid(n, 'severity')!;
      const tmpl = kid(n, 'template_string')!;
      return {
        d: 'diagnostic',
        name: req(n, 'name').text,
        params: kids(n, 'parameter').map((p) => ({
          name: p.namedChildren[0].text,
          type: lowerType(p.namedChildren[1]),
        })),
        severity: sev.text,
        template: lowerTemplateParts(tmpl),
      };
    }
    case 'dimension_declaration': {
      const e = kid(n, 'dimension_expression');
      return { d: 'dimension', name: req(n, 'name').text, terms: e ? lowerDimExpr(e) : undefined };
    }
    case 'unit_declaration': {
      const dim = field(n, 'dimension');
      if (dim) return { d: 'unit', name: req(n, 'name').text, dim: dim.text };
      return {
        d: 'unit',
        name: req(n, 'name').text,
        factor: lowerExpr(field(n, 'factor')!),
        base: field(n, 'base')!.text,
      };
    }
    case 'import_declaration': {
      const from = JSON.parse(kid(n, 'string')!.text);
      const ni = kid(n, 'named_imports');
      if (ni) return { d: 'import', from, names: kids(ni, 'import_item').map(lowerImportItem) };
      return { d: 'import', from, ns: kid(n, 'identifier')!.text };
    }
    case 're_export_declaration':
      return {
        d: 're_export',
        from: JSON.parse(kid(n, 'string')!.text),
        names: kids(n, 'import_item').map(lowerImportItem),
      };
    default:
      return null;
  }
}

function lowerImportItem(it: Node): { name: string; as?: string } {
  const ids = it.namedChildren.filter(Boolean);
  return { name: ids[0].text, as: ids[1]?.text };
}

function maybeTail(n: Node): ElseTail | undefined {
  const t = kid(n, 'else_clause');
  return t ? lowerTail(t) : undefined;
}
function lowerTail(n: Node): ElseTail {
  const sev = kid(n, 'severity');
  if (sev)
    return {
      t: 'inline',
      severity: sev.text,
      template: lowerTemplateParts(kid(n, 'template_string')!),
    };
  const name = kid(n, 'qualified_name')!.text;
  const args = n.namedChildren
    .filter((c): c is Node => !!c && c.type !== 'qualified_name')
    .map((c) => lowerExpr(c));
  return { t: 'ref', name, args };
}
function lowerTemplateParts(n: Node): TemplateParts {
  const parts: TemplateParts = [];
  for (const c of n.namedChildren) {
    if (!c) continue;
    if (c.type === 'template_chars') parts.push(c.text);
    else if (c.type === 'template_escape') parts.push(unescape(c.text));
    else if (c.type === 'interpolation') parts.push(lowerExpr(operands(c)[0]));
  }
  return parts;
}
const unescape = (s: string) =>
  s.replace(/\\(.)/g, (_, c) => (c === 'n' ? '\n' : c === 't' ? '\t' : c === 'r' ? '\r' : c));

// ---------------- types ----------------
const locOf = (n: Node): Loc => ({
  sl: n.startPosition.row,
  sc: n.startPosition.column,
  el: n.endPosition.row,
  ec: n.endPosition.column,
});
function lowerType(n: Node): TypeAst {
  const t = lowerType0(n);
  t.loc = locOf(n);
  return t;
}
function lowerType0(n: Node): TypeAst {
  switch (n.type) {
    case 'union_type':
      return { k: 'union', arms: n.namedChildren.filter(Boolean).map((c) => lowerType(c)) };
    case 'intersection_type':
      return { k: 'isect', arms: n.namedChildren.filter(Boolean).map((c) => lowerType(c)) };
    case 'nullable_type':
      return { k: 'union', arms: [lowerType(n.namedChildren[0]), { k: 'prim', name: 'null' }] };
    case 'array_type': {
      const elem = lowerType(n.namedChildren[0]);
      const range =
        kid(n, 'array_size_range') ??
        (() => {
          const sz = field(n, 'size');
          return sz && sz.type === 'range_expression' ? sz : null;
        })();
      if (range) {
        // endpoints stay names when they reference module consts (§4.13);
        // resolution substitutes their evaluated values
        const [lo, hi] = range.namedChildren
          .filter(Boolean)
          .map((c) => constNum(c))
          .map((v) => (typeof v === 'string' ? v : Number(v)));
        const excl = range.children.some((c) => c && !c.isNamed && c.text === '..<');
        if (typeof hi === 'number') return { k: 'array', elem, lo, hi: excl ? hi - 1 : hi };
        return { k: 'array', elem, lo, hi, excl };
      }
      const size = field(n, 'size');
      if (size) {
        const v0 = constNum(size);
        const v = typeof v0 === 'string' ? v0 : Number(v0);
        return { k: 'array', elem, lo: v, hi: v };
      }
      return { k: 'array', elem };
    }
    case 'range_type': {
      const [a, b] = n.namedChildren.filter(Boolean);
      return { k: 'range', lo: constNum(a), hi: constNum(b), excl: n.text.includes('..<') };
    }
    case 'number_literal':
      return { k: 'lit', v: constNum(n) };
    case 'string':
      return { k: 'lit', v: JSON.parse(n.text.replace(/\n/g, '\\n')) };
    case 'pattern':
      return { k: 'pattern', re: n.text.slice(1, -1) };
    case 'paren_type':
      return lowerType(n.namedChildren[0]);
    case 'record_type': {
      let open = false;
      const members: MemberAst[] = [];
      let pending: Annotation[] = []; // a member's annotations precede it as siblings (§5.10)
      for (const c of n.namedChildren) {
        if (!c) continue;
        if (c.type === 'open_marker') {
          open = true;
          continue;
        }
        if (c.type === 'annotation') {
          pending.push(lowerAnnotation(c));
          continue;
        }
        const m = lowerMember(c);
        if (m) {
          if (pending.length) m.annotations = pending;
          pending = [];
          members.push(m);
        }
      }
      return { k: 'record', members, open };
    }
    case 'map_type':
      return { k: 'map', key: lowerType(req(n, 'key')), val: lowerType(req(n, 'value')) };
    case 'function_type': {
      const cs = n.namedChildren.filter(Boolean).map((c) => lowerType(c));
      return { k: 'func', params: cs.slice(0, -1), ret: cs[cs.length - 1] };
    }
    case 'named_type': {
      const name = kid(n, 'qualified_name')!.text;
      const argsN = kid(n, 'type_arguments');
      const args = argsN ? argsN.namedChildren.filter(Boolean).map((c) => lowerType(c)) : [];
      const predsN = field(n, 'predicates');
      const preds = predsN
        ? predsN.namedChildren.filter(Boolean).map((c) => lowerExpr(c))
        : undefined;
      const extN = field(n, 'extension');
      const ext = extN ? lowerType(extN) : undefined;
      const prim = ['int', 'uint', 'float', 'bool', 'string'];
      if (prim.includes(name) && args.length === 0 && !preds && !ext) return { k: 'prim', name };
      return { k: 'named', name, args, preds, ext };
    }
    default:
      if (n.text === 'true') return { k: 'lit', v: true };
      if (n.text === 'false') return { k: 'lit', v: false };
      if (n.text === 'null') return { k: 'prim', name: 'null' };
      throw new Error(`lowerType: unhandled ${n.type} '${n.text.slice(0, 30)}'`);
  }
}
// dimension expressions are abelian-group products: fold each term's
// exponent, negating after `/` (§3.16)
function lowerDimExpr(n: Node): { name: string; exp: number }[] {
  const out: { name: string; exp: number }[] = [];
  let sign = 1;
  for (const c of n.children) {
    if (!c) continue;
    if (!c.isNamed) {
      if (c.text === '/') sign = -1;
      else if (c.text === '*') sign = 1;
      continue;
    }
    if (c.type === 'dimension_term') {
      const id = c.namedChildren.find((x) => x && x.type === 'identifier')!;
      const num = c.namedChildren.find((x) => x && x.type === 'int');
      let exp = num ? Number(num.text) : 1;
      if (c.children.some((x) => x && !x.isNamed && x.text === '-')) exp = -exp;
      out.push({ name: id.text, exp: exp * sign });
      sign = 1;
    }
  }
  return out;
}

function constNum(n: Node): any {
  if (n.type === 'number_literal') {
    const neg = n.text.trimStart().startsWith('-');
    const inner = n.namedChildren[0];
    const v = constNum(inner);
    return neg ? -v : v;
  }
  if (n.type === 'int') return parseInt_(n.text);
  if (n.type === 'float') return parseFloat(n.text.replace(/_/g, ''));
  if (n.type === 'qualified_name' || n.type === 'identifier') return n.text; // const/param reference
  throw new Error(`constNum: ${n.type} '${n.text.slice(0, 20)}'`);
}
function parseInt_(text: string): bigint {
  const t = text.replace(/_/g, '');
  return BigInt(t);
}

// ---------------- members ----------------
function lowerMember(n: Node): MemberAst | null {
  const m = lowerMember0(n);
  if (m) m.loc = locOf(n);
  return m;
}
function lowerMember0(n: Node): MemberAst | null {
  switch (n.type) {
    // member kinds by syntax (D4, v0.3): `?` — input may supply it; `= e` —
    // the schema computes it. Both: defaulted; `= e` alone: derived
    case 'value_member': {
      const nameN = req(n, 'name');
      const name = nameN.type === 'string' ? JSON.parse(nameN.text) : nameN.text;
      const opt = !!field(n, 'optional');
      const dflt = field(n, 'default') ? lowerExpr(req(n, 'default')) : undefined;
      if (dflt && !opt) return { m: 'derived', name, type: lowerType(req(n, 'type')), expr: dflt };
      return { m: 'value', name, opt, type: lowerType(req(n, 'type')), dflt };
    }
    case 'derived_member': {
      const nameN = req(n, 'name');
      return {
        m: 'derived',
        name: nameN.type === 'string' ? JSON.parse(nameN.text) : nameN.text,
        expr: lowerExpr(req(n, 'value')),
      };
    }
    // `x$ [: T] = e` — computed for the schema's own use, never part of the value (D34)
    case 'hidden_member': {
      return {
        m: 'derived',
        name: req(n, 'name').text,
        type: field(n, 'type') ? lowerType(req(n, 'type')) : undefined,
        expr: lowerExpr(req(n, 'value')),
        hidden: true,
      };
    }
    case 'context_declaration':
      return { m: 'context', variable: req(n, 'variable').text, type: lowerType(req(n, 'type')) };
    case 'assert_member':
      return {
        m: 'assert',
        name: req(n, 'name').text,
        cond: lowerExpr(req(n, 'condition')),
        tail: maybeTail(n),
      };
    case 'when_member': {
      const body: MemberAst[] = [];
      for (const c of n.namedChildren.slice(1)) {
        if (!c) continue;
        const m = lowerMember(c);
        if (m) body.push(m);
      }
      return { m: 'when', cond: lowerExpr(req(n, 'condition')), body };
    }
    default:
      return null;
  }
}

// ---------------- expressions ----------------
const BIN_NODES = new Set([
  'pipe_expression',
  'nullish_expression',
  'binary_expression_or',
  'binary_expression_and',
  'bit_or_expression',
  'bit_xor_expression',
  'bit_and_expression',
  'equality_expression',
  'relational_expression',
  'range_expression',
  'shift_expression',
  'additive_expression',
  'multiplicative_expression',
]);

function lowerExpr(n: Node): Expr {
  const e = lowerExpr0(n);
  e.loc = locOf(n);
  return e;
}
function lowerExpr0(n: Node): Expr {
  switch (n.type) {
    case 'int':
      return { e: 'lit', v: parseInt_(n.text) };
    case 'float':
      return { e: 'lit', v: parseFloat(n.text.replace(/_/g, '')) };
    case 'unit_literal': {
      const m = /^([0-9._]+(?:[eE][+-]?[0-9]+)?)([A-Za-z][A-Za-z0-9]*)$/.exec(n.text)!;
      return { e: 'unitlit', num: parseFloat(m[1].replace(/_/g, '')), unit: m[2] };
    }
    case 'string':
      return { e: 'lit', v: JSON.parse(n.text.replace(/\n/g, '\\n')) };
    case 'template_string':
      return { e: 'template', parts: lowerTemplateParts(n) };
    case 'identifier':
    case 'hidden_name':
      return { e: 'name', name: n.text };
    case 'context_variable':
      return { e: 'ctx', name: n.text };
    case 'referrers_expression':
      return {
        e: 'referrers',
        type: req(n, 'type').text,
        member: JSON.parse(req(n, 'member').text),
      };
    case 'paren_expression':
      return { e: 'paren', x: lowerExpr(operands(n)[0]) };
    case 'unary_expression':
      return { e: 'un', op: n.children[0].text, x: lowerExpr(operands(n)[0]) };
    case 'if_expression':
      return {
        e: 'if',
        c: lowerExpr(req(n, 'condition')),
        t: lowerExpr(req(n, 'then')),
        f: lowerExpr(req(n, 'else')),
      };
    case 'lambda':
      return {
        e: 'lambda',
        params: kids(n, 'lambda_parameter').map((p) => p.namedChildren[0].text),
        body: lowerExpr(req(n, 'body')),
      };
    case 'with_expression': {
      const [base, patch] = operands(n);
      return { e: 'with', base: lowerExpr(base), patch: lowerExpr(patch) };
    }
    case 'member_access':
    case 'safe_access': {
      const [x, name] = operands(n);
      return {
        e: 'member',
        x: lowerExpr(x),
        name: name.type === 'string' ? JSON.parse(name.text) : name.text,
        safe: n.type === 'safe_access' || undefined,
      };
    }
    case 'index_access': {
      const [x, i] = operands(n);
      return { e: 'index', x: lowerExpr(x), i: lowerExpr(i) };
    }
    case 'call': {
      // bare true/false/null are anonymous keyword tokens — include them
      const cs = n.children.filter(
        (c): c is Node => !!c && (c.isNamed || ['true', 'false', 'null'].includes(c.text)),
      );
      return { e: 'call', fn: lowerExpr(cs[0]), args: cs.slice(1).map((c) => lowerExpr(c)) };
    }
    case 'object': {
      const comp = kid(n, 'map_comprehension');
      if (comp) return lowerExpr(comp);
      return {
        e: 'obj',
        entries: kids(n, 'object_entry').map((en) => {
          const key = field(en, 'key');
          if (key)
            return {
              key: key.type === 'string' ? JSON.parse(key.text) : key.text,
              val: lowerExpr(req(en, 'value')),
            };
          return { key: '...', val: lowerExpr(en.namedChildren[0]) }; // spread entry
        }),
      };
    }
    case 'map_comprehension':
      return {
        e: 'mapcomp',
        key: lowerExpr(req(n, 'key')),
        val: lowerExpr(req(n, 'value')),
        clauses: kids(n, 'for_clause').map(lowerFor),
      };
    case 'array': {
      const comp = kid(n, 'array_comprehension');
      if (comp) return lowerExpr(comp);
      return {
        e: 'arr',
        items: kids(n, 'array_entry').map((en) => {
          const spread = en.text.startsWith('...');
          const inner =
            en.namedChildren.find(Boolean) ??
            en.children.find((c) => c && ['true', 'false', 'null'].includes(c.text));
          return { spread, expr: lowerExpr(inner!) };
        }),
      };
    }
    case 'array_comprehension':
      return {
        e: 'comp',
        head: lowerExpr(req(n, 'head')),
        clauses: kids(n, 'for_clause').map(lowerFor),
      };
    case 'matches_expression': {
      const [l, r] = n.namedChildren.filter(Boolean);
      return { e: 'bin', op: 'matches', l: lowerExpr(l), r: lowerExpr(r) };
    }
    case 'pattern':
      return { e: 'pattern', re: n.text.slice(1, -1) };
    case 'match_expression': {
      const arms = kids(n, 'match_arm').map((a) => {
        const body = a.childForFieldName('body')!;
        const others = a.namedChildren.filter((c): c is Node => !!c && c.id !== body.id);
        return {
          v: others[0].text,
          type: others[1] ? lowerType(others[1]) : undefined,
          body: lowerExpr(body),
        };
      });
      return { e: 'match', subject: lowerExpr(req(n, 'subject')), arms };
    }
    default:
      if (BIN_NODES.has(n.type)) {
        const [l, r] = operands(n);
        // the operator is the one anonymous child that is not an operand
        const op = n.children.find(
          (c) => c && !c.isNamed && !isLitKeyword(c) && c.text.trim() !== '',
        )!.text;
        return { e: 'bin', op, l: lowerExpr(l), r: lowerExpr(r) };
      }
      if (n.text === 'true') return { e: 'lit', v: true };
      if (n.text === 'false') return { e: 'lit', v: false };
      if (n.text === 'null') return { e: 'lit', v: null };
      throw new Error(`lowerExpr: unhandled ${n.type} '${n.text.slice(0, 40)}'`);
  }
}
function lowerFor(n: Node) {
  return {
    v: req(n, 'variable').text,
    iter: lowerExpr(req(n, 'iterable')),
    filters: n
      .childrenForFieldName('filter')
      .filter(Boolean)
      .map((c) => lowerExpr(c)),
  };
}
