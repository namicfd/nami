//! Tidy: style rules that clippy cannot express (ADR-0006).
//!
//! Each rule lives in exactly one tool. Tidy only checks what clippy and rustfmt do not. This
//! first version is text-based; the syntax-aware rules (function length, assertion density,
//! bounded loops, `usize` placement) arrive with a `syn`-based pass.
//!
//! Every violation reads `path:line: error[rule]: what; fix (reference)`, so the message tells
//! the reader how to fix it.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Maximum line length in Rust sources (`TIGER_STYLE.md`, Style by the numbers).
const LINE_COLUMNS_MAX: usize = 100;

/// Upper bound on directories visited, so the walk is bounded (`TIGER_STYLE.md`, Limits).
const DIRECTORIES_MAX: u32 = 100_000;

/// Directories that tidy never enters.
const SKIPPED_DIRECTORIES: [&str; 3] = ["target", ".git", "docs"];

/// Debt markers that must not reach `main`. Built with `concat!` so this file passes its own check.
const DEBT_MARKERS: [&str; 3] = [concat!("TO", "DO"), concat!("FIX", "ME"), concat!("X", "XX")];

/// One rule violation at one place in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// One-based line number, or 0 for a whole-file violation.
    pub line: u32,
    /// Short rule identifier.
    pub rule: &'static str,
    /// What is wrong and how to fix it.
    pub message: &'static str,
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        write!(formatter, "{path}:{}: error[{}]: {}", self.line, self.rule, self.message)
    }
}

/// Checks the whole workspace and returns every violation, sorted by path and line.
///
/// # Errors
///
/// Returns an error if a directory or file cannot be read.
pub fn check(root: &Path) -> io::Result<Vec<Violation>> {
    assert!(root.is_dir(), "tidy root must be a directory");
    let mut violations = Vec::new();
    for path in files(root)? {
        let relative = path.strip_prefix(root).expect("walk stays below the root").to_path_buf();
        if is_snapshot_draft(&path) {
            violations.push(snapshot_draft_violation(relative));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let text = fs::read_to_string(&path)?;
            violations.extend(check_rust_source(&relative, &text));
        }
    }
    violations.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Ok(violations)
}

/// Checks one Rust source file.
#[must_use]
pub fn check_rust_source(path: &Path, text: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line_number = u32::try_from(line_index + 1).expect("source files have < 2^32 lines");
        if line.chars().count() > LINE_COLUMNS_MAX {
            violations.push(Violation {
                path: path.to_path_buf(),
                line: line_number,
                rule: "line-length",
                message: "line is longer than 100 columns; wrap it (TIGER_STYLE.md)",
            });
        }
        if DEBT_MARKERS.iter().any(|marker| line.contains(marker)) {
            violations.push(Violation {
                path: path.to_path_buf(),
                line: line_number,
                rule: "debt-marker",
                message: "debt marker in source; finish the work, or record it as a task in \
                          docs/exec-plans/ (TIGER_STYLE.md, Zero technical debt)",
            });
        }
    }
    violations
}

fn is_snapshot_draft(path: &Path) -> bool {
    path.to_string_lossy().ends_with(".snap.new")
}

fn snapshot_draft_violation(path: PathBuf) -> Violation {
    Violation {
        path,
        line: 0,
        rule: "snapshot-draft",
        message: "unreviewed insta snapshot; review it with `cargo insta review` or delete it, \
                  never accept snapshots blindly (ADR-0006)",
    }
}

/// Lists every file below `root`, skipping build output and docs, in sorted order.
fn files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut found = Vec::new();
    for _ in 0..DIRECTORIES_MAX {
        let Some(directory) = pending.pop() else {
            found.sort();
            return Ok(found);
        };
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if !path.is_dir() {
                found.push(path);
            } else if !is_skipped(&path) {
                pending.push(path);
            }
        }
    }
    panic!("tidy visited more than {DIRECTORIES_MAX} directories under {}", root.display());
}

fn is_skipped(directory: &Path) -> bool {
    let name = directory.file_name().map(|name| name.to_string_lossy());
    name.is_some_and(|name| SKIPPED_DIRECTORIES.contains(&name.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::{Violation, check, check_rust_source};
    use std::path::Path;

    fn rules(violations: &[Violation]) -> Vec<&'static str> {
        violations.iter().map(|violation| violation.rule).collect()
    }

    // Goal: the repository itself is tidy. This is what makes tidy part of every test run.
    #[test]
    fn workspace_is_tidy() {
        let violations = check(&crate::workspace_root()).expect("workspace is readable");
        let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
        assert_eq!(report.join("\n"), "", "tidy found violations");
    }

    // Fixtures: each rule fires on a deliberate violation (ADR-0006, fixture tests).
    #[test]
    fn line_length_rule_fires() {
        let long_line = "x".repeat(101);
        let violations = check_rust_source(Path::new("fixture.rs"), &long_line);
        assert_eq!(rules(&violations), ["line-length"]);
        let at_limit = check_rust_source(Path::new("fixture.rs"), &"x".repeat(100));
        assert_eq!(rules(&at_limit), Vec::<&str>::new());
    }

    #[test]
    fn debt_marker_rule_fires() {
        let source = format!("// {}: later\nfn main() {{}}\n", concat!("TO", "DO"));
        let violations = check_rust_source(Path::new("fixture.rs"), &source);
        assert_eq!(rules(&violations), ["debt-marker"]);
        assert_eq!(violations[0].line, 1);
    }
}
