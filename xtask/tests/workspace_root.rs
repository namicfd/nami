//! `cargo xtask` checks the checkout it runs in, not the one it was compiled in (issue #30).
//!
//! A caching `rustc-wrapper` such as sccache can hand one worktree the xtask binary built in
//! another. A root compiled in with `env!("CARGO_MANIFEST_DIR")` then names the other tree.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The source file, relative to the root, that gets a planted violation in the copy.
const PLANTED_FILE: &str = "crates/solver/src/lib.rs";

/// Copies every tracked file of the checkout at `source` that exists on disk to `target`.
fn copy_tracked_files(source: &Path, target: &Path) -> usize {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(source)
        .output()
        .expect("git starts");
    assert!(output.status.success(), "git ls-files failed in {}", source.display());
    let listing = String::from_utf8(output.stdout).expect("git prints UTF-8 paths");
    let mut copied = 0;
    for relative in listing.split('\0').filter(|relative| !relative.is_empty()) {
        let from = source.join(relative);
        if !from.is_file() {
            continue;
        }
        let to = target.join(relative);
        fs::create_dir_all(to.parent().expect("a file has a parent")).expect("temp is writable");
        let _bytes = fs::copy(&from, &to).expect("a tracked file is copyable");
        copied += 1;
    }
    assert!(copied > 0, "the checkout has tracked files");
    copied
}

// Goal: `xtask tidy`, started inside a copy of the checkout, reports a violation planted in
// the copy only. Method: copy the tracked files to a temp dir, add a debt marker there, and
// run the built binary from a member crate of the copy. A root compiled into the binary would
// check the original tree instead, which lacks the marker, and tidy would pass.
#[test]
fn tidy_checks_the_checkout_around_the_current_directory() {
    let original = xtask::workspace_root();
    let copy: PathBuf = std::env::temp_dir().join(format!("nami-root-copy-{}", std::process::id()));
    if copy.exists() {
        fs::remove_dir_all(&copy).expect("a stale copy is removable");
    }
    let _copied = copy_tracked_files(&original, &copy);
    let planted = copy.join(PLANTED_FILE);
    let text = fs::read_to_string(&planted).expect("the planted file was copied");
    let marker = concat!("TO", "DO");
    fs::write(&planted, format!("{text}// {marker}: planted by the workspace_root test\n"))
        .expect("the copy is writable");

    let binary = std::env::var_os("CARGO_BIN_EXE_xtask")
        .expect("cargo and nextest set CARGO_BIN_EXE_xtask when they run a test");
    let output = Command::new(binary)
        .arg("tidy")
        .current_dir(copy.join("crates/solver"))
        .output()
        .expect("xtask starts");
    fs::remove_dir_all(&copy).expect("the copy is removable");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "tidy must fail on the copy's violation: {stderr}");
    assert!(
        stderr.contains(&format!("{PLANTED_FILE}:")) && stderr.contains("debt-marker"),
        "tidy must report the marker planted in the copy: {stderr}"
    );
}
