// The code-block themes, one per site theme, from the six syntax roles in
// brand/palette.mjs. Expressive Code (the ```decl blocks and every other
// fenced block on the site) takes them as VS Code themes; the playground's
// CodeMirror mode reads the same roles as --decl-syn-* custom properties.
import { palette } from './palette.mjs';

/** a VS Code theme for one mode: keywords in bold ink, types blue, strings warm, numbers green, comments quiet */
export function codeTheme(mode) {
  const p = palette[mode];
  const rule = (scope, foreground, fontStyle) => ({ scope, settings: fontStyle ? { foreground, fontStyle } : { foreground } });
  return {
    name: `decl-${mode}`,
    type: mode,
    colors: {
      'editor.background': p.surface,
      'editor.foreground': p.text,
      'editorLineNumber.foreground': p.muted,
      'editor.selectionBackground': p.tint,
      'editorGutter.background': p.surface,
    },
    tokenColors: [
      rule(['comment', 'punctuation.definition.comment'], p.syn.c, 'italic'),
      rule(['keyword', 'storage', 'storage.type', 'keyword.control', 'keyword.declaration', 'keyword.other.severity'], p.syn.k, 'bold'),
      rule(['keyword.operator', 'punctuation', 'meta.brace', 'punctuation.separator', 'punctuation.bracket'], p.syn.o),
      rule(['entity.name.type', 'support.type', 'support.class', 'support.function', 'entity.name.type.unit', 'entity.name.namespace'], p.syn.t),
      rule(['variable.language', 'variable.language.context'], p.syn.t, 'italic'),
      rule(['string', 'string.regexp', 'string.template', 'constant.character.escape', 'punctuation.definition.string'], p.syn.s),
      rule(['meta.interpolation', 'punctuation.section.interpolation'], p.text),
      rule(['constant.numeric', 'constant.language', 'keyword.other.unit', 'constant.numeric.quantity'], p.syn.n),
      rule(['variable', 'variable.other.property', 'variable.other.constant', 'support.type.property-name', 'entity.name.function', 'entity.name.tag', 'entity.other.attribute-name', 'meta.object-literal.key'], p.text),
      rule(['markup.heading'], p.ink, 'bold'),
      rule(['markup.italic'], p.text, 'italic'),
      rule(['markup.bold'], p.text, 'bold'),
    ],
  };
}
