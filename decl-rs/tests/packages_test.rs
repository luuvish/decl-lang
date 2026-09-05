//! Packages, manifests, and the lock file (§8.6–8.7) through the package
//! corpus (tests/packages/cases.json): the manifest and resolution errors
//! as the command line reports them, and the lock — written by the API into
//! a copy of the package, bit for bit the committed text, verified clean,
//! then failing closed on content drift, version drift, and a missing entry.
mod common;
use common::*;
use decl_lang::package::{open_package_universe, verify_lock, write_lock};
use std::path::PathBuf;

#[test]
fn packages_corpus() {
    let root = root();
    let text = std::fs::read_to_string(root.join("tests/packages/cases.json")).unwrap();
    let cases = parse(&text);
    let mut failures: Vec<String> = vec![];
    let codes_of = |v: &Value| -> Vec<String> {
        match v {
            Value::JArr(xs) => xs.iter().map(|x| common::text(x).to_string()).collect(),
            _ => vec![],
        }
    };

    // the manifest and resolution errors, as `decl check` reports them
    if let Some(Value::JArr(errors)) = get(&cases, "errors") {
        for e in errors.iter() {
            let entry = common::text(get(e, "entry").unwrap());
            let want = codes_of(get(e, "codes").unwrap());
            let got = check_codes(&root.join(entry));
            if got != want {
                failures.push(format!("{entry}: expected {want:?}, got {got:?}"));
            }
        }
    }

    // the lock: reproducible, then fail-closed
    let lock = get(&cases, "lock").unwrap();
    let package = root.join(common::text(get(lock, "package").unwrap()));
    let entry_name = common::text(get(lock, "entry").unwrap()).to_string();
    let expected =
        std::fs::read_to_string(root.join(common::text(get(lock, "lock").unwrap()))).unwrap();
    // a fresh copy of the package, with its lock written by the API
    let fresh = || -> (PathBuf, PathBuf) {
        let dir = temp_dir("pkg");
        copy_dir(&package, &dir);
        let entry = dir.join(&entry_name);
        write_lock(&open_package_universe(&entry).expect("in a package"));
        (dir, entry)
    };
    let (dir, entry) = fresh();
    let written = std::fs::read_to_string(dir.join("decl.lock")).unwrap();
    if written != expected {
        failures.push(format!("the lock is not the committed text: {written:?}"));
    }
    if !verify_lock(&open_package_universe(&entry).unwrap()).is_empty() {
        failures.push("a fresh lock does not verify clean".into());
    }
    let codes = check_codes(&entry);
    if !codes.is_empty() {
        failures.push(format!(
            "the command line refuses the locked package: {codes:?}"
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(Value::JArr(drifts)) = get(lock, "drift") {
        for d in drifts.iter() {
            let name = common::text(get(d, "name").unwrap());
            let (dir, entry) = fresh();
            if let Some(Value::JObj(files)) = get(d, "append") {
                for (f, t) in files.iter() {
                    let p = dir.join(f);
                    let mut s = std::fs::read_to_string(&p).unwrap();
                    s.push_str(common::text(t));
                    std::fs::write(&p, s).unwrap();
                }
            }
            if let Some(Value::JArr(pair)) = get(d, "lock_replace") {
                let p = dir.join("decl.lock");
                let s = std::fs::read_to_string(&p).unwrap();
                std::fs::write(
                    &p,
                    s.replacen(common::text(&pair[0]), common::text(&pair[1]), 1),
                )
                .unwrap();
            }
            if let Some(t) = get(d, "lock_text") {
                std::fs::write(dir.join("decl.lock"), common::text(t)).unwrap();
            }
            let want = codes_of(get(d, "codes").unwrap());
            let got = check_codes(&entry);
            if got != want {
                failures.push(format!("{name}: expected {want:?}, got {got:?}"));
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    assert!(
        failures.is_empty(),
        "packages corpus failures:\n  {}",
        failures.join("\n  ")
    );
}
