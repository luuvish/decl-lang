// Packages, decl.toml, and decl.lock (§8.6–8.7): exact-pinned
// dependencies, fail-closed manifests, content-hashed reproducibility.
// Implementation conventions (documented in decl-ts/README.md): dependency
// packages live under <root>/decl_modules/<name>/ in a flat layout, and
// the lock file is line-based `name version sha256` in name order.
import { host, join, dirname, resolvePath as absPath, relative, sha256Hex } from './host.ts';
import type { Diag } from './semantics.ts';
import type { PackageResolver } from './module.ts';

const NAME_RE = /^[a-z][a-z0-9_-]*$/;
const VERSION_RE = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const SEMANTIC = ['name', 'version'];
const METADATA = ['description', 'license', 'authors', 'repository', 'keywords'];

export type Manifest = { name: string; version: string; dependencies: Map<string, string> };

export function parseManifest(
  path: string,
  report: (c: string, m: string) => void,
): Manifest | null {
  const src = host.readFile(path);
  if (src === null) {
    report('E3004', `manifest not found: ${path}`);
    return null;
  }
  const fields = new Map<string, string>();
  const deps = new Map<string, string>();
  let section: string | null = null;
  let ok = true;
  for (const line0 of src.split('\n')) {
    const line = line0.replace(/#.*$/, '').trim();
    if (!line) continue;
    const sec = /^\[([^\]]+)\]$/.exec(line);
    if (sec) {
      section = sec[1];
      if (section !== 'dependencies') {
        report('E3011', `manifest ${path}: unknown section [${section}] (fail-closed, D28)`);
        ok = false;
      }
      continue;
    }
    const kv = /^([A-Za-z0-9_-]+)\s*=\s*(.+)$/.exec(line);
    if (!kv) {
      report('E3011', `manifest ${path}: unparseable line "${line}"`);
      ok = false;
      continue;
    }
    const key = kv[1];
    const raw = kv[2].trim();
    const value = raw.startsWith('"') ? JSON.parse(raw.replace(/\\/g, '\\\\')) : raw;
    if (section === 'dependencies') {
      if (!NAME_RE.test(key)) {
        report('E3013', `manifest ${path}: invalid package name ${key}`);
        ok = false;
        continue;
      }
      if (typeof value !== 'string' || !VERSION_RE.test(value)) {
        report(
          'E3012',
          `manifest ${path}: dependency ${key} = ${raw} is not an exact semantic-version pin`,
        );
        ok = false;
        continue;
      }
      deps.set(key, value);
    } else if (section === null) {
      if (!SEMANTIC.includes(key) && !METADATA.includes(key)) {
        report('E3011', `manifest ${path}: unknown field ${key} (fail-closed, D28)`);
        ok = false;
        continue;
      }
      if (typeof value === 'string') fields.set(key, value);
    }
  }
  const name = fields.get('name') ?? '';
  const version = fields.get('version') ?? '';
  if (!NAME_RE.test(name)) {
    report('E3013', `manifest ${path}: invalid package name ${JSON.stringify(name)}`);
    ok = false;
  }
  if (!VERSION_RE.test(version)) {
    report('E3012', `manifest ${path}: version ${JSON.stringify(version)} is not an exact triple`);
    ok = false;
  }
  return ok ? { name, version, dependencies: deps } : null;
}

// content hash: SHA-256 over the package's module files in canonical
// path order (§8.7)
export function packageHash(dir: string): string {
  const files: string[] = [];
  const walk = (d: string) => {
    for (const e of host.readDir(d).sort()) {
      const p = join(d, e);
      if (e === 'decl_modules') continue;
      if (host.isDir(p)) walk(p);
      else if (p.endsWith('.decl')) files.push(p);
    }
  };
  walk(dir);
  const chunks: string[] = [];
  for (const f of files.sort((a, b) => (a < b ? -1 : 1)))
    chunks.push(relative(dir, f), '\0', host.readFile(f) ?? '', '\0');
  return sha256Hex(chunks);
}

export type ResolvedPackage = { name: string; version: string; dir: string; hash: string };

export type PackageUniverse = {
  rootDir: string;
  manifest: Manifest;
  packages: Map<string, ResolvedPackage>; // closed dependency set (root excluded)
  resolver: PackageResolver;
  diags: Diag[];
};

// find the enclosing package root (the nearest ancestor with decl.toml)
export function findPackageRoot(fromFile: string): string | null {
  let dir = dirname(absPath(fromFile));
  for (;;) {
    if (host.exists(join(dir, 'decl.toml'))) return dir;
    const up = dirname(dir);
    if (up === dir) return null;
    dir = up;
  }
}

export function openPackageUniverse(entryFile: string): PackageUniverse | null {
  const diags: Diag[] = [];
  const report = (code: string, message: string) =>
    diags.push({ severity: 'error', code, message, path: '' });
  const rootDir = findPackageRoot(entryFile);
  if (rootDir === null) return null; // not in a package: relative imports only
  const manifest = parseManifest(join(rootDir, 'decl.toml'), report);
  if (!manifest)
    return {
      rootDir,
      manifest: { name: '?', version: '0.0.0', dependencies: new Map() },
      packages: new Map(),
      resolver: () => ({ code: 'E3011', message: 'unusable manifest' }),
      diags,
    };

  // resolve the closed dependency set (flat decl_modules layout);
  // conflicting versions for one package are E3014 against both requirers
  const packages = new Map<string, ResolvedPackage>();
  const requiredBy = new Map<string, { version: string; by: string }>();
  const visit = (m: Manifest) => {
    for (const [dep, ver] of m.dependencies) {
      const prev = requiredBy.get(dep);
      if (prev && prev.version !== ver) {
        report(
          'E3014',
          `package ${dep} required at ${prev.version} (by ${prev.by}) and ${ver} (by ${m.name})`,
        );
        continue;
      }
      requiredBy.set(dep, { version: ver, by: m.name });
      if (packages.has(dep)) continue;
      const dir = join(rootDir, 'decl_modules', dep);
      const dm = parseManifest(join(dir, 'decl.toml'), report);
      if (!dm) continue;
      if (dm.name !== dep)
        report('E3013', `package at ${dir} names itself ${dm.name}, expected ${dep}`);
      if (dm.version !== ver)
        report(
          'E3016',
          `package ${dep}: manifest version ${dm.version} differs from required pin ${ver}`,
        );
      packages.set(dep, { name: dep, version: dm.version, dir, hash: packageHash(dir) });
      visit(dm);
    }
  };
  visit(manifest);

  const resolver: PackageResolver = (spec, fromDir) => {
    const slash = spec.indexOf('/');
    const pkg = slash < 0 ? spec : spec.slice(0, slash);
    const rest = slash < 0 ? '' : spec.slice(slash + 1);
    // which package does the importing file belong to?
    const fromPkgDir =
      [...packages.values()].find((p) => absPath(fromDir).startsWith(p.dir))?.dir ?? rootDir;
    const fromManifest =
      fromPkgDir === rootDir ? manifest : parseManifest(join(fromPkgDir, 'decl.toml'), () => {})!;
    if (!fromManifest.dependencies.has(pkg))
      return {
        code: 'E3010',
        message: `package ${pkg} not declared in [dependencies] of ${fromManifest.name}`,
      };
    const p = packages.get(pkg);
    if (!p) return { code: 'E3004', message: `package ${pkg} could not be resolved` };
    return join(p.dir, rest);
  };
  return { rootDir, manifest, packages, resolver, diags };
}

// ---------------- decl.lock (§8.7) ----------------
export function lockText(u: PackageUniverse): string {
  const lines = [...u.packages.values()]
    .sort((a, b) => (a.name < b.name ? -1 : 1))
    .map((p) => `${p.name} ${p.version} ${p.hash}`);
  return lines.join('\n') + (lines.length ? '\n' : '');
}
export function writeLock(u: PackageUniverse): string {
  const path = join(u.rootDir, 'decl.lock');
  host.writeFile(path, lockText(u));
  return path;
}
// fail-closed verification: missing entry, version drift, or hash
// mismatch stops resolution — never a silent re-resolve
export function verifyLock(u: PackageUniverse): Diag[] {
  const path = join(u.rootDir, 'decl.lock');
  const text = host.readFile(path);
  if (text === null) return [];
  const out: Diag[] = [];
  const report = (code: string, message: string) =>
    out.push({ severity: 'error', code, message, path: '' });
  const locked = new Map<string, { version: string; hash: string }>();
  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    const [name, version, hash] = line.trim().split(/\s+/);
    locked.set(name, { version, hash });
  }
  for (const p of u.packages.values()) {
    const l = locked.get(p.name);
    if (!l) {
      report('E3015', `lock: missing entry for ${p.name}`);
      continue;
    }
    if (l.version !== p.version)
      report('E3016', `lock: ${p.name} version ${l.version} differs from manifest ${p.version}`);
    else if (l.hash !== p.hash) report('E3017', `lock: ${p.name} content-hash mismatch`);
  }
  return out;
}
