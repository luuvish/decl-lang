# Modules and Packages

A tutorial on splitting a description across files and across
projects. It walks through two things that exist in the repository:
the service-graph example, whose three modules import one another, and
the package test case, whose manifest, dependency, and lock file the
tool chain resolves. Nothing on this page needs to be typed in; the
paths are given, so you can run every command from a checkout
([08. Modules](../specification/08_modules.md)).

The code blocks on this page are quoted from those files (each is
titled with its path), not assembled into a module of their own as the
other tutorials' blocks are.

## 1. Three modules, one universe

`examples/svcgraph/` describes deployments of a small service graph.
The schemas live in one module and export what the others need:

```decl title="examples/svcgraph/schemas.decl"
// Service-graph schemas: the API/config benchmark case grown to
// production level (Phase 5) — resource quantities, health checks,
// reverse-reference fan-in, and layered environments.

export type Name = /[a-z][a-z0-9_-]*/
export type Protocol = "http" | "grpc" | "tcp"

export type Resources = {
    cpu_millis?: 100..64000 = 500
    memory?: quantity<DataSize> = 256MiB
    assert memory_floor: memory >= 16MiB
        else error `memory below the 16MiB scheduling floor`
}

export type HealthCheck = {
    path?: /\/[a-z0-9\/_-]*/ = "/healthz"
    interval?: quantity<Time> = 10s
    timeout?: quantity<Time> = 2s
    assert timely: timeout < interval
        else error `health timeout must be shorter than its interval`
}

export type Service = {
    name: Name
    protocol: Protocol
    port: 1024..65535
    replicas?: 1..64 = 1
    public?: bool = false
    resources?: Resources = { }
    health?: HealthCheck = { }

    endpoint = `${name}:${port}`
    inbound = $referrers(Link, "target")
    inbound_count = std.array.count(inbound)

    assert grpc_ports: protocol != "grpc" || port >= 9000
        else warn `grpc service ${name} outside the 9000+ convention`
}

export type Link = {
    source: ref<Service>
    target: ref<Service>
    weight?: 1..100 = 1
}

export type Topology = {
    services: Service[1..128]
    links?: Link[] = []

    service_count = std.array.count(services)
    total_replicas = std.array.fold(services, 0, (a, s) => a + s.replicas)

    assert unique_names: std.array.all_distinct([s.name for s in services])
        else error `service names must be unique`
    assert no_self_links: std.array.all(links, (l) =>
        std.ref.path(l.source) != std.ref.path(l.target))
        else error `a service must not link to itself`
}
```

The [example's page](../../examples/svcgraph) shows the evaluated
output of both deployments. A second module imports the topology and
wraps it in an environment:

```decl title="examples/svcgraph/deployment.decl"
import { Topology, Service } from "./schemas.decl"

export type EnvName = "dev" | "staging" | "prod"

export type Deployment = {
    env: EnvName
    topology: Topology

    public_count = std.array.count(std.array.filter(topology.services, (s) => s.public))
    capacity_millis = std.array.fold(topology.services, 0, (a, s) =>
        a + s.resources.cpu_millis * s.replicas)

    assert prod_is_redundant: (
        env != "prod" || std.array.all(topology.services, (s) =>
            !s.public || s.replicas >= 2))
        else error `every public prod service needs at least 2 replicas`
    assert has_a_front_door: public_count >= 1
        else warn `no public service in this deployment`
}
```

- `import { Topology, Service } from "./schemas.decl"` brings two
  names into this module's single name space; a name is imported by
  its exported name, or renamed with `as`
  ([08. Modules](../specification/08_modules.md) §8.2).
- Only exported declarations can be imported. The `Link` type is not
  imported here, and does not need to be: `Topology` carries it.
- Units and dimensions travel the same way, in their own name spaces.

The entry module imports both and produces the outputs:

```decl title="examples/svcgraph/main.decl"
import { Topology } from "./schemas.decl"
import { Deployment } from "./deployment.decl"

func front_replicas(prod: bool): int = if prod then 3 else 1
func back_replicas(prod: bool): int = if prod then 2 else 1

func mk_topology(prod: bool): Topology = {
    services: [
        { name: "gateway", protocol: "http", port: 8080, public: true, replicas: front_replicas(prod) }
        { name: "auth", protocol: "grpc", port: 9001, resources: { cpu_millis: 1000 }, replicas: back_replicas(prod) }
        { name: "billing", protocol: "grpc", port: 9002, resources: { memory: 512MiB }, replicas: back_replicas(prod) }
        { name: "ledger", protocol: "tcp", port: 5432, resources: { cpu_millis: 2000, memory: 1GiB }, replicas: back_replicas(prod) }
    ]
    links: [
        { source: services[0], target: services[1] }
        { source: services[0], target: services[2], weight: 3 }
        { source: services[2], target: services[3], weight: 5 }
        { source: services[1], target: services[3] }
    ]
}

export output dev: Deployment = { env: "dev", topology: mk_topology(false) }
export output prod: Deployment = { env: "prod", topology: mk_topology(true) }
```

Why a function rather than a shared constant: references bind to
places, and a module constant is not a root, so each output builds
its own topology and its links resolve inside it
([07. Relationships](../specification/07_relationships.md) §7.5).

```bash
decl evaluate examples/svcgraph/main.decl --output dev
```

The command opens the entry's **universe** — the module and everything
it imports, transitively — checks all of it, and evaluates. The first
lines of the document it prints:

```json
{ "env": "dev",
  "topology": { "services": [
    { "name": "gateway", "protocol": "http", "port": 8080, "public": true, "replicas": 1,
      "resources": { "cpu_millis": 500, "memory": { "value": 2147483648.0, "unit": "bit" } },
      "endpoint": "gateway:8080", "inbound": [], … },
    …
```

A diagnostic in an imported module names that module by its absolute
path, and the entry by the path you gave on the command line
([01. Command line](../tooling/01_cli.md) §2). The language server and
the REPL open the same universe (the editor extensions follow every
import, and `decl repl examples/svcgraph/main.decl` loads all three
files).

## 2. A package

A **package** is a directory with a manifest, `decl.toml`, and the
dependencies it names live under `decl_modules/`, one directory per
package. `tests/packages/app/` is the smallest complete one:

```text
tests/packages/app/
├── decl.toml
├── decl.lock                 (written by the resolver — see below)
├── main.decl
└── decl_modules/
    └── corelib/
        ├── decl.toml
        └── types/base.decl
```

```toml title="tests/packages/app/decl.toml"
name = "app"
version = "0.1.0"
description = "package test root"

[dependencies]
corelib = "1.0.0"
```

A dependency is pinned to an exact version: no ranges, no resolution
strategy to reason about, so that two people resolving the same
manifest get the same universe ([08. Modules](../specification/08_modules.md)
§8.6; the design decision is D28). An unknown field in the manifest or
a version that is not exact is an error, not a warning.

Importing from a package names the package, then the path inside it:

```decl title="tests/packages/app/main.decl"
import { Base, WIDTH } from "corelib/types/base.decl"
export output box: Base = { label: "x" }
export output w: int = WIDTH * 2
```

```bash
decl check tests/packages/app/main.decl
decl evaluate tests/packages/app/main.decl
```

```text
ok: 1 entry file(s) check clean
{"box":{"label":"x","width":8},"w":16}
```

## 3. The lock

When a manifest governs the entry's directory, the resolver records
what it used in `decl.lock`: one line per dependency, with the version
and a content hash of the dependency's files.

```text title="decl.lock"
corelib 1.0.0 df4deaada740da5706ba84a3dcd06bdcc4936920e0424726e1c589e11f50235c
```

From then on the lock is checked before anything runs, and it fails
closed ([08. Modules](../specification/08_modules.md) §8.7): a
dependency missing from the lock is **E3015**, a version that differs
from the manifest is **E3016**, and a file changed under
`decl_modules/` — a hash that no longer matches — is **E3017**. None of
these re-resolves silently. The package corpus in the repository
exercises all three by tampering with a copy of `app/` and expecting
exactly those codes ([tests/packages](../../tests/packages/README.md)).

The hash is over the dependency's files in a fixed order, so it is the
same on every platform and from every implementation; the three
implementations compute it in their own languages and the parity
harness diffs the lock reports.

## Where to go next

- The exact rules for exports, provenance, and re-export:
  [08. Modules](../specification/08_modules.md) §8.1–8.5.
- The manifest and lock formats, field by field:
  [08. Modules](../specification/08_modules.md) §8.6–8.7.
- The command line's universe and package reports:
  [01. Command line](../tooling/01_cli.md).

---

- Index: [Documentation home](../README.md)
