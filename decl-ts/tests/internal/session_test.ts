// session (tests/internal/checks.json): the operation log — apply, undo,
// redo, and a new operation after undo discarding the redo tail.
import { join } from 'node:path';
import { readFileSync } from 'node:fs';
import { initParser } from '../../src/node.ts';
import { Session, SessionError } from '../../src/session.ts';
import { pathStr } from '../../src/semantics.ts';
import { check, total, root } from '../common/check.ts';

await initParser();
const s = new Session(join(root, 'tests/repl/documents/main.decl'));
const bind = (text: string) => s.apply({ op: 'bind', name: 'extra', src: { kind: 'inline', text } });
const invalid = () => {
  try {
    s.documentText('extra');
    return false;
  } catch (e) {
    return e instanceof SessionError;
  }
};
bind('{ "port": 1, "name": "x" }');
const bound = s.documentText('extra');
const undone = s.undo();
const gone = invalid();
const redone = s.redo();
const back = s.documentText('extra');
s.undo();
bind('{ "port": 2, "name": "y" }');
const nothing = s.redo();
const latest = s.documentText('extra');
check(
  'undo_redo',
  bound === '{"port":1,"name":"x"}' &&
    undone === 1 &&
    gone &&
    redone === 1 &&
    back === bound &&
    nothing === 0 &&
    latest === '{"port":2,"name":"y"}',
  JSON.stringify({ bound, undone, gone, redone, back, nothing, latest }),
);
let editCases = 0;
for (const c of JSON.parse(readFileSync(join(root, 'tests/internal/edits.json'), 'utf8'))) {
  editCases++;
  const entry = join(root, 'tests/internal/session_case.decl');
  const overlay = new Map([[entry, c.source]]);
  const retained = new Session(entry, overlay), fresh = new Session(entry, overlay);
  const apply = (s: Session, op: any, full: boolean) => {
    Session.fullRecompute = full;
    try {
      if (op.op === 'undo') s.undo();
      else if (op.op === 'redo') s.redo();
      else s.apply(op);
    } finally { Session.fullRecompute = false; }
  };
  const snapshot = (s: Session, full: boolean) => {
    Session.fullRecompute = full;
    try {
      const run = s.run();
      const values = [...(run.entry?.env.roots ?? [])].map(([name, value]) => [name, run.eng?.serialize(value, name)]);
      const diagnostics = run.diags.map((d) => { const out = { ...d }; delete out.by; return out; });
      return { run, text: JSON.stringify({ values, diagnostics, load: run.loadDiags, checks: run.checks, sessionChecks: run.sessionChecks }) };
    } finally { Session.fullRecompute = false; }
  };
  for (const op of c.initial) { apply(retained, op, false); apply(fresh, op, true); }
  let prior = snapshot(retained, false);
  check(`${c.name}: initial`, prior.text === snapshot(fresh, true).text && !!prior.run.eng, prior.text);
  for (const [i, step] of c.steps.entries()) {
    const programs = prior.run.eng?.programs;
    const inst = prior.run.entry?.env.registry.find((r) => pathStr(r.path) === step.retained);
    const edits = prior.run.eng?.edits;
    const verified = edits?.revisions.verifiedCutoffs ?? 0, equal = edits?.revisions.valueCutoffs ?? 0;
    apply(retained, step.action, false); apply(fresh, step.action, true);
    const next = snapshot(retained, false), expected = snapshot(fresh, true);
    check(`${c.name}: step ${i} parity`, next.text === expected.text, `expected ${expected.text}\ngot ${next.text}`);
    check(`${c.name}: step ${i} programs`, !!programs && programs === next.run.eng?.programs);
    if (step.retained) check(`${c.name}: retained record`, !!inst && !!next.run.entry?.env.registry.includes(inst));
    if (step.max_prepared !== undefined) check(`${c.name}: bounded verification`, (edits?.preparedQueries ?? Infinity) <= step.max_prepared, String(edits?.preparedQueries));
    if (step.max_computes !== undefined) check(`${c.name}: work`, next.run.timing.recomputed !== undefined && next.run.timing.recomputed <= step.max_computes, JSON.stringify(next.run.timing));
    if (step.min_verified !== undefined) check(`${c.name}: verification cutoff`, (edits?.revisions.verifiedCutoffs ?? 0) - verified >= step.min_verified, String((edits?.revisions.verifiedCutoffs ?? 0) - verified));
    if (step.min_value_cutoffs !== undefined) check(`${c.name}: value cutoff`, (edits?.revisions.valueCutoffs ?? 0) - equal >= step.min_value_cutoffs, String((edits?.revisions.valueCutoffs ?? 0) - equal));
    prior = next;
  }
}
check('retained_edits', editCases > 0);
const temporary = JSON.parse(readFileSync(join(root, 'tests/internal/temporary.json'), 'utf8'));
const scratchEntry = join(root, 'tests/internal/temporary_case.decl');
const scratch = new Session(scratchEntry, new Map([[scratchEntry, temporary.source]]));
const scratchEngine = scratch.run().eng!;
const sourcePrograms = scratchEngine.programs!;
const initialCounts = [sourcePrograms.compiled, sourcePrograms.compiledSchemas, scratchEngine.env.registry.length, scratchEngine.slotsByKey.size, scratchEngine.reads.size];
let temporaryCorrect = true;
for (let i = 1; i <= temporary.iterations; i++) for (const q of temporary.queries) {
  const result = scratch.evaluateExpr(q.expr.replaceAll('{i}', String(i)));
  temporaryCorrect &&= q.error ? result.error?.code === q.error : result.value === String(q.factor * i + q.offset) && !result.error;
  temporaryCorrect &&= scratchEngine.programs === sourcePrograms && JSON.stringify(initialCounts) === JSON.stringify([sourcePrograms.compiled, sourcePrograms.compiledSchemas, scratchEngine.env.registry.length, scratchEngine.slotsByKey.size, scratchEngine.reads.size]);
}
check('temporary_programs', temporaryCorrect);
const churn = temporary.churn;
const churnSession = new Session(scratchEntry, new Map([[scratchEntry, churn.source]]));
churnSession.apply({ op: 'bind', name: churn.root, src: { kind: 'inline', text: churn.initial } });
churnSession.run();
let churnCounts: string | undefined;
let churnCorrect = true;
for (let i = 0; i < churn.iterations; i++) {
  for (const kind of ['create', 'remove'] as const) {
    churnSession.apply({ op: 'edit', kind, path: `${churn.root}["item${i}"]`, ...(kind === 'create' ? { expr: churn.item } : {}) });
    const run = churnSession.run(), eng = run.eng!;
    churnCorrect &&= run.diags.length === 0 && eng.serialize(eng.env.roots.get(churn.output), churn.output) === churn[kind === 'create' ? 'present' : 'absent'];
    if (kind === 'remove') {
      const counts = JSON.stringify([eng.programs!.compiled, eng.programs!.compiledSchemas, eng.env.registry.length, eng.slotsByKey.size, eng.reads.size, eng.edits!.revisions.trackedQueries]);
      churnCounts ??= counts;
      churnCorrect &&= counts === churnCounts;
    }
  }
}
check('retained_lifetime', churnCorrect);
total();
