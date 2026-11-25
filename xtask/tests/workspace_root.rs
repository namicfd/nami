//! `cargo xtask` checks the checkout it runs in, not the one it was compiled in (issue #30).
//!
//! A caching `rustc-wrapper` such as sccache can hand one worktree the xtask binary built in
//! another. A root compiled in with `env!("CARGO_MANIFEST_DIR")` then names the other tree.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// The source file, relative to the root, that gets the planted violation.
const PLANTED_FILE: &str = "crates/solver/src/lib.rs";

/// The files a synthetic workspace needs for tidy to reach its source rules, with their text.
/// Tidy reports the missing documents (such as `docs/RULES.md`) as violations; the test only
/// looks for the planted one.
const SKELETON: [(&str, &str); 5] = [
    ("Cargo.toml", "[workspace]\nmembers = [\"crates/solver\"]\n"),
    ("xtask/Cargo.toml", "[package]\nname = \"xtask\"\n"),
    ("xtask/expect-budget.txt", "0\n"),
    ("docs/TIGER_STYLE.md", "# TigerStyle\n"),
    ("crates/solver/Cargo.toml", "[package]\nname = \"solver\"\n"),
];

/// A synthetic workspace under the temporary directory, removed on drop, also when a setup
/// step panics first. Built from scratch, so it never depends on stray files in a checkout.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str, source: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nami-root-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).expect("a stale workspace is removable");
        }
        let workspace = Self(root);
        fs::create_dir_all(workspace.0.join("docs/design")).expect("temp is writable");
        for (relative, text) in SKELETON.iter().chain([(PLANTED_FILE, source)].iter()) {
            let file = workspace.0.join(relative);
            fs::create_dir_all(file.parent().expect("a file has a parent")).expect("writable");
            fs::write(&file, text).expect("temp is writable");
        }
        assert!(workspace.0.join(PLANTED_FILE).is_file(), "the workspace has its source file");
        workspace
    }

    /// Runs the built `xtask tidy` from `relative` inside the workspace. Returns its stderr.
    fn tidy_from(&self, relative: &str) -> String {
        let binary = std::env::var_os("CARGO_BIN_EXE_xtask")
            .expect("cargo and nextest set CARGO_BIN_EXE_xtask when they run a test");
        let directory = self.0.join(relative);
        assert!(directory.is_dir(), "tidy starts in a directory of the workspace");
        let output =
            Command::new(binary).arg("tidy").current_dir(directory).output().expect("xtask starts");
        String::from_utf8(output.stderr).expect("tidy reports in UTF-8")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Removal is best effort: a failed removal must not hide the test's own failure.
        let _removed = fs::remove_dir_all(&self.0);
    }
}

/// True if one line of `stderr` reports a debt marker in the planted file.
fn reports_planted_marker(stderr: &str) -> bool {
    let prefix = format!("{PLANTED_FILE}:");
    stderr.lines().any(|line| line.starts_with(&prefix) && line.contains("debt-marker"))
}

// Goal: `xtask tidy`, started inside a workspace, checks that workspace. Method: build two
// synthetic workspaces in the temp dir, one with a debt marker planted in a member crate and a
// control without it, and run the built binary from the member crate of each. Only the first
// may report the marker. A root compiled into the binary would check the checkout the binary
// was built in instead, which lacks the marker, so the first run would not report it.
#[test]
fn tidy_checks_the_workspace_around_the_current_directory() {
    let marker = concat!("TO", "DO");
    let planted = Workspace::new("planted", &format!("//! Solver.\n// {marker}: planted\n"));
    let control = Workspace::new("control", "//! Solver.\n");

    let planted_stderr = planted.tidy_from("crates/solver");
    assert!(
        reports_planted_marker(&planted_stderr),
        "tidy must report the planted marker: {planted_stderr}"
    );
    let control_stderr = control.tidy_from("crates/solver");
    assert!(
        !reports_planted_marker(&control_stderr),
        "the control has no marker: {control_stderr}"
    );
    // The control run must have checked its tree, not stopped on an unreadable workspace.
    assert!(
        control_stderr.contains("violation(s)"),
        "tidy ran its rules on the control: {control_stderr}"
    );
}
