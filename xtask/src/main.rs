//! `cargo xtask`: project tooling for nami.
//!
//! Scripts are Rust, not shell, so they are portable and typed (`TIGER_STYLE.md`, Tooling).

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match arguments.as_slice() {
        ["tidy"] => run_tidy(),
        ["hook", event] => xtask::hook::run(event),
        ["task", rest @ ..] => xtask::task::run(rest),
        _ => {
            eprintln!("usage: cargo xtask tidy | hook <post-edit|stop> | task <command>");
            ExitCode::FAILURE
        }
    }
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
