//! Packages, decl.toml, and decl.lock (§8.6–8.7) — a port of the
//! reference implementation's package.ts: exact-pinned dependencies,
//! fail-closed manifests, content-hashed reproducibility. Conventions:
//! dependency packages live under `<root>/decl_modules/<name>/` in a flat
//! layout, and the lock file is line-based `name version sha256` in name
//! order.
use crate::semantics::{json_str, read_json, Diag, Value};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SEMANTIC: [&str; 2] = ["name", "version"];
const METADATA: [&str; 5] = [
    "description",
    "license",
    "authors",
    "repository",
    "keywords",
];

fn name_re() -> Regex {
    Regex::new(r"^[a-z][a-z0-9_-]*$").unwrap()
}
fn version_re() -> Regex {
    Regex::new(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$").unwrap()
}

#[derive(Clone, Debug)]
/// a `decl.toml` (§8.6)
pub struct Manifest {
    /// the package's name
    pub name: String,
    /// its version, an exact semantic version
    pub version: String,
    /// declaration order, like the reference's Map
    pub dependencies: Vec<(String, String)>,
}
impl Manifest {
    /// Whether the manifest declares a dependency of that name.
    pub fn has_dep(&self, n: &str) -> bool {
        self.dependencies.iter().any(|(d, _)| d == n)
    }
}

/// Read a manifest, fail-closed (D28): an unknown field is E3011, a range pin
/// E3012, a missing file E3004 — each reported through `report` with the code
/// and the message. `None` when the manifest cannot be used.
pub fn parse_manifest(path: &Path, report: &mut dyn FnMut(&str, String)) -> Option<Manifest> {
    let shown = path.display().to_string();
    let Ok(src) = std::fs::read_to_string(path) else {
        report("E3004", format!("manifest not found: {shown}"));
        return None;
    };
    let (name_re, version_re) = (name_re(), version_re());
    let comment = Regex::new(r"#.*$").unwrap();
    let sec_re = Regex::new(r"^\[([^\]]+)\]$").unwrap();
    let kv_re = Regex::new(r"^([A-Za-z0-9_-]+)\s*=\s*(.+)$").unwrap();
    let mut fields: Vec<(String, String)> = vec![];
    let mut deps: Vec<(String, String)> = vec![];
    let mut section: Option<String> = None;
    let mut ok = true;
    for line0 in src.split('\n') {
        let line = comment.replace(line0, "").trim().to_string();
        if line.is_empty() {
            continue;
        }
        if let Some(sec) = sec_re.captures(&line) {
            let s = sec[1].to_string();
            if s != "dependencies" {
                report(
                    "E3011",
                    format!("manifest {shown}: unknown section [{s}] (fail-closed, D28)"),
                );
                ok = false;
            }
            section = Some(s);
            continue;
        }
        let Some(kv) = kv_re.captures(&line) else {
            report(
                "E3011",
                format!("manifest {shown}: unparseable line \"{line}\""),
            );
            ok = false;
            continue;
        };
        let key = kv[1].to_string();
        let raw = kv[2].trim().to_string();
        let value: Option<String> = if raw.starts_with('"') {
            match read_json(&raw.replace('\\', "\\\\")) {
                Ok(Value::Str(s)) => Some(s),
                Ok(_) => None,
                Err(_) => {
                    report(
                        "E3011",
                        format!("manifest {shown}: unparseable line \"{line}\""),
                    );
                    ok = false;
                    continue;
                }
            }
        } else {
            Some(raw.clone())
        };
        match section.as_deref() {
            Some("dependencies") => {
                if !name_re.is_match(&key) {
                    report(
                        "E3013",
                        format!("manifest {shown}: invalid package name {key}"),
                    );
                    ok = false;
                    continue;
                }
                match &value {
                    Some(v) if version_re.is_match(v) => deps.push((key, v.clone())),
                    _ => {
                        report("E3012", format!("manifest {shown}: dependency {key} = {raw} is not an exact semantic-version pin"));
                        ok = false;
                    }
                }
            }
            None => {
                if !SEMANTIC.contains(&key.as_str()) && !METADATA.contains(&key.as_str()) {
                    report(
                        "E3011",
                        format!("manifest {shown}: unknown field {key} (fail-closed, D28)"),
                    );
                    ok = false;
                    continue;
                }
                if let Some(v) = value {
                    fields.push((key, v));
                }
            }
            _ => {}
        }
    }
    let field = |k: &str| {
        fields
            .iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let (name, version) = (field("name"), field("version"));
    if !name_re.is_match(&name) {
        report(
            "E3013",
            format!("manifest {shown}: invalid package name {}", json_str(&name)),
        );
        ok = false;
    }
    if !version_re.is_match(&version) {
        report(
            "E3012",
            format!(
                "manifest {shown}: version {} is not an exact triple",
                json_str(&version)
            ),
        );
        ok = false;
    }
    if ok {
        Some(Manifest {
            name,
            version,
            dependencies: deps,
        })
    } else {
        None
    }
}

/// content hash: SHA-256 over the package's module files in canonical path order (§8.7)
pub fn package_hash(dir: &Path) -> String {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for e in names {
            let p = d.join(&e);
            if e == "decl_modules" {
                continue;
            }
            if p.is_dir() {
                walk(&p, out);
            } else if e.ends_with(".decl") {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    walk(dir, &mut files);
    files.sort();
    let mut h = Sha256::new();
    for f in files {
        let rel = f
            .strip_prefix(dir)
            .unwrap_or(&f)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        h.update(rel.as_bytes());
        h.update(b"\0");
        h.update(std::fs::read(&f).unwrap_or_default());
        h.update(b"\0");
    }
    // the digest as lowercase hex (sha2 0.11 no longer formats its array)
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Debug)]
/// a dependency resolved to a directory
pub struct ResolvedPackage {
    /// its name
    pub name: String,
    /// its version
    pub version: String,
    /// where it lives
    pub dir: PathBuf,
    /// the content hash of its files, the one the lock records
    pub hash: String,
}

/// maps a package specifier, from a directory, to the package's path or to a (code, message) diagnostic
pub type Resolver = Rc<dyn Fn(&str, &Path) -> Result<PathBuf, (String, String)>>;

/// the closed set of packages an entry file's manifest reaches (§8.6)
pub struct PackageUniverse {
    /// the directory holding `decl.toml`
    pub root_dir: PathBuf,
    /// that manifest
    pub manifest: Manifest,
    /// closed dependency set (root excluded), in resolution order
    pub packages: Vec<ResolvedPackage>,
    /// the resolver over the closed set
    pub resolver: Resolver,
    /// the manifest and resolution diagnostics
    pub diags: Vec<Diag>,
}
impl PackageUniverse {
    /// The resolved package of that name, when the closed set has it.
    pub fn package(&self, n: &str) -> Option<&ResolvedPackage> {
        self.packages.iter().find(|p| p.name == n)
    }
}

/// the enclosing package root (the nearest ancestor with decl.toml)
pub fn find_package_root(from_file: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(from_file).unwrap_or_else(|_| from_file.to_path_buf());
    let mut dir = abs.parent()?.to_path_buf();
    loop {
        if dir.join("decl.toml").exists() {
            return Some(dir);
        }
        let up = dir.parent()?.to_path_buf();
        if up == dir {
            return None;
        }
        dir = up;
    }
}

/// Open the package universe of an entry file: the manifest found upward from
/// it, its dependencies resolved to a closed set. `None` when no manifest
/// governs the entry.
pub fn open_package_universe(entry_file: &Path) -> Option<PackageUniverse> {
    let mut diags: Vec<Diag> = vec![];
    let root_dir = find_package_root(entry_file)?; // not in a package: relative imports only
    let mut report =
        |code: &str, message: String| diags.push(Diag::error(message, String::new(), Some(code)));
    let Some(manifest) = parse_manifest(&root_dir.join("decl.toml"), &mut report) else {
        return Some(PackageUniverse {
            root_dir,
            manifest: Manifest {
                name: "?".into(),
                version: "0.0.0".into(),
                dependencies: vec![],
            },
            packages: vec![],
            resolver: Rc::new(|_, _| Err(("E3011".into(), "unusable manifest".into()))),
            diags,
        });
    };

    // resolve the closed dependency set (flat decl_modules layout);
    // conflicting versions for one package are E3014 against both requirers
    let mut packages: Vec<ResolvedPackage> = vec![];
    let mut required_by: Vec<(String, String, String)> = vec![]; // dep, version, by
    fn visit(
        m: &Manifest,
        root_dir: &Path,
        packages: &mut Vec<ResolvedPackage>,
        required_by: &mut Vec<(String, String, String)>,
        report: &mut dyn FnMut(&str, String),
    ) {
        for (dep, ver) in &m.dependencies {
            if let Some((_, pv, pby)) = required_by.iter().find(|(d, _, _)| d == dep) {
                if pv != ver {
                    report(
                        "E3014",
                        format!(
                            "package {dep} required at {pv} (by {pby}) and {ver} (by {})",
                            m.name
                        ),
                    );
                    continue;
                }
            }
            if let Some(e) = required_by.iter_mut().find(|(d, _, _)| d == dep) {
                *e = (dep.clone(), ver.clone(), m.name.clone());
            } else {
                required_by.push((dep.clone(), ver.clone(), m.name.clone()));
            }
            if packages.iter().any(|p| &p.name == dep) {
                continue;
            }
            let dir = root_dir.join("decl_modules").join(dep);
            let Some(dm) = parse_manifest(&dir.join("decl.toml"), report) else {
                continue;
            };
            if &dm.name != dep {
                report(
                    "E3013",
                    format!(
                        "package at {} names itself {}, expected {dep}",
                        dir.display(),
                        dm.name
                    ),
                );
            }
            if &dm.version != ver {
                report(
                    "E3016",
                    format!(
                        "package {dep}: manifest version {} differs from required pin {ver}",
                        dm.version
                    ),
                );
            }
            packages.push(ResolvedPackage {
                name: dep.clone(),
                version: dm.version.clone(),
                dir: dir.clone(),
                hash: package_hash(&dir),
            });
            visit(&dm, root_dir, packages, required_by, report);
        }
    }
    visit(
        &manifest,
        &root_dir,
        &mut packages,
        &mut required_by,
        &mut report,
    );

    let resolver: Resolver = {
        let root_dir = root_dir.clone();
        let manifest = manifest.clone();
        let packages = packages.clone();
        Rc::new(move |spec: &str, from_dir: &Path| {
            let (pkg, rest) = match spec.find('/') {
                Some(i) => (&spec[..i], &spec[i + 1..]),
                None => (spec, ""),
            };
            // which package does the importing file belong to?
            let from_abs = std::path::absolute(from_dir).unwrap_or_else(|_| from_dir.to_path_buf());
            let from_pkg_dir = packages
                .iter()
                .find(|p| from_abs.starts_with(&p.dir))
                .map(|p| p.dir.clone())
                .unwrap_or_else(|| root_dir.clone());
            let from_manifest = if from_pkg_dir == root_dir {
                Some(manifest.clone())
            } else {
                parse_manifest(&from_pkg_dir.join("decl.toml"), &mut |_, _| {})
            };
            let from_name = from_manifest
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| "?".into());
            if !from_manifest.map(|m| m.has_dep(pkg)).unwrap_or(false) {
                return Err((
                    "E3010".into(),
                    format!("package {pkg} not declared in [dependencies] of {from_name}"),
                ));
            }
            let Some(p) = packages.iter().find(|p| p.name == pkg) else {
                return Err((
                    "E3004".into(),
                    format!("package {pkg} could not be resolved"),
                ));
            };
            Ok(if rest.is_empty() {
                p.dir.clone()
            } else {
                p.dir.join(rest)
            })
        })
    };
    Some(PackageUniverse {
        root_dir,
        manifest,
        packages,
        resolver,
        diags,
    })
}

// ---------------- decl.lock (§8.7) ----------------
/// The lock file's text (§8.7): one line per package, `name version hash`, sorted by name.
pub fn lock_text(u: &PackageUniverse) -> String {
    let mut ps: Vec<&ResolvedPackage> = u.packages.iter().collect();
    ps.sort_by(|a, b| a.name.cmp(&b.name));
    let lines: Vec<String> = ps
        .iter()
        .map(|p| format!("{} {} {}", p.name, p.version, p.hash))
        .collect();
    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    }
}
/// Write the lock file into the package root; returns its path.
pub fn write_lock(u: &PackageUniverse) -> PathBuf {
    let path = u.root_dir.join("decl.lock");
    let _ = std::fs::write(&path, lock_text(u));
    path
}
/// fail-closed verification: missing entry, version drift, or hash
/// mismatch stops resolution — never a silent re-resolve
pub fn verify_lock(u: &PackageUniverse) -> Vec<Diag> {
    let path = u.root_dir.join("decl.lock");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    let mut out = vec![];
    let mut report =
        |code: &str, message: String| out.push(Diag::error(message, String::new(), Some(code)));
    let mut locked: Vec<(String, String, String)> = vec![];
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        locked.push((
            parts[0].to_string(),
            parts.get(1).unwrap_or(&"").to_string(),
            parts.get(2).unwrap_or(&"").to_string(),
        ));
    }
    for p in &u.packages {
        let Some((_, v, h)) = locked.iter().find(|(n, _, _)| *n == p.name) else {
            report("E3015", format!("lock: missing entry for {}", p.name));
            continue;
        };
        if *v != p.version {
            report(
                "E3016",
                format!(
                    "lock: {} version {v} differs from manifest {}",
                    p.name, p.version
                ),
            );
        } else if *h != p.hash {
            report("E3017", format!("lock: {} content-hash mismatch", p.name));
        }
    }
    out
}
