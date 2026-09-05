// session (tests/internal/checks.json): the operation log — apply, undo,
// redo, and a new operation after undo discarding the redo tail.
import { join } from 'node:path';
import { initParser } from '../../src/node.ts';
import { Session, SessionError } from '../../src/session.ts';
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
total();
