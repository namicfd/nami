//! `cargo xtask`: project tooling for nami.
//!
//! Scripts are Rust, not shell, so they are portable and typed (`TIGER_STYLE.md`, Tooling).

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match arguments.as_slice() {
        ["tidy"] => run_tidy(),
        ["tidy", "--root", root] => xtask::tidy::cli::tidy(root, None),
        ["tidy", "--root", root, "--policy", policy] => xtask::tidy::cli::tidy(root, Some(policy)),
        ["rules"] => xtask::tidy::cli::rules(&xtask::workspace_root(), false),
        ["rules", "--write"] => xtask::tidy::cli::rules(&xtask::workspace_root(), true),
        ["doctor"] => xtask::doctor::run(),
        ["dylint"] => xtask::dylint::run(&xtask::workspace_root()),
        ["protected", options @ ..] => xtask::pr::run_protected(options),
        ["pr-body", options @ ..] => xtask::pr::run_body(options),
        ["hook", event] => xtask::hook::run(event),
        ["task", rest @ ..] => xtask::task::run(rest),
        _ => {
            eprintln!("usage: cargo xtask tidy [--root <dir> [--policy <dir>]] | rules [--write]");
            eprintln!(
                "       | doctor | dylint | protected [--base <ref>] | pr-body [--base <ref>]"
            );
            eprintln!("       | hook <post-edit|stop|session-start> | task <command>");
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
