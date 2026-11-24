//! `cargo xtask`: project tooling for nami.
//!
//! Scripts are Rust, not shell, so they are portable and typed (`TIGER_STYLE.md`, Tooling).

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [command] = arguments.as_slice()
        && command == "tidy"
    {
        return run_tidy();
    }
    eprintln!("usage: cargo xtask tidy");
    ExitCode::FAILURE
}

fn run_tidy() -> ExitCode {
    let root = xtask::workspace_root();
    match xtask::tidy::check(&root) {
        Ok(violations) if violations.is_empty() => ExitCode::SUCCESS,
        Ok(violations) => {
            for violation in &violations {
                eprintln!("{violation}");
            }
            eprintln!("tidy: {} violation(s)", violations.len());
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("tidy: cannot read the workspace: {error}");
            ExitCode::FAILURE
        }
    }
}
