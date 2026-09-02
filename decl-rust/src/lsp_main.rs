//! The `decl-lsp` binary — the language server over stdio (lsp.rs).
fn main() {
    std::process::exit(decl_lang::lsp::main());
}
