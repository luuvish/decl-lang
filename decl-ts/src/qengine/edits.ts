// A Session revision replays root binding over retained record identities.
// The query graph verifies individual member inputs, values and membership.
import type { Engine, Scope } from '../engine.ts';
import type { RecInst, RT, Seg, Slot, Value } from '../semantics.ts';
import {
  ABSENT,
  isArr,
  isMap,
  isQ,
  isRange,
  isRec,
  isRef,
  pathStr,
  valueEq,
} from '../semantics.ts';
import { Revisions } from './revisions.ts';

function sameRaw(a: Value, b: Value): boolean {
  if (a === b) return true;
  if (Array.isArray(a) && Array.isArray(b))
    return a.length === b.length && a.every((x, i) => sameRaw(x, b[i]));
  if (a?.__jobj && b?.__jobj)
    return (
      a.entries.length === b.entries.length &&
      a.entries.every(
        ([k, v]: [string, Value], i: number) =>
          k === b.entries[i][0] && sameRaw(v, b.entries[i][1]),
      )
    );
  if (a?.__expr && b?.__expr) {
    const x: Scope = a.scope,
      y: Scope = b.scope;
    return (
      a.__expr === b.__expr &&
      x.inst === y.inst &&
      x.menv === y.menv &&
      x.rootName === y.rootName &&
      x.locals.size === y.locals.size &&
      [...x.locals].every(([k, v]) => y.locals.has(k) && sameRaw(v, y.locals.get(k)))
    );
  }
  if (isQ(a) && isQ(b)) return valueEq(a, b);
  return false;
}

/** Snapshot the observable shape before records receive their next slot maps.
 * A record's values are separate member queries; whole-value consumers record
 * an aggregate observation, which is conservatively invalidated on edits. */
function capture(v: Value): (next: Value) => boolean {
  if (isRec(v)) {
    const rt = v.rt,
      owner = (v as any).eng,
      path = pathStr(v.path),
      entries = [...v.entryOrder];
    const members = [...v.slots].map(([n, s]) => [n, s.kind, !!s.hidden]);
    const extras = [...v.extras].map(([k, x]) => [k, capture(x)] as const);
    return (n) =>
      isRec(n) &&
      n === v &&
      n.rt === rt &&
      (n as any).eng === owner &&
      pathStr(n.path) === path &&
      n.entryOrder.length === entries.length &&
      n.entryOrder.every((k: string, i: number) => k === entries[i]) &&
      n.slots.size === members.length &&
      [...n.slots].every(
        ([k, s], i) =>
          k === members[i][0] && s.kind === members[i][1] && !!s.hidden === members[i][2],
      ) &&
      n.extras.size === extras.length &&
      [...n.extras].every(([k, x], i) => k === extras[i][0] && extras[i][1](x));
  }
  if (isArr(v)) {
    const path = pathStr(v.path),
      items = v.items.map(capture);
    return (n) =>
      isArr(n) &&
      pathStr(n.path) === path &&
      n.items.length === items.length &&
      items.every((matches: (x: Value) => boolean, i: number) => matches(n.items[i]));
  }
  if (isMap(v)) {
    const path = pathStr(v.path),
      entries = [...v.entries].map(([k, x]) => [k, capture(x)] as const);
    return (n) =>
      isMap(n) &&
      pathStr(n.path) === path &&
      n.entries.size === entries.length &&
      [...n.entries].every(([k, x], i) => k === entries[i][0] && entries[i][1](x));
  }
  if (isRef(v)) {
    if (v.snap || v.inverseRef || v.roundRef) return () => false;
    return (n) => isRef(n) && !(n.snap || n.inverseRef || n.roundRef) && valueEq(v, n);
  }
  if (v === null || typeof v !== 'object' || isQ(v) || isRange(v)) return (n) => valueEq(v, n);
  return () => false;
}

export class Edits {
  active = false;
  readonly revisions = new Revisions();
  slotComputes = 0;
  retainedRecords = 0;
  preparedQueries = 0;
  private readonly inputs = new WeakMap<RecInst, Map<string, Value>>();
  private readonly roots = new Map<string, { raw: Value; expression: boolean }>();
  private pool = new Map<string, RecInst>();
  private live = new Set<RecInst>();
  private invalid = new Set<string>();
  private rootValues = new Map<string, Value>();
  private rootReads = new Map<string, Set<string>>();
  private bindingReuse = true;

  constructor() {
    this.revisions.resolve = (eng, key) => {
      const slot = eng.slotsByKey.get(key);
      if (slot) {
        eng.forceSlot(slot.inst, slot.name);
        return true;
      }
      if (key.startsWith('const:')) {
        const [index, name] = key.slice(6).split('|');
        const env = [...eng.constEnvs][Number(index)];
        if (env?.consts.has(name)) {
          eng.forceConstIn(env, name, '');
          return true;
        }
      }
      return false;
    };
  }

  begin(eng: Engine, changed: Set<string>, documents: Map<string, Value>): void {
    const values = new Map<string, Value>(),
      forced = new Set<string>();
    const errors = eng.env.diagnostics.length > 0;
    this.bindingReuse = !errors;
    this.pool = new Map(eng.env.registry.map((r) => [pathStr(r.path), r]));
    this.live.clear();
    this.retainedRecords = 0;
    this.rootValues = new Map(eng.env.roots);
    this.rootReads = new Map(
      [...eng.env.roots.keys()].map((n) => [`root:${n}`, new Set(eng.reads.get(`root:${n}`))]),
    );
    const knownInputs = new Set<string>();
    const collect = (v: Value, into: Set<string>): void => {
      if (isRec(v))
        for (const [n, slot] of v.slots) {
          const key = `${pathStr(v.path)}.${n}`;
          eng.slotsByKey.set(key, { inst: v, name: n });
          into.add(key);
          if (slot.state === 'ok') collect(slot.value, into);
        }
      else if (isArr(v)) v.items.forEach((x: Value) => collect(x, into));
      else if (isMap(v)) [...v.entries.values()].forEach((x) => collect(x, into));
    };
    const diff = (before: Value, next: Value, value: Value, producer: string): void => {
      if (sameRaw(before, next)) return;
      knownInputs.add(producer);
      forced.add(producer);
      if (isRec(value) && before?.__jobj && next?.__jobj) {
        const a = new Map<string, Value>(before.entries),
          b = new Map<string, Value>(next.entries);
        for (const name of new Set([...a.keys(), ...b.keys()])) {
          if (a.has(name) === b.has(name) && sameRaw(a.get(name), b.get(name))) continue;
          const slot = value.slots.get(name);
          if (slot) {
            const key = `${pathStr(value.path)}.${name}`;
            eng.slotsByKey.set(key, { inst: value, name });
            diff(a.get(name), b.get(name), slot.value, key);
            forced.add(key);
          }
        }
      } else if (isArr(value) && Array.isArray(before) && Array.isArray(next)) {
        for (let i = 0; i < Math.max(before.length, next.length); i++)
          diff(before[i], next[i], value.items[i], producer);
      } else if (isMap(value) && before?.__jobj && next?.__jobj) {
        const a = new Map<string, Value>(before.entries),
          b = new Map<string, Value>(next.entries);
        for (const k of new Set([...a.keys(), ...b.keys()]))
          diff(a.get(k), b.get(k), value.entries.get(k), producer);
      } else collect(value, forced);
    };
    for (const name of changed) {
      const previous = this.roots.get(name),
        value = eng.env.roots.get(name),
        key = `root:${name}`;
      forced.add(key);
      knownInputs.add(key);
      if (previous && !previous.expression && documents.has(name))
        diff(previous.raw, documents.get(name), value, key);
      else collect(value, forced);
    }
    const readers = new Map<string, string[]>();
    for (const [key, deps] of eng.reads)
      for (const dep of deps) {
        (readers.get(dep) ?? readers.set(dep, []).get(dep)!).push(key);
        if (['edge:', 'snapshot:', 'round:', 'value:'].some((p) => dep.startsWith(p)))
          forced.add(key);
      }
    for (const [name, value] of eng.env.roots) values.set(`root:${name}`, value);
    for (const inst of eng.env.registry)
      for (const [name, slot] of inst.slots) {
        const key = `${pathStr(inst.path)}.${name}`;
        if (slot.state === 'ok' || slot.state === 'absent')
          values.set(key, slot.state === 'absent' ? ABSENT : slot.value);
        if (errors) {
          forced.add(key);
          eng.slotsByKey.set(key, { inst, name });
        }
      }
    for (const [index, env] of [...eng.constEnvs].entries())
      for (const [name, con] of env.consts) {
        const key = `const:${index}|${name}`;
        if (con.state === 'ok') values.set(key, con.value);
        if (errors) forced.add(key);
      }
    // Captured scopes and reference answers belong to their original owner.
    // A matching path alone cannot make such a result reusable in a new edit.
    const retain = (v: Value, seen = new Set<Value>()): boolean => {
      if (v === null || typeof v !== 'object' || isQ(v) || isRange(v)) return true;
      if (seen.has(v)) return true;
      seen.add(v);
      if (isRef(v)) return !(v.snap || v.inverseRef || v.roundRef);
      if (isRec(v))
        return (
          (v as any).eng === eng &&
          [...(this.inputs.get(v)?.values() ?? [])].every((x) => retain(x, seen))
        );
      if (isArr(v)) return v.items.every((x: Value) => retain(x, seen));
      if (isMap(v)) return [...v.entries.values()].every((x) => retain(x, seen));
      if (Array.isArray(v)) return v.every((x) => retain(x, seen));
      if (v.__jobj) return v.entries.every(([, x]: [string, Value]) => retain(x, seen));
      if (v.__expr) {
        const sc: Scope = v.scope;
        return (
          (!sc.inst || (sc.inst as any).eng === eng) &&
          [...sc.locals.values()].every((x) => retain(x, seen))
        );
      }
      return false;
    };
    if (eng.queried.size) for (const [key, value] of values) if (!retain(value)) forced.add(key);
    if (errors) for (const key of eng.reads.keys()) forced.add(key);
    const aggregate = [...eng.reads.values()].some((deps) =>
      [...deps].some((d) => d.startsWith('value:')),
    );
    const invalid = new Set<string>(),
      queue = [...forced];
    if (aggregate) queue.push(...values.keys());
    while (queue.length) {
      const key = queue.pop()!;
      if (invalid.has(key)) continue;
      invalid.add(key);
      // An expression producer can change its descendants' captured inputs.
      // Direct document changes were already diffed down to supplied members.
      if (!knownInputs.has(key) && values.has(key)) {
        const descendants = new Set<string>();
        collect(values.get(key), descendants);
        for (const child of descendants) if (!invalid.has(child)) queue.push(child);
      }
      queue.push(...(readers.get(key) ?? []));
    }
    this.invalid = invalid;
    this.preparedQueries = invalid.size;
    this.revisions.beginQueries(eng, invalid, values, forced, capture);
    const copied = new Set<RecInst>();
    for (const key of invalid) {
      const found = eng.slotsByKey.get(key);
      if (!found) continue;
      const slot = found.inst.slots.get(found.name);
      if (!slot?.compute) continue;
      if (!copied.has(found.inst)) {
        found.inst.slots = new Map(found.inst.slots);
        copied.add(found.inst);
      }
      found.inst.slots.set(found.name, { ...slot, state: 'unforced', value: undefined });
    }
    for (const [index, env] of [...eng.constEnvs].entries())
      for (const [name, con] of env.consts)
        if (invalid.has(`const:${index}|${name}`)) {
          con.state = 'unforced';
          delete con.value;
        }
    eng.env.roots.clear();
    eng.env.registry.splice(0);
    eng.env.diagnostics.splice(0);
    eng.failedInputs.clear();
    eng.deferredSlots = [];
    eng.deferredRoots = [];
    eng.phase = 1;
    eng.settled = false;
    eng.prev = null;
    eng.snap = null;
    eng.queried.clear();
    eng.refIndex.clear();
    eng.computingEdges.clear();
    eng.edgeBases = [];
    eng.roundRoots = null;
    eng.snapshotValue = null;
    this.active = true;
  }

  root(eng: Engine, name: string, raw: Value, expression: boolean, run: () => Value): Value {
    const previous = this.roots.get(name);
    this.roots.set(name, { raw, expression });
    if (!this.active) return run();
    const key = `root:${name}`;
    if (
      !previous ||
      previous.expression !== expression ||
      !(expression ? previous.raw === raw : sameRaw(previous.raw, raw))
    )
      this.revisions.force(key);
    if (!this.invalid.has(key) && this.rootValues.has(name)) {
      eng.reads.set(key, this.rootReads.get(key) ?? new Set());
      const value = this.rootValues.get(name);
      this.activate(eng, value);
      return value;
    }
    return this.compute(eng, key, run);
  }

  unchanged(inst: RecInst, entries: [string, Value][]): boolean {
    const before = this.inputs.get(inst);
    return (
      this.bindingReuse &&
      !!before &&
      before.size === entries.length &&
      inst.entryOrder.length === entries.length &&
      entries.every(
        ([k, v], i) => inst.entryOrder[i] === k && before.has(k) && sameRaw(before.get(k), v),
      )
    );
  }

  record(rt: RT, path: Seg[], parent: RecInst | null): RecInst | undefined {
    if (!this.active) return undefined;
    const old = this.pool.get(pathStr(path));
    return old && old.rt === rt && old.parent === parent ? old : undefined;
  }

  bound(
    eng: Engine,
    inst: RecInst,
    entries: [string, Value][],
    oldSlots?: Map<string, Slot>,
  ): void {
    const before = this.inputs.get(inst),
      next = new Map(entries);
    if (this.active)
      for (const [name, slot] of inst.slots) {
        const key = `${pathStr(inst.path)}.${name}`;
        if (
          !before ||
          before.has(name) !== next.has(name) ||
          !sameRaw(before.get(name), next.get(name)) ||
          oldSlots?.get(name)?.kind !== slot.kind
        )
          this.revisions.force(key);
        else if (oldSlots?.has(name)) inst.slots.set(name, oldSlots.get(name)!);
        eng.slotsByKey.set(key, { inst, name });
      }
    this.inputs.set(inst, next);
    if (this.active)
      for (const slot of inst.slots.values())
        if (slot.state === 'ok') this.activate(eng, slot.value);
  }

  register(eng: Engine, inst: RecInst): void {
    if (this.live.has(inst)) return;
    this.live.add(inst);
    eng.env.registry.push(inst);
    if (this.pool.get(pathStr(inst.path)) === inst) this.retainedRecords++;
  }

  activate(eng: Engine, value: Value): void {
    if (!this.active || eng.noReg > 0) return;
    if (isRec(value)) {
      if ((value as any).eng !== eng || this.live.has(value)) return;
      this.register(eng, value);
      for (const s of value.slots.values()) if (s.state === 'ok') this.activate(eng, s.value);
    } else if (isArr(value)) for (const v of value.items) this.activate(eng, v);
    else if (isMap(value)) for (const v of value.entries.values()) this.activate(eng, v);
  }

  compute(eng: Engine, key: string, run: () => Value): Value {
    const value = this.revisions.compute(eng, key, run);
    this.activate(eng, value);
    return value;
  }

  absent(key: string): void {
    if (this.active) this.revisions.accept(key, ABSENT);
  }

  finish(eng: Engine): void {
    if (!this.active) return;
    for (const [key, found] of eng.slotsByKey)
      if (!this.live.has(found.inst)) {
        eng.slotsByKey.delete(key);
        eng.reads.delete(key);
      }
    for (const name of this.roots.keys()) if (!eng.env.roots.has(name)) this.roots.delete(name);
    // Assertions run again after settling, so their old reads have no cache
    // to preserve. Drop removed roots along with removed member queries.
    for (const key of eng.reads.keys())
      if (
        key.startsWith('assert:') ||
        (key.startsWith('root:') && !eng.env.roots.has(key.slice(5)))
      )
        eng.reads.delete(key);
    this.revisions.finish();
    this.revisions.prune(eng);
    this.pool.clear();
    this.live.clear();
    this.rootValues.clear();
    this.rootReads.clear();
    this.invalid.clear();
    this.active = false;
  }
}
