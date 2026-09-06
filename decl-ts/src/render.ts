// The renderer (docs/tooling/05_render.md): the form a module declares
// for an output with `@render` — a format and a layout, a template, a
// destination, a fan-out — read from the annotation (§3), the structured
// text of a document in that form (§4), and the templates (§5) and the
// fan-out (§6) that turn one evaluated root into text or files. The
// command line, the REPL, the library, and the editor preview all emit
// through here, so that the three implementations print the same bytes.
import type { Decl, Expr } from './ast.ts';
import { toJson, toYaml } from './yaml.ts';

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
