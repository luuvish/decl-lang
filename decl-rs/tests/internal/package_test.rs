//! package: the manifest reader and the package hash the lock file rests on.
use crate::common::{copy_dir, root, temp_dir};
use decl_lang::package::{package_hash, parse_manifest};

#[test]
fn manifest() {
    let dir = temp_dir("manifest");
    let read = |text: &str| {
        std::fs::write(dir.join("decl.toml"), text).unwrap();
        let mut codes: Vec<String> = vec![];
        let m = parse_manifest(&dir.join("decl.toml"), &mut |c, _m| {
            codes.push(c.to_string())
        });
        (m, codes)
    };
    let (good, codes) =
        read("name = \"app\"\nversion = \"1.0.0\"\n\n[dependencies]\ncorelib = \"1.0.0\"\n");
    assert!(codes.is_empty(), "{codes:?}");
    let good = good.expect("a manifest");
    assert_eq!(
        (good.name.as_str(), good.version.as_str()),
        ("app", "1.0.0")
    );
    assert_eq!(
        good.dependencies,
        vec![("corelib".to_string(), "1.0.0".to_string())]
    );
    let (_, codes) = read("name = \"app\"\nversion = \"1.0.0\"\nflavor = \"x\"\n\n[dependencies]\ncorelib = \"^1.0\"\n");
    assert!(
        codes.contains(&"E3011".to_string()) && codes.contains(&"E3012".to_string()),
        "{codes:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hash() {
    let corelib = root().join("tests/packages/app/decl_modules/corelib");
    let lock = std::fs::read_to_string(root().join("tests/packages/lock/decl.lock")).unwrap();
    let locked = lock.trim().split(' ').nth(2).unwrap().to_string();
    let h1 = package_hash(&corelib);
    assert_eq!(h1, locked);
    assert_eq!(package_hash(&corelib), h1, "the same on a second call");
    let copy = temp_dir("hash");
    copy_dir(&corelib, &copy);
    let base = copy.join("types/base.decl");
    let mut s = std::fs::read_to_string(&base).unwrap();
    s.push_str("// drift\n");
    std::fs::write(&base, s).unwrap();
    assert_ne!(
        package_hash(&copy),
        h1,
        "different for a copy with one file appended to"
    );
    let _ = std::fs::remove_dir_all(&copy);
}
