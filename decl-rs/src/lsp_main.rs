//! The `decl-lsp` binary — the language server over stdio (lsp.rs).
fn main() {
    // `decl-lsp --version`: the same string as `decl --version`
    if std::env::args().skip(1).any(|a| a == "--version") {
        println!("decl-lsp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    std::process::exit(decl_lang::lsp::main());
}
