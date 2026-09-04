// Compile the Decl tree-sitter grammar (parser.c + external scanner).
// The sources come from grammar/ inside the crate (published layout) or
// from ../tree-sitter-decl/src in the source tree.
use std::path::Path;

fn main() {
    let dir = ["grammar", "../tree-sitter-decl/src"]
        .iter()
        .map(Path::new)
        .find(|p| p.join("parser.c").exists())
        .expect("grammar sources not found: run `npm run build` in ../decl-ts or keep ../tree-sitter-decl/src");
    let mut build = cc::Build::new();
    build
        .include(dir)
        .file(dir.join("parser.c"))
        .file(dir.join("scanner.c"))
        .std("c11")
        .warnings(false);
    build.compile("tree-sitter-decl");
    println!("cargo:rerun-if-changed={}", dir.join("parser.c").display());
    println!("cargo:rerun-if-changed={}", dir.join("scanner.c").display());
    println!("cargo:rerun-if-changed=build.rs");
}
