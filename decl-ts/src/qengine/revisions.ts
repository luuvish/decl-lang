// Revision verification over the Engine's existing slot cache. Previous values
// live here only while an invalidated slot awaits verification or recomputation.
import type { Engine } from '../engine.ts';
import { isArr, isMap, isQ, isRange, isRec, isRef, pathStr, valueEq } from '../semantics.ts';
import type { RecInst, Value } from '../semantics.ts';

interface Pending {
  value: Value;
  deps: Set<string>;
  changed: number;
  forced: boolean;
  matches?: (value: Value) => boolean;
}

/** Values with no constructed records or frozen owners can be compared without
 * forcing another query or hiding a change of structural ownership. */
function comparable(v: Value): boolean {
  if (!v || typeof v !== 'object') return true;
  if (isRec(v)) return false;
  if (isRef(v)) return !(v.snap || v.inverseRef || v.roundRef);
  if (isArr(v)) return v.items.every(comparable);
  if (isMap(v)) return [...v.entries.values()].every(comparable);
  return isQ(v) || isRange(v);
}

function equal(a: Value, b: Value): boolean {
  if (isArr(a) && isArr(b))
    return (
      pathStr(a.path) === pathStr(b.path) &&
      a.items.length === b.items.length &&
      a.items.every((v: Value, i: number) => equal(v, b.items[i]))
    );
  if (isMap(a) && isMap(b)) {
    const ae = [...a.entries],
      be = [...b.entries];
    return (
      pathStr(a.path) === pathStr(b.path) &&
      ae.length === be.length &&
      ae.every(([k, v], i) => k === be[i][0] && equal(v, be[i][1]))
    );
  }
  return valueEq(a, b);
}

export class Revisions {
  revision = 0;
  verifiedCutoffs = 0;
  valueCutoffs = 0;
  recomputed = 0;
  private readonly changed = new Map<string, number>();
  private readonly pending = new Map<string, Pending>();
  resolve: ((eng: Engine, key: string) => boolean) | null = null;

  get trackedQueries(): number {
    return this.changed.size;
  }

  prune(eng: Engine): void {
    for (const key of this.changed.keys())
      if (!eng.reads.has(key) && !eng.slotsByKey.has(key)) this.changed.delete(key);
  }

  beginQueries(
    eng: Engine,
    invalid: Set<string>,
    values: Map<string, Value>,
    forced: Set<string>,
    capture: (value: Value) => (next: Value) => boolean,
  ): void {
    this.pending.clear();
    this.revision++;
    for (const key of invalid) {
      if (values.has(key)) {
        const value = values.get(key);
        this.pending.set(key, {
          value,
          deps: new Set(eng.reads.get(key)),
          changed: this.changed.get(key) ?? 0,
          forced: forced.has(key),
          matches: capture(value),
        });
      }
      this.changed.set(key, this.revision);
    }
  }

  force(key: string): void {
    const prior = this.pending.get(key);
    if (prior) prior.forced = true;
    this.changed.set(key, this.revision);
  }

  accept(key: string, value: Value): void {
    const prior = this.pending.get(key);
    if (
      prior &&
      (prior.matches ? prior.matches(value) : comparable(value) && equal(prior.value, value))
    ) {
      this.changed.set(key, prior.changed);
      this.valueCutoffs++;
    }
    this.pending.delete(key);
  }

  finish(): void {
    this.pending.clear();
  }

  /** External observations advance first. A recomputed equal slot restores its
   * earlier change stamp, letting its readers verify without running again. */
  begin(eng: Engine, invalid: Set<string>, forced: Set<string>, dropped: Set<RecInst>): void {
    this.pending.clear();
    this.revision++;
    for (const key of invalid) {
      const found = eng.slotsByKey.get(key),
        slot = found?.inst.slots.get(found.name);
      if (
        found &&
        !dropped.has(found.inst) &&
        slot?.state === 'ok' &&
        slot.compute &&
        comparable(slot.value)
      )
        this.pending.set(key, {
          value: slot.value,
          deps: new Set(eng.reads.get(key)),
          changed: this.changed.get(key) ?? 0,
          forced: forced.has(key),
        });
      this.changed.set(key, this.revision);
    }
  }

  /** Called inside the normal slot step, so cycles, deferral and diagnostics
   * retain their normal forcing stack. Verification reads are not new reads of
   * the caller; recomputed dependencies still record their own dependencies. */
  compute(eng: Engine, key: string, run: () => Value): Value {
    const prior = this.pending.get(key);
    if (!prior) return run();
    const unchanged =
      !prior.forced &&
      eng.verifyReads(() => {
        // Verify owners before their former contents. A removed record must
        // not execute merely to verify a reader's old dependency list.
        const deps = [...prior.deps].sort();
        deps.sort((a, b) => Number(b.startsWith('root:')) - Number(a.startsWith('root:')));
        for (const dep of deps) {
          if (this.pending.has(dep)) {
            if (this.resolve) {
              if (!this.resolve(eng, dep)) return false;
            } else {
              const found = eng.slotsByKey.get(dep);
              if (!found) return false;
              eng.forceSlot(found.inst, found.name);
            }
          }
          if ((this.changed.get(dep) ?? 0) === this.revision) return false;
        }
        return true;
      });
    if (unchanged) {
      eng.reads.set(key, prior.deps);
      this.changed.set(key, prior.changed);
      this.pending.delete(key);
      this.verifiedCutoffs++;
      return prior.value;
    }
    const value = run();
    this.recomputed++;
    this.accept(key, value);
    return value;
  }
}
