# Validating Documents

A tutorial on the third capability: binding a JSON document that
someone else wrote to a type you declared, and reading what comes
back. It builds one schema — a machine inventory — and runs two
documents through it, one that passes and one that does not. The
```decl blocks on this page form one module, `inventory.decl`; the
documents and the command lines are given in full, so you can follow
along at a shell.

What the page teaches: a document is validated by the same pipeline
that generates values ([Decl by example](01_overview_by_example.md)),
a failure is a diagnostic with a path and a cause, and only root causes
are reported ([06. Constraints](../specification/06_constraints.md)
§6.6).

## 1. The schema

Names first. A hostname is a pattern, and the pattern carries its own
message; a role is a union of literals.

```decl
export type Hostname = /[a-z][a-z0-9-]*(\.[a-z0-9-]+)*/
    else error `hostnames are lowercase labels separated by dots`

export type Role = "web" | "db" | "cache"
```

A machine has two members the document must supply, three it may, two
that are computed, and two rules. Memory and disks are physical
quantities of the `DataSize` dimension, so a document may write them
in whatever unit it likes ([03. Types](../specification/03_types.md)
§3.16).

```decl
export type Machine = {
    host: Hostname
    role: Role
    cores?: 1..128 = 2
    memory?: quantity<DataSize> = 4GiB
    disks?: quantity<DataSize>[] = []

    storage = std.array.fold(disks, 0B, (a, d) => a + d)
    memory_per_core = memory / cores

    assert db_memory: role != "db" || memory >= 16GiB
        else error `a db host needs at least 16GiB of memory`
    assert cache_disks: role != "cache" || std.array.count(disks) == 0
        else warn `cache host ${host} carries disks it will not use`
}
```

- `storage` folds the disks with a quantity as the seed, `0B`; a sum
  of quantities is a quantity ([13. Standard library](../specification/13_stdlib.md) §13.2).
- `memory_per_core` divides a quantity by an integer: the dimension is
  unchanged.
- `db_memory` is an error: a failing value is *invalid*. `cache_disks`
  is a warning: the value is kept and annotated
  ([06. Constraints](../specification/06_constraints.md) §6.6).

The inventory owns the machines and points at one of them. `primary`
is a reference, not a copy: the document names a place in itself, and
the rule reads through it ([07. Relationships](../specification/07_relationships.md)).

```decl
export type Inventory = {
    machines: Machine[1..256]
    primary: ref<Machine>

    web_count = std.array.count(std.array.filter(machines, (m) => m.role == "web"))
    assert unique_hosts: std.array.all_distinct([m.host for m in machines])
        else error `hostnames must be unique`
    assert primary_is_db: primary.role == "db"
        else error `the primary must be a db host`
}
```

A sample value proves the schema can be satisfied — the module's own
output — and the input root is the door documents come in through:

```decl
export output sample: Inventory = {
    machines: [
        { host: "web-1", role: "web" }
        { host: "db-1", role: "db", cores: 8, memory: 32GiB, disks: [1TiB, 1TiB] }
    ]
    primary: machines[1]
}

export input fleet: Inventory
```

`decl evaluate inventory.decl` prints `sample`, completed: defaults
filled, quantities normalized to the base unit of their dimension
(`bit`), the reference as a document-relative path.

## 2. A document that passes

`fleet.json`, from a provisioning system that has never heard of Decl:

```json
{
  "machines": [
    { "host": "edge-1.sea", "role": "web", "cores": 4 },
    { "host": "db-primary", "role": "db", "cores": 16,
      "memory": { "value": 64, "unit": "GiB" },
      "disks": [{ "value": 2, "unit": "TiB" }] },
    { "host": "cache-1", "role": "cache",
      "memory": { "value": 8, "unit": "GiB" } }
  ],
  "primary": "$.machines[1]"
}
```

Quantities arrive in the interchange form, `{ "value", "unit" }`, in
any unit of the right dimension; the reference arrives as the path
`$.machines[1]`, relative to the document's own root
([10. Interchange](../specification/10_interchange.md)).

```bash
decl validate inventory.decl --input fleet=fleet.json
```

Prints nothing and exits 0: no diagnostics. To see the document as the
schema completed it, ask `evaluate` for the root:

```bash
decl evaluate inventory.decl --input fleet=fleet.json --output fleet
```

```json
{
  "machines": [
    { "host": "edge-1.sea", "role": "web", "cores": 4,
      "memory": { "value": 34359738368.0, "unit": "bit" }, "disks": [],
      "storage": { "value": 0.0, "unit": "bit" },
      "memory_per_core": { "value": 8589934592.0, "unit": "bit" } },
    { "host": "db-primary", "role": "db", "cores": 16,
      "memory": { "value": 549755813888.0, "unit": "bit" },
      "disks": [{ "value": 17592186044416.0, "unit": "bit" }],
      "storage": { "value": 17592186044416.0, "unit": "bit" },
      "memory_per_core": { "value": 34359738368.0, "unit": "bit" } },
    { "host": "cache-1", "role": "cache",
      "memory": { "value": 68719476736.0, "unit": "bit" }, "cores": 2, "disks": [],
      "storage": { "value": 0.0, "unit": "bit" },
      "memory_per_core": { "value": 34359738368.0, "unit": "bit" } }
  ],
  "primary": "$.machines[1]",
  "web_count": 1
}
```

Everything the schema knows has been added: the defaults (`cores: 2`,
`disks: []`), the derived members (`storage`, `memory_per_core`,
`web_count`), the normalized quantities. Bind this output back to
`fleet` and it validates and serializes identically — the round trip
is a property of the language, not of this example
([10. Interchange](../specification/10_interchange.md) §10.5).

## 3. A document that does not

`fleet_bad.json` has four things wrong with it:

```json
{
  "machines": [
    { "host": "edge-1.sea", "role": "web", "cores": 4 },
    { "host": "DB 01", "role": "db", "cores": 16,
      "memory": { "value": 8, "unit": "GiB" } },
    { "host": "cache-1", "role": "cache",
      "disks": [{ "value": 500, "unit": "GB" }] }
  ],
  "primary": "$.machines[0]"
}
```

```bash
decl validate inventory.decl --input fleet=fleet_bad.json
```

```text
inventory.decl: error [E6001] Inventory.primary_is_db at fleet: the primary must be a db host
inventory.decl: error [E6001] Machine.db_memory at fleet.machines[1]: a db host needs at least 16GiB of memory
inventory.decl: error [E4001] at fleet.machines[1].host: hostnames are lowercase labels separated by dots
inventory.decl: warn [W6001] Machine.cache_disks at fleet.machines[2]: cache host cache-1 carries disks it will not use
```

Exit status 1. Read the four lines against the document:

- **`primary_is_db` at `fleet`**: the reference resolves — `$.machines[0]`
  exists — and the rule reads through it and finds a web host. The
  path is the inventory itself, because that is where the assert is
  declared.
- **`db_memory` at `fleet.machines[1]`**: 8 GiB is below the floor.
  The message is the one the assert carries; the id names the type
  and the assert, so a tool can find the rule.
- **`E4001` at `fleet.machines[1].host`**: `"DB 01"` fails the
  pattern, and the report is the pattern's own message, not a generic
  mismatch. Note that this machine has *two* independent defects and
  both are reported: `memory` does not depend on `host`, so the
  failure of one does not silence the other
  ([06. Constraints](../specification/06_constraints.md) §6.6).
- **`cache_disks` at `fleet.machines[2]`**: a warning. The value
  stays; `decl evaluate --output fleet` would still print this
  machine with its 500 GB disk and its `storage`.

The order of the lines is not the order of discovery: diagnostics are
sorted by path, the root before its members, then by id, so the same
document produces the same report from every implementation
([12. Errors](../specification/12_errors.md) §12.3). Add `--json` to
get the same four records as a JSON array, one object each with
`file`, `code`, `id`, `severity`, `message`, and `path`
([01. Command line](../tooling/01_cli.md) §4).

## 4. What was not reported

Change `"DB 01"` to a valid name, keep the memory at 8 GiB, and add a
second machine called `cache-1`. The report gains `unique_hosts` at
`fleet` and loses `E4001`; `db_memory` stays. Now remove the
`"memory"` member from the db machine altogether: the default `4GiB`
applies, and `db_memory` fires on the default, at the same path — a
default is a value like any other, and the rules see it.

What never appears is a consequence of a failure: if `host` were
invalid and some derived member read it, that member would be invalid
silently, and a rule that read the derived member would be skipped
silently. One defect, one line. That is the property that makes the
report a to-do list rather than a log.

## Where to go next

- The reverse direction, generating documents from a description:
  [Generating configuration](03_generating_configuration.md).
- The rules for binding, absence, and null:
  [10. Interchange](../specification/10_interchange.md) §10.2.
- The complete list of codes: [12. Errors](../specification/12_errors.md) §12.4.

---

- Index: [Documentation home](../README.md)
