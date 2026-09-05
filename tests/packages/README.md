# Package cases

Packages, manifests, and the lock file (specification §8.6–8.7): `app/`
is a package with one exact-pinned dependency (`decl_modules/corelib`);
`bad_manifest/`, `undeclared/`, and `conflict/` are packages whose
manifests or resolutions fail. `cases.json` fixes what the command line
reports and how the lock behaves:

```json
{ "errors": [ { "entry": "tests/packages/bad_manifest/main.decl", "codes": ["E3011", "E3012"] }, … ],
  "lock": { "package": "tests/packages/app", "entry": "main.decl", "lock": "tests/packages/lock/decl.lock",
            "drift": [ { "name": "content drift is E3017", "append": { "decl_modules/corelib/types/base.decl": "// drift\n" }, "codes": ["E3017"] },
                       { "name": "version drift is E3016", "lock_replace": ["1.0.0", "1.0.1"], "codes": ["E3016"] },
                       { "name": "a missing entry is E3015", "lock_text": "", "codes": ["E3015"] } ] } }
```

- `errors`: the codes `decl check <entry>` reports, in order;
- `lock`: in a fresh copy of the package, the lock written by the
  implementation's API must be `lock/decl.lock` byte for byte and
  verify clean; then each `drift` — a file appended to, the lock's
  version replaced, the lock emptied — must make `decl check` report
  exactly the codes named (fail-closed: never a silent re-resolve).

The evaluation of `app/` is a golden (`tests/golden/manifest.json`);
the harness runs `check` and `evaluate` over every entry three-way.
The lock scenario runs in each suite (`decl-ts/tests/packages_test.ts`,
`decl-rs/tests/packages_test.rs`, `decl-py/tests/packages_test.py`), since
the lock is written by the library, not by a command.
