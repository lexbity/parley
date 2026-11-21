//! Parley command-line entry point.
//!
//! `parley --version` reports the build identity and is exercised by the
//! package smoke step of `scripts/check-slice`. Workspace launch — exclusive
//! lock, migration, reconciliation, service startup and browser hand-off — is
//! specified by RFC-0004 slice S01 and is delivered by that slice; every other
//! invocation exits nonzero rather than pretending to run.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("parley {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    eprintln!(
        "parley: workspace launch is delivered by RFC-0004 slice S01 and is not implemented yet."
    );
    std::process::exit(2);
}
