# 07. Relationships

Values relate in exactly two ways (D26): **composition** — ownership,
forming a tree — and **reference** — `ref<T>`, forming graphs. This
chapter defines canonical paths, navigation and context variables,
reference construction and integrity, and the reverse query `$referrers`.

## 7.1 Composition and reference

| | Composition (default) | Reference (`ref<T>`) |
|---|---|---|
| A property's value is | owned — part of this value | a pointer to a value owned elsewhere |
| Shape | tree: single parent | graph: shared targets, cycles allowed |
| Cycles | impossible (the member dependency graph is acyclic, D23) | allowed |
| Serialized as | the value itself | a canonical path string (D29) |
| Missing target | — | dangling-reference validation error |

Values are immutable; composition therefore has copy semantics with no
observable sharing.

## 7.2 Canonical paths

A path names a location under an evaluation root:

```
path     = root-name segment*
segment  = "." identifier          // record member (identifier-shaped name)
         | "[" int "]"             // array index (0-based)
         | "[" string "]"          // map key, or record member with a non-identifier name
```

- `demo.services[2].port`, `registry["svc-a"].endpoint`
- **Canonical form** mirrors the access rules of the expression surface
  (§4.3): a record member is `.name` when its name is dot-spellable
  (identifier-shaped, not a keyword) and `["…"]` otherwise; a **map key
  is always** `["…"]`, even when identifier-shaped; array indices are
  `[int]`; no spaces. So `.` in a path always means a declared record member, and
  no location has two canonical spellings. Serialization and
  diagnostics emit exactly this form, and a reference path string from
  input data must **be** canonical — a non-canonical spelling
  (`demo["services"]`) does not resolve (§7.5).
- **Document-relative form**: in *serialized reference strings and
  bound documents only*, the root may be spelled `$` — "the evaluation
  root this document is bound to" (`"$.services[0]"`) — keeping
  documents self-contained and independent of the slot name they bind
  to ([10. Interchange](10_interchange.md)). `$` is a wire form: the
  location it denotes is an absolute path, and reference identity,
  place equality, diagnostics, and path ordering always use the
  absolute form.
- **Canonical path order** — the total order used for `$referrers` results
  (§7.6) and diagnostic sorting (§6.7) — compares **segment-wise**:
  root names lexicographically (by Unicode code point), member and key
  segments lexicographically, array indices **numerically**
  (`[2] < [10]`), and a shorter path that is a prefix of a longer one
  precedes it. Byte comparison of path strings is *not* the order
  (`[10]` would sort before `[2]`).

## 7.3 Navigation and context variables

Member expressions (defaults, derived members, constraint conditions)
navigate with `.`, `[…]`, `?.` under the rules of
[04. Expressions](04_expressions.md), starting from sibling names or
from a context variable:

| Variable | Meaning | Type |
|---|---|---|
| `$this` | the record value whose member is being evaluated | the enclosing type |
| `$parent` | the value that owns `$this` | site-dependent |
| `$root` | the evaluation root's value | site-dependent |
| `$key` | the key or index under which `$this` sits in its parent collection | `string` (map) / `int` (array), site-dependent |
| `$path` | the canonical path of `$this` | `string` |
| `$referrers` | reverse query, §7.6 | — |

- A bare name `x` in a member expression resolves
  **nearest-enclosing-instance-first**: the members of `$this`, then of
  `$parent`, and so on up the ownership chain, before module and
  imported names — the one sanctioned lookup-order exception to the
  no-shadowing rule (D27). It is resolution order over coexisting name
  spaces, not a rebinding, and the outer name stays addressable
  outside member expressions. The chain (not just siblings) is what
  lets a nested literal reach its container's entries — `source:
  ports["si0"]` inside an edge literal reaches the enclosing node's
  `ports` (§4.2); sibling-only resolution would contradict this
  chapter's own §7.4 example, a defect the §0.6 evaluator spike caught
  in execution.
- **Context-dependent types.** A type whose member expressions mention
  `$parent`, `$root`, or `$key` depends on where it is embedded. Such
  a type is checked **per composition site**, the way generics are
  checked per instantiation (§3.15): at every member declaration that
  embeds it, `$parent` is typed as the embedding record, `$key` as the
  embedding collection's key/index type, `$root` as the root's type.
  A site that gives the variable no meaning — `$parent` where the type
  is itself the evaluation root's type, `$key` where the immediate
  owner is not a collection element — is a compile error **at that
  site**. Types that avoid these variables stay position-independent
  and are checked once.

```decl
type Port = {
    name: PortName
    width: int = $parent.data_width     // checked at each embedding site
}

type Router = {
    data_width: DataWidth
    ports: Port[]                       // ok: $parent is Router here
}
```

## 7.4 Reference values

How a reference is *written*: in a `ref<T>`-typed position, a
**navigation expression denotes the reference itself** — the location,
not a copy. No marker syntax exists or is needed: a `ref<T>` position
cannot hold a `T` value, so there is no ambiguity to resolve (the same
type-directed reading that lets `10ms` be a quantity).

```decl
output net: Network = {
    services: [ { name: "a" }, { name: "b" } ]
    links: [
        { source: services[0], target: services[1] }   // references, not copies
    ]
}
```

- Statically, the navigated location's type must be `⊑ T`.
- In the interchange surface, the same reference is the canonical path
  **string** (`"net.services[0]"`) — the input-binding form and the
  serialized form (D29, [10. Interchange](10_interchange.md)); the
  duality mirrors quantities (source literal vs interchange object,
  D15).
- **Using a reference**: member access and indexing navigate through
  it transparently (`link.source.name` reads the target's member);
  spread (`{ ...link.source }`) copies the target's entries.
- **Type-directed dereference — the mirror rule.** Just as a
  navigation in a `ref<T>` position denotes the reference, a reference
  in a **`T`-typed position denotes the target's value**:

  ```decl
  const neighbors: Service[] = [l.source for l in inbound]
  const first: Service = inbound[0].source
  ```

  The expected type decides, in both directions; without an expected
  value type, a reference stays a reference. Dereference happens only
  where a declaration asks for the value — never silently. A dangling
  reference reported under §7.5 taints its dereference (§6.6).
- In every other position the reference behaves as a reference value:
  equality compares **canonical target paths** (§4.5), and
  serialization emits the path.
- `ref<T₁> ⊑ ref<T₂>` iff `T₁ ⊑ T₂` (§3.17).

## 7.5 Reference integrity

- **Legal targets** are values owned by evaluation roots — `output`
  and `input` values and their sub-values (D22). A module `const` is
  not a legal target; a navigation of one in a `ref` position is a
  compile error. Cross-root references (a value in one output
  referencing another output's sub-value, or an input's) are legal.
- A reference whose target does not exist — an out-of-range index, a
  missing key, a path into an absent optional member, or (from input
  data) a path that does not resolve — is a **dangling-reference
  validation error** at the reference's path.
- A reference whose target exists but fails its type is not dangling;
  the target's own diagnostics stand, and the reference is tainted by
  them under root-cause rules (§6.6) — no second report.
- Reference **cycles are permitted** and terminate: navigation is
  demand-driven, and constraint evaluation visits each (rule,
  instance) pair once ([09. Semantics](09_semantics.md)).

## 7.6 Reverse queries: `$referrers`

**The premise comes first**: in Decl, a reference is always owned by a
record instance — it sits in some member, directly or inside the
arrays/maps under that member (§7.5, and the boundary bullet below). A
reverse lookup therefore has a natural, fully determined shape: name
*whose* reference and *which member carries it*, and ask who points
here through that edge.

```decl
$referrers(T, "m")      // "the T instances whose member m references me"
```

The two arguments are the two halves of a relationship edge — read them
the way an association end or an ORM reverse relation is read:

- `T` — **who** refers: the referrer's declared type. It is also what
  types the result (`ref<T>[]`).
- `"m"` — **through what**: the member that carries the reference; the
  edge's name.

```decl
type Link = {
    source: ref<Service>
    target: ref<Service>
}

type Service = {
    name: ServiceName

    const inbound  = $referrers(Link, "target")   // Links pointing at me via target
    const outbound = $referrers(Link, "source")   // …and via source

    assert not_isolated: std.array.count(inbound) > 0
        else warn `service ${name} has no inbound links`
}
```

- `$referrers(T, "m")` — every value in the evaluation universe that
  (a) occupies a position whose **declared type** is `⊑ T` — declared
  positions, not structural coincidence: a value that merely shares
  `T`'s shape is not a candidate — and (b) whose member `m` **contains
  a reference to `$this`** (place equality, §7.4).
- **"Contains" traverses collections, not records.** The reference
  need not be `m`'s direct value: `m: ref<S>`, an array element
  (`m: ref<S>[]`), a map value (`m: { [string]: ref<S> }`), and any
  nesting of arrays and maps under `m` all count — the edge is still
  "through `m`". It does **not** descend into nested records: a
  reference inside a record under `m` is owned by that record, which
  is then the referrer to query with its own type. An absent optional
  `m` refers to nothing.

  ```decl
  type Hub = { spokes: ref<Service>[] }

  type Service = {
      const hubs = $referrers(Hub, "spokes")   // Hubs listing me among spokes
  }
  ```

- **Static checks** — both fall out of `T`'s declaration alone: `T`
  must be a record type, and `"m"` a string **literal** naming a
  member of `T` whose type contains at least one `ref` position
  compatible with the enclosing type; anything else is a compile
  error (a typo in the edge name cannot silently return an empty
  answer). The same property lets implementations maintain a reverse
  index per `(T, m)` edge.
- **Combining edges is explicit**: a union of edges is a spread —
  `[...inbound, ...outbound]` — and ad-hoc conditions on the referrers
  are ordinary filters over the result
  (`[l for l in inbound if l.weight > 2]`).
- **Referrers are record instances — by premise.** A reference that no
  record owns — a `ref`-typed root (`output primary: ref<Service>`),
  or a bare collection of references (`output watchlist:
  ref<Service>[]`) — is *not* found by `$referrers`: there is no owner
  value to return. Such sites are still queryable, because they are
  nameable — `$this in watchlist` — and a model that wants them
  reverse-discoverable should wrap the reference in a record (an owner
  also gives it a place for constraints and metadata):
  `type Watch = { service: ref<Service>, since: string }`.
- The result is `ref<T>[]` — **distinct** referrers (one entry per
  referring value, however many of its references point at `$this`) in
  **canonical path order** (§7.2), defined identically for
  language-declared and input-bound data (V6).
- The **evaluation universe** is fixed and explicit: all evaluation
  roots of the module set being evaluated — every `output` and every
  bound `input` — regardless of which root the tool ultimately
  serializes, so the answer never depends on what a tool happens to
  demand ([09. Semantics](09_semantics.md) §9.2).
- A candidate whose member `m` is invalid is excluded silently under
  root-cause rules (§6.6); filters over the result follow the ordinary
  taint rules.
- **`$referrers` and laziness**: the query is answerable only after
  every instance of the universe is materialized — a demand-driven
  implementation must defer `$referrers`-dependent members (and
  everything that transitively reads them) until materialization
  completes, or the same document could yield different answers
  depending on demand order, violating observational equivalence
  (§9.4). The §0.6 spike hit this as a live bug: a width forced
  mid-materialization memoized an empty referrer set.
- `$referrers` is the **only** universe query. Ad-hoc relationship
  constraints — uniqueness, degree rules, joins — belong to the
  container type that owns the collections involved, filtering
  **named** collections with ordinary comprehensions and place
  equality (§4.5); there is no ambient "all values of T" enumeration
  form.

## Open questions

None.

---

## Previous / Next

- Previous: [06. Constraints and Diagnostics](06_constraints.md)
- Next: [08. Modules and Packages](08_modules.md)
- Index: [Documentation home](../README.md)
