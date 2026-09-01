# 08. Modules and Packages

A file is a module; a package is a versioned set of modules with a
manifest and a lock. This chapter defines visibility, imports, name
provenance, package resolution, and multi-module evaluation. The
guiding principle is P6: the origin of every name is always answerable,
and resolution is deterministic under a lock (D27, D28).

## 8.1 Files are modules

- One `.decl` file is one module. A module is identified by its path:
  relative to the importing file within a package, or
  package-qualified across packages (§8.6).
- The **module import graph must be acyclic**: an import cycle is a
  compile error naming the cycle. This matches every other graph the
  language constrains (function references §5.3, member dependencies
  §9) and keeps name resolution a single forward pass.
- A module's declarations share one name space with its imports and
  the predeclared names (§5.1, §2.4); there is no shadowing anywhere.

## 8.2 Exports

```decl
export type Service = { … }
export const max_services = 64
export func clog2(n: int): int = …
export output demo: Service = { … }
export input topology: Topology
export diagnostic width_mismatch(src: int, dst: int) { … }
export dimension Time
export unit s: Time
```

- `export` prefixes any module-level declaration; only exported
  declarations are visible outside the module. Every declaration kind
  is exportable — including `unit` (imported units enable unit
  literals, §2.7) and `input` (importers may navigate its bound
  value, §7.5).
- Exported `output`s are the tool-facing emission units (§5.5); an
  exported `type` is usable, extendable, and intersectable by
  importers.
- Export is not transitive: importing a module does not re-expose its
  imports.

## 8.3 Imports

```decl
import { Service, Port } from "./schemas.decl"
import { Config as NetConfig } from "./net.decl"
import * as topo from "./topology.decl"
```

- **Named imports** bind listed exported names; `as` renames at the
  import site. Because nothing may shadow and a module has one name
  space, renaming is the collision tool: importing two `Config`s
  requires renaming at least one.
- **Namespace imports** (`* as topo`) bind a single name; members are
  reached as `topo.Service`. The namespace name is not a value — it
  cannot be bound, passed, or serialized; only `ns.member` access is
  legal.
- **No bare wildcard injection**: `import * from "…"` does not exist
  (D27) — every name in scope is either declared in this file,
  explicitly listed in an import, or reached through a namespace, so
  provenance is answerable by looking at the file alone.
- Importing a name that is not exported, importing from a nonexistent
  module, or a collision with any existing binding is a compile error.
- **Declaring-module scope**: an imported type's member expressions —
  derived members, defaults, asserts, `$referrers` targets — resolve
  names in the scope of the module that **declared** the type, not the
  importer's. Instantiating an imported type never changes what its
  own expressions mean; checking and evaluation both honor this.

## 8.4 Re-export

```decl
export { Service, Port as PublicPort } from "./schemas.decl"
```

- Named re-export republishes selected exports of another module,
  optionally renamed. It does **not** bind the names locally — a
  module that also uses `Service` imports it separately (re-export is
  an interface statement, not a scope statement).
- `export * from "…"` does not exist (D27): a module's public surface
  is always an explicit list, so a dependency adding an export can
  never silently widen this module's interface.

## 8.5 `std` is ambient

The standard library is not a module or a package: `std` is available
in every module with no import, its dotted access is namespace member
access (never module resolution), `import … from "std"` is an error,
and `std` is reserved as a package name (D16, §13). Its version is the
language version.

## 8.6 Packages and `decl.toml`

```toml
name = "netlib"
version = "1.4.0"
description = "service topology schemas"     # metadata: inert

[dependencies]
corelib = "2.0.1"
```

- A package is a directory tree of modules with a `decl.toml` manifest
  at its root. **Semantic fields**: `name`, `version`,
  `[dependencies]`. **Metadata fields** (`description`, `license`,
  `authors`, `repository`, `keywords`) are permitted and never affect
  resolution or evaluation. Any other field is an error — fail-closed
  (D28).
- **Package names** match `/[a-z][a-z0-9_-]*/` — and therefore never
  begin with `.`, which is what keeps import specifiers unambiguous:
  a specifier starting with `./` or `../` is a relative module path
  within the current package; anything else is
  `<package>/<module-path>`:

  ```decl
  import { Service } from "./schemas.decl"          // this package
  import { Base } from "corelib/types/base.decl"    // dependency
  ```

- Versions are exact semantic-version triples (`"2.0.1"`). **Exact
  pins only**: ranges, carets, tildes, and wildcards are manifest
  errors. Two dependencies requiring different versions of the same
  package is a resolution error reported against both requirers —
  there is no solver choosing among candidates, and consequently
  nothing to be nondeterministic about.
- Importing from a package not listed in `[dependencies]` is an
  error.

## 8.7 The lock file and reproducibility

- Resolution writes `decl.lock` at the root package: for every
  package in the closed dependency set, its name, version, and the
  **content hash** — SHA-256 over the package's module files in
  canonical path order.
- The lock is **fail-closed**: when a lock is present, a missing
  entry, a version differing from the manifest, or a hash mismatch
  stops resolution with an error — never a silent re-resolve.
- Under the same lock, module resolution is bit-for-bit deterministic
  (P2): the same import specifier resolves to the same module content
  on every machine. Evaluation results can therefore be stamped with
  the lock's hashes as the provenance of the specification they used.

## 8.8 Multi-module evaluation

- The evaluation universe (§7.6, §9) is the set of evaluation roots —
  `output`s and bound `input`s — of the **module set** being
  evaluated: the entry modules a tool is invoked on plus everything
  they import, transitively.
- **Root names must be unique across the universe.** Canonical paths
  (§7.2) begin with a bare root name, so two in-universe roots named
  `net` would make paths ambiguous; the tool reports the collision as
  an evaluation-setup error naming both declaring modules. (Renaming
  an output, or evaluating the modules separately, resolves it; a
  future revision may introduce module-qualified roots if practice
  demands more.)
- Cross-module navigation and references follow the ordinary rules:
  an imported output's value is navigated as `net.services[0]`, and
  references into it serialize as canonical paths rooted at `net`
  (§7.4, §7.5).

## Open questions

None.

---

## Previous / Next

- Previous: [07. Relationships](07_relationships.md)
- Next: [09. Evaluation Semantics](09_semantics.md)
- Index: [Documentation home](../README.md)
