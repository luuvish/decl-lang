// The incremental query core (src/qengine/db.ts): memoization, dependency
// tracking, and the two early cutoffs. Recompute counts prove the cutoffs.
import { Db, QueryCycle } from '../../src/qengine/db.ts';

let pass = 0;
let fail = 0;
const check = (name: string, ok: boolean, detail = '') => {
  if (ok) {
    pass++;
    console.log(`  ok   ${name}`);
  } else {
    fail++;
    console.log(`  FAIL ${name}: ${detail}`);
  }
};

console.log('== qengine: the incremental query core ==');

// memoize; recompute on a real change; verify and value cutoff
{
  const calls = { sum: 0, doubled: 0, sign: 0 };
  const db = new Db({
    eq: (a, b) => a === b,
    resolve: (k) => {
      if (k === 'sum')
        return (cx) => {
          calls.sum++;
          return (cx.query('a') as number) + (cx.query('b') as number);
        };
      if (k === 'doubled')
        return (cx) => {
          calls.doubled++;
          return (cx.query('sum') as number) * 2;
        };
      if (k === 'sign')
        return (cx) => {
          calls.sign++;
          return Math.sign(cx.query('a') as number);
        };
      return undefined;
    },
  });

  db.setInput('a', 1);
  db.setInput('b', 2);
  check('first query computes the demanded chain only', db.query('doubled') === 6 && calls.sum === 1 && calls.doubled === 1 && calls.sign === 0);

  db.query('doubled'); // nothing changed
  check('no change recomputes nothing', calls.sum === 1 && calls.doubled === 1);

  db.setInput('b', 10); // sum and doubled both change
  check('a real change recomputes the chain', db.query('doubled') === 22 && calls.sum === 2 && calls.doubled === 2);

  // a and b change but their sum does not: `sum` recomputes, its value is
  // unchanged, so `doubled` cuts off (value cutoff propagates)
  db.setInput('a', 5);
  db.setInput('b', 6);
  const v = db.query('doubled') as number;
  check('value cutoff: an equal recompute stops its dependents', v === 22 && calls.sum === 3 && calls.doubled === 2, `doubled=${v} sum=${calls.sum} doubled#=${calls.doubled}`);
}

// verify cutoff: an unrelated input change recomputes nothing
{
  const calls = { sum: 0 };
  const db = new Db({
    eq: (a, b) => a === b,
    resolve: (k) =>
      k === 'sum'
        ? (cx) => {
            calls.sum++;
            return (cx.query('a') as number) + (cx.query('b') as number);
          }
        : undefined,
  });
  db.setInput('a', 1);
  db.setInput('b', 2);
  db.setInput('c', 9);
  db.query('sum');
  db.setInput('c', 99); // `sum` never read `c`
  check('verify cutoff: an unrelated input recomputes nothing', db.query('sum') === 3 && calls.sum === 1, `sum#=${calls.sum}`);
}

// a query that reads itself is a cycle
{
  const graph: Record<string, (cx: { query(k: string): unknown }) => unknown> = {
    x: (cx) => cx.query('y'),
    y: (cx) => cx.query('x'),
  };
  const db = new Db({ resolve: (k) => graph[k] });
  let threw = false;
  try {
    db.query('x');
  } catch (e) {
    threw = e instanceof QueryCycle;
  }
  check('a query that reads itself is a cycle', threw);
}

// live edits over a derived structure (the session model): the node
// set and each weight are inputs; the structure and totals are derived. A
// create/update/remove/unrelated edit recomputes only the slice it reaches.
{
  const load: Record<string, number> = {};
  let total = 0;
  let structure = 0;
  const db = new Db({
    eq: (a, b) => {
      if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((x, i) => x === b[i]);
      return a === b;
    },
    resolve: (key) => {
      if (key === 'structure')
        return (cx) => {
          structure++;
          return cx.query('nodes');
        };
      if (key === 'total')
        return (cx) => {
          total++;
          const ns = cx.query('structure') as string[];
          return ns.reduce((s, n) => s + (cx.query(`load:${n}`) as number), 0);
        };
      if (key.startsWith('load:')) {
        const n = key.slice(5);
        return (cx) => {
          load[n] = (load[n] ?? 0) + 1;
          return (cx.query(`w:${n}`) as number) * 10;
        };
      }
      return undefined;
    },
  });

  db.setInput('nodes', ['a', 'b']);
  db.setInput('w:a', 1);
  db.setInput('w:b', 2);
  check('structure: initial total', db.query('total') === 30 && total === 1 && structure === 1 && load.a === 1 && load.b === 1);

  // create a node: structure + total recompute; existing loads cut off
  db.setInput('nodes', ['a', 'b', 'c']);
  db.setInput('w:c', 3);
  check('create: only the new node computes its load', db.query('total') === 60 && load.a === 1 && load.b === 1 && load.c === 1 && total === 2, `total=${total} load=${JSON.stringify(load)}`);

  // update a weight: that node's load + total recompute; the others cut off
  db.setInput('w:b', 5);
  check('update: only the changed node recomputes', db.query('total') === 90 && load.a === 1 && load.b === 2 && load.c === 1 && total === 3, `total=${total} load=${JSON.stringify(load)}`);

  // remove a node: structure + total recompute; survivors cut off
  db.setInput('nodes', ['b', 'c']);
  check('remove: survivors are not recomputed', db.query('total') === 80 && load.b === 2 && load.c === 1 && total === 4, `total=${total} load=${JSON.stringify(load)}`);

  // an edit outside the current structure recomputes nothing
  db.setInput('w:a', 99);
  check('an edit outside the structure recomputes nothing', db.query('total') === 80 && total === 4, `total=${total}`);
}

console.log(`\nTOTAL ${pass} ok, ${fail} failed`);
process.exitCode = fail > 0 ? 1 : 0;
