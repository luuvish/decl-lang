// Reference-round reuse over the value layer. The dependency graph includes
// member reads (also cache hits), roots, frozen member reads and edge answers.
// A failed eligibility check keeps the fresh-round evaluator as the fallback.
import type { Engine } from '../engine.ts';
import { isArr, isMap, isRec, isRef, isQ, isRange, pathStr, valueEq } from '../semantics.ts';
import type { Env, RecInst, Seg, Value } from '../semantics.ts';

type Edge = Map<string, { path: Seg[]; refs: Seg[][] }>;
class IneligibleSnapshot extends Error {}

const slotKey = (inst: RecInst, name: string) => `${pathStr(inst.path)}.${name}`;

/** Compare the observed container shape; member values have their own reads.
 * Context-dependent results are handled separately by snapshotResult. */
function same(a: Value, b: Value): boolean {
  if (a === b) return true;
  if (isRec(a) && isRec(b)) {
    const as = [...a.slots],
      bs = [...b.slots];
    return (
      a.typeName === b.typeName &&
      a.rt === b.rt &&
      pathStr(a.path) === pathStr(b.path) &&
      as.length === bs.length &&
      as.every(
        ([n, x], i) =>
          n === bs[i][0] && x.kind === bs[i][1].kind && !!x.hidden === !!bs[i][1].hidden,
      ) &&
      a.entryOrder.length === b.entryOrder.length &&
      a.entryOrder.every((n: string, i: number) => n === b.entryOrder[i]) &&
      same({ __map: true, entries: a.extras }, { __map: true, entries: b.extras })
    );
  }
  if (isArr(a) && isArr(b))
    return (
      pathStr(a.path) === pathStr(b.path) &&
      a.items.length === b.items.length &&
      a.items.every((v: Value, i: number) => same(v, b.items[i]))
    );
  if (isMap(a) && isMap(b)) {
    const ae = [...a.entries],
      be = [...b.entries];
    return (
      pathStr(a.path ?? []) === pathStr(b.path ?? []) &&
      ae.length === be.length &&
      ae.every(([k, v], i) => k === be[i][0] && same(v, be[i][1]))
    );
  }
  if (isRec(a) || isRec(b) || isArr(a) || isArr(b) || isMap(a) || isMap(b)) return false;
  return valueEq(a, b);
}

/** A cached scalar can survive an epoch change. A forwarded inverse reference
 * or frozen record view cannot, even if its canonical path is unchanged. */
function snapshotResult(v: Value, eng: Engine, prefix: string, seen = new Set<object>()): boolean {
  if (!v || typeof v !== 'object' || seen.has(v)) return false;
  seen.add(v);
  if (isRef(v)) return !!(v.inverseRef || v.snap);
  if (isArr(v)) return v.items.some((x: Value) => snapshotResult(x, eng, prefix, seen));
  if (isMap(v)) return [...v.entries.values()].some((x) => snapshotResult(x, eng, prefix, seen));
  if (isRec(v)) {
    const path = pathStr(v.path);
    if (
      ((v as any).eng && (v as any).eng !== eng) ||
      !(path === prefix || path.startsWith(prefix + '.') || path.startsWith(prefix + '['))
    )
      return true;
    return (
      [...v.slots.values()].some((s) => snapshotResult(s.value, eng, prefix, seen)) ||
      [...v.extras.values()].some((x) => snapshotResult(x, eng, prefix, seen))
    );
  }
  return false;
}

/** Statistics describe work, without adding user-visible diagnostics. */
export class RoundCache {
  /** Skip dependency recording when the loaded module graph has no reference
   * queries. A missed optimization still follows the complete fresh driver. */
  static needed(env: Env, seen = new Set<Env>()): boolean {
    if (seen.has(env)) return false;
    seen.add(env);
    const contains = (node: unknown): boolean =>
      node !== null &&
      typeof node === 'object' &&
      (('e' in node && node.e === 'referrers') || Object.values(node).some(contains));
    const asts = [
      ...env.typeAsts.values(),
      ...env.funcs.values(),
      ...env.inputs.values(),
      ...env.outputs,
      ...env.unitDecls.values(),
      ...[...env.consts.values()].map((c) => [c.expr, c.type]),
    ];
    return (
      asts.some(contains) ||
      [...env.imports.values()].some((im) => this.needed(im.env, seen)) ||
      [...env.namespaces.values()].some((ns) => this.needed(ns.env, seen))
    );
  }

  rounds = 0;
  reusedRounds = 0;
  retainedRecords = 0;
  invalidatedSlots = 0;
  readonly cleanRecords = new WeakSet<RecInst>();
  private rebased = new WeakMap<object, Value>();

  /** Reused referrer values denote the new round's frozen universe. Frozen
   * copies have roundRef cleared, so their references keep their old owner. */
  value(v: Value, previous: Engine | null): Value {
    if (!previous || !v || typeof v !== 'object' || isRec(v)) return v;
    const cached = this.rebased.get(v);
    if (cached !== undefined) return cached;
    let result = v;
    if (isRef(v) && v.roundRef) result = { ...v, snap: previous };
    else if (isArr(v)) {
      const items = v.items.map((x: Value) => this.value(x, previous));
      if (items.some((x: Value, i: number) => x !== v.items[i])) result = { ...v, items };
    } else if (isMap(v)) {
      const entries = new Map<string, Value>();
      let changed = false;
      for (const [k, x] of v.entries) {
        const y = this.value(x, previous);
        entries.set(k, y);
        changed ||= y !== x;
      }
      if (changed) result = { ...v, entries };
    }
    this.rebased.set(v, result);
    return result;
  }

  advance(eng: Engine, edges: Map<string, Edge>): Engine | null {
    this.rounds++;
    const env = eng.env;
    // Unfinished values and diagnostics require the fresh evaluator's exact
    // retry/reporting behavior. Never guess that an incomplete value is pure.
    if (
      env.diagnostics.length ||
      env.registry.some((r) =>
        [...r.slots.values()].some((s) => s.state !== 'ok' && s.state !== 'absent'),
      )
    )
      return null;
    const priorRecords = new Map((eng.prev?.frozenRegistry ?? []).map((r) => [pathStr(r.path), r]));
    const records = new Map(env.registry.map((r) => [pathStr(r.path), r]));
    if (records.size !== env.registry.length) return null;
    const reverse = new Map<string, Set<string>>();
    const pending: string[] = [];
    const indexes = new Map<Edge, Map<string, string[]>>();
    const answer = (edge: Edge | undefined, target: string): string[] => {
      if (!edge) return [];
      let index = indexes.get(edge);
      if (!index) {
        index = new Map();
        indexes.set(edge, index);
        for (const [source, row] of edge)
          for (const target of new Set(row.refs.map((p) => pathStr(p))))
            (index.get(target) ?? index.set(target, []).get(target)!).push(source);
        for (const paths of index.values()) paths.sort();
      }
      return index.get(target) ?? [];
    };
    for (const [reader, deps] of eng.reads)
      for (const dep of deps) {
        (reverse.get(dep) ?? reverse.set(dep, new Set()).get(dep)!).add(reader);
        if (dep.startsWith('edge:')) {
          // Referrer answers include an empty result: the first incoming edge
          // invalidates the readers that previously found no candidate at all.
          const parts = dep.slice(5).split('|');
          const key = `${parts[0]}|${parts[1]}`,
            target = parts.slice(2).join('|');
          const a = answer(eng.snap?.edges.get(key), target),
            b = answer(edges.get(key), target);
          if (a.length !== b.length || a.some((p, i) => p !== b[i])) pending.push(dep);
        } else if (dep === 'round:nested') {
          pending.push(dep);
        } else if (dep === 'round:reference') {
          const slot = eng.slotsByKey.get(reader);
          const prefix = reader.startsWith('root:') ? reader.slice(5) : reader;
          const value = slot ? slot.inst.slots.get(slot.name)?.value : env.roots.get(prefix);
          if (reader.startsWith('const:') || snapshotResult(value, eng, prefix))
            pending.push(reader);
        } else if (dep.startsWith('snapshot:')) {
          const current = eng.slotsByKey.get(dep.slice(9));
          const a =
            current && priorRecords.get(pathStr(current.inst.path))?.slots.get(current.name);
          const b = current?.inst.slots.get(current.name);
          const av = eng.prev?.snapshotValue ? eng.prev.snapshotValue(a?.value) : a?.value;
          if (!a || !b || a.state !== b.state || !same(av, b.value)) pending.push(dep);
        }
      }
    // Constants may own typed records and lexical closures; conservatively
    // re-open their readers until constant inputs have independent ownership.

    const invalid = new Set<string>();
    const dropped = new Set<RecInst>();
    const roots = new Set<string>();
    const queue = pending;
    const subtrees = new Map<string, RecInst[]>();
    for (const inst of records.values())
      for (let i = 1; i <= inst.path.length; i++) {
        const prefix = pathStr(inst.path.slice(0, i));
        (subtrees.get(prefix) ?? subtrees.set(prefix, []).get(prefix)!).push(inst);
      }
    const addSubtree = (path: Seg[]) => {
      const prefix = pathStr(path);
      for (const inst of subtrees.get(prefix) ?? []) {
        if (dropped.has(inst)) continue;
        dropped.add(inst);
        for (const name of inst.slots.keys()) queue.push(slotKey(inst, name));
      }
    };
    while (queue.length) {
      const key = queue.pop()!;
      if (invalid.has(key)) continue;
      invalid.add(key);
      if (key.startsWith('const:')) return null;
      for (const reader of reverse.get(key) ?? []) queue.push(reader);
      if (key.startsWith('root:')) {
        const name = key.slice(5);
        roots.add(name);
        addSubtree([name]);
      } else {
        const slot = eng.slotsByKey.get(key);
        if (slot) addSubtree([...slot.inst.path, slot.name]);
      }
    }
    // Rebinding an earlier root could change the forcing order of retained
    // later roots. Keep the fresh driver for that case.
    let rebound = false;
    for (const name of env.roots.keys()) {
      rebound ||= roots.has(name);
      if (rebound && !roots.has(name)) return null;
    }
    const dirty = new Set<RecInst>();
    for (const key of invalid) {
      const found = eng.slotsByKey.get(key);
      for (let inst = found?.inst ?? null; inst; inst = inst.parent) dirty.add(inst);
    }
    let snapshot: Engine;
    try {
      snapshot = this.freeze(eng);
    } catch (error) {
      if (error instanceof IneligibleSnapshot) return null;
      throw error;
    }
    for (const inst of dropped)
      for (const name of inst.slots.keys()) {
        const key = slotKey(inst, name);
        eng.reads.delete(key);
        eng.slotsByKey.delete(key);
      }
    const copiedSlots = new Set<RecInst>();
    for (const key of invalid) {
      const found = eng.slotsByKey.get(key);
      if (found) {
        if (!copiedSlots.has(found.inst)) {
          found.inst.slots = new Map(found.inst.slots);
          copiedSlots.add(found.inst);
        }
        const slot = found.inst.slots.get(found.name)!;
        if (slot.compute) {
          found.inst.slots.set(found.name, { ...slot, state: 'unforced', value: undefined });
          this.invalidatedSlots++;
        }
      }
      eng.reads.delete(key);
    }
    env.registry.splice(0, env.registry.length, ...env.registry.filter((r) => !dropped.has(r)));
    for (const inst of env.registry) {
      if (dirty.has(inst)) this.cleanRecords.delete(inst);
      else this.cleanRecords.add(inst);
    }
    for (const name of roots) env.roots.delete(name);
    eng.deferredSlots = [];
    eng.deferredRoots = [];
    eng.roundRoots = roots;
    eng.phase = 1;
    eng.snap = null;
    eng.queried.clear();
    for (const deps of eng.reads.values())
      for (const dep of deps)
        if (dep.startsWith('edge:')) eng.queried.add(dep.slice(5).split('|').slice(0, 2).join('|'));
    eng.refIndex.clear();
    eng.computingEdges.clear();
    eng.edgeBases = [];
    for (const reset of eng.roundResets) reset();

    this.rebased = new WeakMap();
    for (const [name, value] of env.roots) env.roots.set(name, this.value(value, snapshot));
    this.retainedRecords += env.registry.length;
    this.reusedRounds++;
    return snapshot;
  }

  private freeze(eng: Engine): Engine {
    const frozen = Object.assign(Object.create(Object.getPrototypeOf(eng)), eng) as Engine;
    const copies = new WeakMap<object, Value>();
    const previous = eng.prev;
    const normalizer = new RoundCache();
    const copy = (raw: Value): Value => {
      const v = normalizer.value(raw, previous);
      if (!v || typeof v !== 'object') return v;
      const hit = copies.get(v);
      if (hit) return hit;
      if (isRef(v)) return v.roundRef ? { ...v, roundRef: false } : v;
      if (isQ(v) || isRange(v)) return { ...v };
      if (v.__jobj) return v; // lexical JSON is immutable input data
      if (isRec(v)) {
        if ((v as any).eng && (v as any).eng !== eng) return v;
        const r = { ...v, eng: frozen };
        copies.set(v, r);
        r.parent = v.parent ? copy(v.parent) : null;
        return r;
      }
      if (isArr(v)) {
        const a = { ...v, items: [] as Value[] };
        copies.set(v, a);
        a.items = v.items.map(copy);
        if (a.items.every((x: Value, i: number) => x === v.items[i])) {
          copies.set(v, v);
          return v;
        }
        return a;
      }
      if (isMap(v)) {
        const m = { ...v, entries: new Map<string, Value>() };
        copies.set(v, m);
        let changed = false;
        for (const [k, x] of v.entries) {
          const y = copy(x);
          m.entries.set(k, y);
          changed ||= x !== y;
        }
        if (!changed) {
          copies.set(v, v);
          return v;
        }
        return m;
      }
      // Lazy prevalues and closures capture execution context; retain the
      // fresh-round behavior until they can be represented as snapshot data.
      throw new IneligibleSnapshot('context-bearing snapshot');
    };
    frozen.frozenRegistry = eng.env.registry.map(copy);
    frozen.frozenRoots = new Map([...eng.env.roots].map(([k, v]) => [k, copy(v)]));
    // Validate and memoize container copies once. Record slot maps are shared
    // with the snapshot until the live owner replaces a slot (copy-on-write).
    for (const inst of eng.env.registry) {
      for (const slot of inst.slots.values()) copy(slot.value);
      for (const value of inst.extras.values()) copy(value);
    }
    frozen.slotsByKey = new Map();
    frozen.snapshotValue = copy;
    frozen.roundCache = null;
    frozen.roundRoots = null;
    frozen.track = false;
    frozen.computing = [];
    frozen.reads = new Map();
    return frozen;
  }
}
