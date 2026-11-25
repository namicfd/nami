//! Command-line entry points beyond plain `cargo xtask tidy`.
//!
//! - `cargo xtask tidy --root <dir> [--policy <dir>]` checks another tree with this binary.
//! - `cargo xtask rules [--write]` prints or writes `docs/RULES.md`.
//!
//! This is process glue: it validates the arguments, calls the library and maps the result to
//! an exit code. Everything it calls is tested on its own.

use super::{Violation, check, check_with_policy, rules_doc};
use std::fs;
use std::io;
use std::path::Path;
use std::process::ExitCode;

/// Checks the tree at `root` with this binary's rules and prints the violations.
///
/// With `policy`, tidy acts as a foreign gate: see [`check_with_policy`].
pub fn tidy(root: &str, policy: Option<&str>) -> ExitCode {
    for directory in [Some(root), policy].into_iter().flatten() {
        if !Path::new(directory).is_dir() {
            eprintln!("tidy: `{directory}` is not a directory");
            return ExitCode::FAILURE;
        }
    }
    let result = match policy {
        Some(policy) => check_with_policy(Path::new(root), Path::new(policy)),
        None => check(Path::new(root)),
    };
    report(result)
}

/// Prints `docs/RULES.md` as the registry generates it for this workspace, or writes it with
/// `write`.
pub fn rules(write: bool) -> ExitCode {
    rules_at(&crate::workspace_root(), write)
}

fn rules_at(root: &Path, write: bool) -> ExitCode {
    let text = match rules_doc::render(root) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("rules: cannot read the workspace: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !write {
        print!("{text}");
        return ExitCode::SUCCESS;
    }
    match fs::write(root.join(rules_doc::RULES_DOC), text) {
        Ok(()) => {
            eprintln!("rules: wrote {}", rules_doc::RULES_DOC);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rules: cannot write {}: {error}", rules_doc::RULES_DOC);
            ExitCode::FAILURE
        }
    }
}

fn report(result: io::Result<Vec<Violation>>) -> ExitCode {
    match result {
        Ok(violations) if violations.is_empty() => ExitCode::SUCCESS,
        Ok(violations) => {
            for violation in &violations {
                eprintln!("{violation}");
            }
            eprintln!("tidy: {} violation(s)", violations.len());
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("tidy: cannot read the tree: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{rules, rules_at, tidy};
    use crate::tidy::rules_doc;
    use crate::tidy::scratch::Scratch;
    use std::process::ExitCode;

    /// `ExitCode` has no equality, so compare what it prints.
    fn is(code: ExitCode, expected: ExitCode) -> bool {
        format!("{code:?}") == format!("{expected:?}")
    }

    // Goal: the exit code says whether the tree is tidy. A tree with violations, a tree that
    // cannot be read and a directory that does not exist are failures, not crashes.
    #[test]
    fn tidy_exit_code_follows_the_result() {
        let root = crate::workspace_root();
        let root = root.to_str().expect("the workspace path is UTF-8");
        assert!(is(tidy(root, None), ExitCode::SUCCESS), "this workspace is tidy");
        assert!(is(tidy(root, Some(root)), ExitCode::SUCCESS), "and tidy under its own policy");
        assert!(is(tidy("/no/such/directory", None), ExitCode::FAILURE), "a missing root");
        assert!(is(tidy(root, Some("/no/such/directory")), ExitCode::FAILURE), "a missing policy");
        let scratch = Scratch::new("cli-tidy");
        let tree = scratch.root().to_str().expect("the scratch path is UTF-8");
        assert!(is(tidy(tree, Some(root)), ExitCode::FAILURE), "a tree that cannot be read");
        scratch.write("docs/design/README.md", "");
        assert!(is(tidy(tree, Some(root)), ExitCode::FAILURE), "the budget does not match");
        assert!(!is(ExitCode::SUCCESS, ExitCode::FAILURE), "the comparison tells them apart");
    }

    // Goal: `rules` prints without touching the tree, `rules --write` writes exactly what the
    // registry generates, and a tree without a style guide is a failure.
    #[test]
    fn rules_prints_or_writes_the_document() {
        let scratch = Scratch::new("cli-rules");
        assert!(is(rules_at(scratch.root(), false), ExitCode::FAILURE), "no style guide to count");
        scratch.write("docs/TIGER_STYLE.md", "## Rules\n\n- A rule. `[review]`\n");
        let document = scratch.root().join(rules_doc::RULES_DOC);
        assert!(is(rules_at(scratch.root(), false), ExitCode::SUCCESS));
        assert!(!document.exists(), "printing writes nothing");
        assert!(is(rules_at(scratch.root(), true), ExitCode::SUCCESS));
        let written = std::fs::read_to_string(&document).unwrap();
        assert_eq!(written, rules_doc::render(scratch.root()).unwrap());
        assert_eq!(rules_doc::check(scratch.root()).unwrap(), Vec::new(), "it is current");
        std::fs::remove_file(&document).unwrap();
        std::fs::create_dir(&document).unwrap();
        assert!(
            is(rules_at(scratch.root(), true), ExitCode::FAILURE),
            "the file cannot be written"
        );
        assert!(is(rules(false), ExitCode::SUCCESS), "this workspace has a style guide");
    }
}
