//! Developer command line for underscore.
//!
//! The first milestone is `underscore play <book> <music>` against local
//! files on a desktop output device. Until then this binary only reports
//! what exists, so the workspace builds end to end from the first commit.

use std::process::ExitCode;

use underscore_core::PlayerConfig;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None | Some("--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!("underscore {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("defaults") => {
            println!("{:#?}", PlayerConfig::default());
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("underscore: unknown command `{other}`");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "underscore: your audiobook or podcast on top, your own music underneath.\n\
         \n\
         Usage: underscore <command>\n\
         \n\
         Commands:\n\
         \x20 defaults   Print every tunable's default value\n\
         \x20 --version  Print the version\n\
         \x20 --help     Print this help\n\
         \n\
         Playback commands arrive with the first milestone; see docs/architecture.md."
    );
}
