// The incremental, memoized query core of the new engine (qengine/DESIGN.md).
//
// A demand-driven database of queries. Some keys are *inputs* (set from
// outside); the rest are *derived* — computed by a resolver-supplied function
// that reads other queries through the context, so its dependencies are
// recorded automatically. Results are memoized with a global revision and two
// stamps per entry, giving early cutoff on both axes:
//   - verify cutoff : if none of an entry's dependencies changed since it was
//     verified, it is reused without recomputing.
//   - value cutoff  : a recompute that yields an equal value does not advance
//     the entry's changed-revision, so *its* dependents cut off in turn.
// This is Salsa/Adapton-shaped, generic, and independent of the decl language;
// the decl layer (compiled expressions as derived queries) is built on top.

/** a query key — opaque string; the decl layer defines the grammar of keys */
export type Key = string;

/** what a derived query reads to compute its value */
export interface Cx {
  /** read another query's value, recording it as a dependency */
  query(key: Key): unknown;
}

/** compute a derived query's value from other queries */
export type Compute = (cx: Cx) => unknown;

/** map a derived key to its compute; return undefined for an unknown key */
export type Resolve = (key: Key) => Compute | undefined;

/** structural equality for value/verify cutoff (default: identity) */
export type Eq = (a: unknown, b: unknown) => boolean;

interface Entry {
  value: unknown;
  deps: Key[];
  changedRev: number; // last revision the value actually changed
  verifiedRev: number; // last revision the value was confirmed current
}

/** thrown when a derived query (transitively) reads itself */
export class QueryCycle extends Error {
  readonly cycle: Key[];
  constructor(cycle: Key[]) {
    super(`query cycle: ${cycle.join(' -> ')}`);
    this.cycle = cycle;
  }
}

export class Db {
  private rev = 0;
  private readonly memo = new Map<Key, Entry>();
  private readonly inputs = new Map<Key, unknown>();
  private readonly inputRev = new Map<Key, number>();
  private readonly resolve: Resolve;
  private readonly eq: Eq;
  // the queries currently (re)computing, innermost last — dependency capture
  // and cycle detection
  private readonly stack: { key: Key; deps: Key[] }[] = [];

  constructor(opts: { resolve: Resolve; eq?: Eq }) {
    this.resolve = opts.resolve;
    this.eq = opts.eq ?? Object.is;
  }

  /** the current revision (advances when an input changes) */
  get revision(): number {
    return this.rev;
  }

  /**
   * Set an input's value. If it differs from the current value the revision
   * advances and dependents will recompute on demand; setting an equal value
   * is a no-op (nothing downstream changed).
   */
  setInput(key: Key, value: unknown): void {
    if (this.inputs.has(key) && this.eq(this.inputs.get(key), value)) return;
    this.rev++;
    this.inputs.set(key, value);
    this.inputRev.set(key, this.rev);
  }

  /** read a query's value, recording it as a dependency of the caller (if any) */
  query(key: Key): unknown {
    const top = this.stack[this.stack.length - 1];
    if (top) top.deps.push(key);
    return this.evaluate(key);
  }

  /** bring a query up to date and return its value, without recording a dependency */
  private evaluate(key: Key): unknown {
    if (this.inputs.has(key)) return this.inputs.get(key);
    const m = this.memo.get(key);
    if (m && m.verifiedRev === this.rev) return m.value; // already current
    if (m && this.depsUnchanged(m)) {
      m.verifiedRev = this.rev; // verify cutoff: no dependency changed
      return m.value;
    }
    return this.recompute(key, m);
  }

  private depsUnchanged(m: Entry): boolean {
    for (const d of m.deps) {
      this.evaluate(d); // bring the dependency current (may recompute it)
      if (this.changedRevOf(d) > m.verifiedRev) return false;
    }
    return true;
  }

  private recompute(key: Key, prev: Entry | undefined): unknown {
    if (this.stack.some((f) => f.key === key)) {
      throw new QueryCycle([...this.stack.map((f) => f.key), key]);
    }
    const compute = this.resolve(key);
    if (!compute) throw new Error(`no such query: ${key}`);
    const frame = { key, deps: [] as Key[] };
    this.stack.push(frame);
    let value: unknown;
    try {
      value = compute(this);
    } finally {
      this.stack.pop();
    }
    // value cutoff: an equal recompute keeps the old changed-revision, so
    // dependents that only read this query need not recompute
    const changedRev = prev && this.eq(value, prev.value) ? prev.changedRev : this.rev;
    this.memo.set(key, { value, deps: frame.deps, changedRev, verifiedRev: this.rev });
    return value;
  }

  private changedRevOf(key: Key): number {
    if (this.inputs.has(key)) return this.inputRev.get(key)!;
    return this.memo.get(key)!.changedRev;
  }
}
