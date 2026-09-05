// The TextMate grammar (site/grammars/decl.tmLanguage.json, copied into
// the VS Code extension) must not drift from the tree-sitter grammar:
// every keyword the grammar reserves appears in the TextMate keyword
// pattern, and nothing else does. Run by the extension's build.
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const grammar = readFileSync(join(root, 'tree-sitter-decl/grammar.js'), 'utf8');
const tm = JSON.parse(readFileSync(join(root, 'site/grammars/decl.tmLanguage.json'), 'utf8'));

// the reserved words: the quoted lower-case tokens of grammar.js that are
// not field names (field('name', …)) or the literal keywords
// (a quoted word inside field('…') or alias('…') names a field or a rule, not a token)
const tokens = grammar.replace(/\b(?:field|alias)\('[a-z_]+'/g, '');
const words = new Set([...tokens.matchAll(/'([a-z][a-z_]*)'/g)].map(m => m[1]).filter(w => !['decl'].includes(w)));
const literal = new Set(['true', 'false', 'null']);
const keywords = new Set([...words].filter(w => !literal.has(w)));

// the TextMate side: every `match` whose scope is a keyword scope, its alternatives
const tmWords = new Set();
const visit = (node) => {
  if (!node || typeof node !== 'object') return;
  if (Array.isArray(node)) { node.forEach(visit); return; }
  // a keyword-scoped rule: every group's alternatives; a rule whose
  // captures scope some groups as keywords: those groups' alternatives
  for (const key of ['match', 'begin']) {
    if (typeof node[key] !== 'string') continue;
    const groups = [...node[key].matchAll(/\((\?:)?([a-z_]+(?:\|[a-z_]+)*)\)/g)];
    const captures = node[key === 'match' ? 'captures' : 'beginCaptures'] ?? {};
    let index = 0;
    for (const g of groups) {
      if (!g[1]) index++;                                   // a capturing group
      const own = /keyword/.test(node.name ?? '') || (!g[1] && /keyword/.test(captures[String(index)]?.name ?? ''));
      if (own) for (const w of g[2].split('|')) tmWords.add(w);
    }
  }
  for (const v of Object.values(node)) visit(v);
};
visit(tm);
const missing = [...keywords].filter(w => !tmWords.has(w)).sort();
const extra = [...tmWords].filter(w => !keywords.has(w) && !literal.has(w)).sort();
export function checkGrammar() {
  if (missing.length || extra.length) {
    const why = [missing.length ? `lacks the keywords: ${missing.join(', ')}` : '', extra.length ? `has keywords the language does not: ${extra.join(', ')}` : ''].filter(Boolean).join('; ');
    throw new Error(`TextMate grammar ${why}`);
  }
  return keywords.size;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try { console.log(`grammar check: ${checkGrammar()} keywords agree`); }
  catch (e) { console.error(String(e.message)); process.exit(1); }
}
