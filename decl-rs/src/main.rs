//! The `decl` binary — check / evaluate / validate / fmt (cli.rs).
//!
//! The command runs on a thread with a large stack: a member reading a member
//! reading … nests one group of frames per hop, and a real document's chains
//! run hundreds of hops deep (§9.9 sets no limit).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let worker = std::thread::Builder::new()
        .name("decl".into())
        .stack_size(1 << 30)
        .spawn(move || decl_lang::cli::main(args))
        .expect("spawn the evaluation thread");
    let code = worker.join().unwrap_or(1);
    std::process::exit(code);
}
