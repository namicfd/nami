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

/// Directories not copied: build output, git metadata, and other checkouts nested inside.
const SKIPPED_DIRECTORIES: [&str; 2] = ["target", ".git"];

/// Upper bound on directories walked while copying the checkout.
const DIRECTORIES_MAX: u32 = 10_000;

/// A scratch directory that is removed on drop, also when a setup step fails first.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        // Removal is best effort: a failed removal must not hide the test's own failure.
        let _removed = fs::remove_dir_all(&self.0);
    }
}

/// True for a directory the copy leaves out. A `.git` inside a directory (a folder, or the
/// file a worktree has) marks another checkout, such as `.claude/worktrees/<agent>`.
fn is_skipped(source: &Path, directory: &Path) -> bool {
    let name = directory.file_name().map(|name| name.to_string_lossy().into_owned());
    let is_listed = name.is_some_and(|name| SKIPPED_DIRECTORIES.contains(&name.as_str()));
    is_listed || (directory != source && directory.join(".git").exists())
}

/// Copies `file`, which lies below `source`, to the same place below `target`.
fn copy_file(source: &Path, target: &Path, file: &Path) {
    let relative = file.strip_prefix(source).expect("the walk stays below the source");
    let to = target.join(relative);
    fs::create_dir_all(to.parent().expect("a file has a parent")).expect("temp is writable");
    let _bytes = fs::copy(file, &to).expect("a checkout file is copyable");
    assert!(to.is_file(), "{} was copied to {}", relative.display(), to.display());
}

/// Copies the checkout at `source` to `target` with a filesystem walk, without `target/`,
/// `.git` and nested checkouts. Needs no git, so it also works in cargo-mutants' copy.
fn copy_checkout(source: &Path, target: &Path) -> u32 {
    assert!(source.join("Cargo.toml").is_file(), "the source is a workspace");
    assert!(!target.exists(), "the copy starts empty");
    let mut pending = vec![source.to_path_buf()];
    let mut copied = 0_u32;
    for _ in 0..DIRECTORIES_MAX {
        let Some(directory) = pending.pop() else {
            assert!(copied > 0, "the checkout has files");
            return copied;
        };
        for entry in fs::read_dir(&directory).expect("the checkout is readable") {
            let path = entry.expect("a directory entry is readable").path();
            if !path.is_dir() {
                copy_file(source, target, &path);
                copied += 1;
            } else if !is_skipped(source, &path) {
                pending.push(path);
            }
        }
    }
    panic!("the copy visited more than {DIRECTORIES_MAX} directories under {}", source.display());
}

// Goal: `xtask tidy`, started inside a copy of the checkout, reports a violation planted in
// the copy only. Method: copy the checkout to a temp dir, add a debt marker there, and run the
// built binary from a member crate of the copy. A root compiled into the binary would check
// the original tree instead, which lacks the marker, and tidy would pass.
#[test]
fn tidy_checks_the_checkout_around_the_current_directory() {
    let original = xtask::workspace_root();
    let copy = std::env::temp_dir().join(format!("nami-root-copy-{}", std::process::id()));
    if copy.exists() {
        fs::remove_dir_all(&copy).expect("a stale copy is removable");
    }
    let _scratch = Scratch(copy.clone());
    let _copied = copy_checkout(&original, &copy);
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "tidy must fail on the copy's violation: {stderr}");
    assert!(
        stderr.contains(&format!("{PLANTED_FILE}:")) && stderr.contains("debt-marker"),
        "tidy must report the marker planted in the copy: {stderr}"
    );
}
