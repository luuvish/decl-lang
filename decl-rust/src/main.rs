//! The `decl` binary — check / evaluate / validate / fmt (cli.rs).
fn main() {
    std::process::exit(decl_lang::cli::main(std::env::args().skip(1).collect()));
}
