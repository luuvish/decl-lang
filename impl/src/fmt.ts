// Canonical formatter (ROADMAP Phase 4; §2.1/D1): LF, 4-space
// indentation, no tabs, normalized intra-line spacing. The original
// line structure is preserved — §2.9 makes newlines separators, so
// where a construct breaks lines is the author's statement — and the
// formatter re-derives indentation and token spacing deterministically,
// which makes it idempotent by construction.
import { Parser, Language, Node } from 'web-tree-sitter';
import { WASM } from './parse.ts';

let language: Language | null = null;
async function lang(): Promise<Language> {
  if (!language) { await Parser.init(); language = await Language.load(WASM); }
  return language;
}
export async function initFormatter(): Promise<void> { await lang(); }

type Leaf = { text: string; type: string; parent: string; row: number; endRow: number; col: number };

// atoms: leaves kept verbatim, including their internal whitespace
const ATOMS = new Set(['string', 'template_string', 'pattern', 'unit_literal',
  'doc_comment', 'line_comment', 'block_comment']);

function collect(n: Node, out: Leaf[]) {
  if (ATOMS.has(n.type) || n.childCount === 0) {
    if (n.text.length === 0) return;   // zero-width externals (NEWLINE)
    out.push({ text: n.text, type: n.type, parent: n.parent?.type ?? '',
      row: n.startPosition.row, endRow: n.endPosition.row, col: n.startPosition.column });
    return;
  }
  for (const c of n.children) if (c) collect(c, out);
}

const KEYWORDY = /^[A-Za-z_$][A-Za-z0-9_]*$/;
const BIN_OPS = new Set(['=', '==', '!=', '<=', '>=', '+', '*', '/', '%', '&&', '||', '??',
  '|>', '=>', '<<', '>>', 'in', 'matches', 'with', 'then', 'else', 'for', 'if', 'as', 'from']);
const CONT_STARTERS = new Set(['else', '=', 'for', 'if', '&&', '||', '|>', '??', '.', '?.',
  '+', '-', '*', '/', '==', '!=', '<=', '>=', '<', '>', '=>', 'then']);

function isTypeAngle(l: Leaf): boolean {
  return (l.text === '<' || l.text === '>')
    && (l.parent === 'type_arguments' || l.parent === 'type_parameters');
}

// spacing decision: does a space go between a and b on one line?
function spaced(a: Leaf, b: Leaf, prev: Leaf | null): boolean {
  const at = a.text, bt = b.text;
  // comments keep at least one space before them (handled by caller)
  if (b.type.endsWith('comment')) return true;
  if (isTypeAngle(a)) { if (at === '<') return false; /* '>' */ }
  if (isTypeAngle(b)) return false;   // Vec<...>, no space before either angle
  if (at === '(' || at === '[') return false;
  if (bt === ')' || bt === ']' || bt === ',' || bt === ':') return false;
  if (bt === '?' || at === '?') return false;                    // int?, name?:
  if (at === '.' || bt === '.' || at === '?.' || bt === '?.') return false;
  if (bt === ';') return false;
  if (at === '..' || at === '..<' || bt === '..' || bt === '..<') return false;
  if (bt === '(') {
    // call/parameter parens attach to a name or closing bracket; grouping parens do not
    return !(KEYWORDY.test(at) && !isKeyword(at)) && at !== ')' && at !== ']' && !isTypeAngle(a);
  }
  if (bt === '[') {
    // index/size brackets attach; array literals stand off
    return !(KEYWORDY.test(at) || at === ')' || at === ']' || isTypeAngle(a));
  }
  if (at === '{' || bt === '}') return true;                     // { a: 1 }
  if (bt === '{' || at === '}') return true;
  if (at === '!' || at === '~') return false;                    // unary
  if (at === '-' || at === '+') {
    // unary sign: previous token is an operator, opener, or keyword
    const p = prev?.text;
    const unary = !p || BIN_OPS.has(p) || ['(', '[', '{', ',', ':', '<', '..', '..<', '-', '+', '!', '~'].includes(p!)
      || (KEYWORDY.test(p!) && isKeyword(p!));
    if (unary) return false;
  }
  return true;
}
const KEYWORDS = new Set(['type', 'const', 'func', 'output', 'input', 'export', 'import',
  'diagnostic', 'dimension', 'unit', 'assert', 'when', 'if', 'then', 'else', 'match', 'for',
  'in', 'with', 'as', 'from', 'true', 'false', 'null', 'error', 'warn', 'info', 'matches']);
const isKeyword = (t: string) => KEYWORDS.has(t);

export function format(src: string): string {
  if (!language) throw new Error('call initFormatter() first');
  const parser = new Parser();
  parser.setLanguage(language);
  const tree = parser.parse(src)!;
  if (tree.rootNode.hasError) throw new Error('cannot format: file has parse errors');
  const leaves: Leaf[] = [];
  collect(tree.rootNode, leaves);

  // group leaves by their original starting row
  const lines: Leaf[][] = [];
  const rowOf = new Map<number, Leaf[]>();
  for (const l of leaves) {
    let bucket = rowOf.get(l.row);
    if (!bucket) { bucket = []; rowOf.set(l.row, bucket); lines.push(bucket); }
    bucket.push(l);
  }

  const out: string[] = [];
  let depth = 0;
  let lastRowEnd = -1;      // last original row consumed (multiline atoms span rows)
  for (const line of lines) {
    const first = line[0];
    if (first.row <= lastRowEnd) continue;          // inside a multiline atom
    // one blank line max between constructs
    if (out.length > 0 && first.row > lastRowEnd + 1) out.push('');
    // indentation: bracket depth, closers on the line start dedent first
    let closers = 0;
    for (const l of line) { if ([')', ']', '}'].includes(l.text)) closers++; else break; }
    let indent = Math.max(0, depth - closers);
    // a line starting with a continuation token hangs one level deeper
    if (closers === 0 && CONT_STARTERS.has(first.text)) indent = depth + 1;
    let text = '    '.repeat(indent);
    let prev: Leaf | null = null, prev2: Leaf | null = null;
    for (const l of line) {
      if (prev) {
        if (l.type.endsWith('comment')) {
          // inline comment: keep the author's alignment (min one space)
          text += ' '.repeat(Math.max(1, l.col - (prev.col + prev.text.length)));
        } else if (spaced(prev, l, prev2)) text += ' ';
      }
      text += l.text;
      if (!ATOMS.has(l.type)) {
        for (const ch of l.text) {
          if (ch === '{' || ch === '[' || ch === '(') depth++;
          else if (ch === '}' || ch === ']' || ch === ')') depth = Math.max(0, depth - 1);
        }
      }
      prev2 = prev; prev = l;
      lastRowEnd = Math.max(lastRowEnd, l.endRow);
    }
    out.push(text.replace(/[ \t]+$/, ''));
  }
  return out.join('\n') + '\n';
}
