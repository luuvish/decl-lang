// A CodeMirror stream mode for Decl — lexical classes only (the site's
// static highlighting uses the TextMate grammar in grammars/; this one
// runs in the playground editor).
import { StreamLanguage, HighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { tags as t } from '@lezer/highlight';
import type { StringStream } from '@codemirror/language';

const keywords = new Set([
  'import', 'export', 'from', 'as', 'type', 'const', 'func', 'output', 'input', 'diagnostic', 'dimension', 'unit',
  'assert', 'when', 'else', 'if', 'then', 'for', 'in', 'match', 'with', 'matches', 'error', 'warn', 'info',
]);
const types = new Set(['int', 'float', 'bool', 'string', 'quantity', 'ref', 'map', 'any']);
const constants = new Set(['true', 'false', 'null']);

type State = { block: number; prev: string };

const patternAllowedAfter = new Set([':', '=', '(', ',', '|', 'matches', 'else', '']);

export const declLanguage = StreamLanguage.define<State>({
  startState: () => ({ block: 0, prev: '' }),
  token(stream: StringStream, st: State): string | null {
    if (st.block > 0) {
      while (!stream.eol()) {
        if (stream.match('/*')) st.block++;
        else if (stream.match('*/')) { st.block--; if (st.block === 0) break; }
        else stream.next();
      }
      return 'comment';
    }
    if (stream.sol()) st.prev = '';
    if (stream.eatSpace()) return null;
    if (stream.match('/*')) { st.block = 1; return 'comment'; }
    if (stream.match(/^\/\/.*/)) return 'comment';
    let m: RegExpMatchArray | null;
    if (stream.match(/^\$(this|parent|path|root|key|referrers)\b/)) { st.prev = 'ctx'; return 'variableName.special'; }
    if (stream.match(/^"(?:[^"\\]|\\.)*"?/)) { st.prev = 'str'; return 'string'; }
    if (stream.match(/^`(?:[^`\\]|\\.)*`?/)) { st.prev = 'str'; return 'string.special'; }
    if (patternAllowedAfter.has(st.prev) && stream.match(/^\/(?:[^/\\\n]|\\.)+\//)) { st.prev = 'pat'; return 'regexp'; }
    if (stream.match(/^0[xX][0-9a-fA-F][0-9a-fA-F_]*\b/) || stream.match(/^0[oO][0-7][0-7_]*\b/) || stream.match(/^0[bB][01][01_]*\b/)) { st.prev = 'num'; return 'number'; }
    if (stream.match(/^(?:0|[1-9][0-9_]*)(?:\.[0-9][0-9_]*)?(?:[eE][+-]?[0-9]+)?(?:[A-DF-Za-df-z][A-Za-z0-9]*|[eE][A-Za-z][A-Za-z0-9]*|[eE])\b/)) { st.prev = 'num'; return 'unit'; }
    if (stream.match(/^(?:0|[1-9][0-9_]*)(?:\.[0-9][0-9_]*(?:[eE][+-]?[0-9]+)?|[eE][+-]?[0-9]+)\b/) || stream.match(/^(?:0|[1-9][0-9_]*)\b/)) { st.prev = 'num'; return 'number'; }
    if ((m = stream.match(/^[A-Za-z_][A-Za-z0-9_]*/) as RegExpMatchArray | null)) {
      const w = m[0];
      st.prev = w;
      if (keywords.has(w)) return 'keyword';
      if (constants.has(w)) return 'atom';
      if (types.has(w)) return 'typeName';
      if (w === 'std') return 'namespace';
      if (/^[A-Z]/.test(w)) return 'typeName';
      if (stream.match(/^\s*\??\s*:(?!:)/, false)) return 'propertyName';
      if (stream.match(/^\s*\(/, false)) return 'variableName.function';
      return 'variableName';
    }
    if ((m = stream.match(/^(\|>|\?\?|\?\.|=>|\.\.<|\.\.|&&|\|\||==|!=|<=|>=|<<|>>|[-+*/%<>=!&|^~])/) as RegExpMatchArray | null)) {
      st.prev = m[0];
      return 'operator';
    }
    const ch = stream.next() ?? '';
    st.prev = ch;
    return null;
  },
  languageData: { commentTokens: { line: '//', block: { open: '/*', close: '*/' } } },
});

/** the six syntax roles of the identity (brand/palette.mjs, as --decl-syn-* in tokens.css), so the editor colours a token exactly as a ```decl block does */
export const declHighlight = syntaxHighlighting(HighlightStyle.define([
  { tag: t.comment, color: 'var(--decl-syn-c)', fontStyle: 'italic' },
  { tag: t.keyword, color: 'var(--decl-syn-k)', fontWeight: '600' },
  { tag: t.atom, color: 'var(--decl-syn-n)' },
  { tag: t.number, color: 'var(--decl-syn-n)' },
  { tag: t.unit, color: 'var(--decl-syn-n)' },
  { tag: t.string, color: 'var(--decl-syn-s)' },
  { tag: t.special(t.string), color: 'var(--decl-syn-s)' },
  { tag: t.regexp, color: 'var(--decl-syn-s)' },
  { tag: t.typeName, color: 'var(--decl-syn-t)' },
  { tag: t.namespace, color: 'var(--decl-syn-t)' },
  { tag: t.propertyName, color: 'var(--sl-color-text)' },
  { tag: t.special(t.variableName), color: 'var(--decl-syn-t)', fontStyle: 'italic' },
  { tag: t.function(t.variableName), color: 'var(--sl-color-text)' },
  { tag: t.operator, color: 'var(--decl-syn-o)' },
]));
