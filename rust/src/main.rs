//! The `decl` binary — the native Rust runtime's command line (cli.rs).
mod ast;
mod cli;
mod engine;
mod module;
mod parse;
mod semantics;
mod subsume;

fn main() {
    std::process::exit(cli::main(std::env::args().skip(1).collect()));
}
