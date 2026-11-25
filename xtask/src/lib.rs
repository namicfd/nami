//! Library half of `cargo xtask`, so that checks also run as ordinary tests (ADR-0006).

use std::path::{Path, PathBuf};

pub mod doctor;
pub mod dylint;
pub mod hook;
pub mod issue;
pub mod json;
pub mod pr;
pub mod task;
pub mod tidy;

/// Returns the workspace root: the nami checkout around the current directory.
///
/// The root is found at run time, never compiled in. A compile-time path such as
/// `env!("CARGO_MANIFEST_DIR")` names the tree the binary was first built in, and a caching
/// `rustc-wrapper` (sccache) hands that binary to every other worktree, which then checks the
/// wrong tree (issue #30). `cargo xtask`, `cargo test` and nextest all run inside the checkout.
///
/// # Panics
///
/// Panics if the current directory is not inside a nami checkout.
#[must_use]
pub fn workspace_root() -> PathBuf {
    let current = std::env::current_dir().expect("the current directory is readable");
    let root = workspace_root_of(&current).unwrap_or_else(|| {
        panic!(
            "{} is not inside a nami checkout (no ancestor holds Cargo.toml and \
             xtask/Cargo.toml); run cargo xtask from within the checkout",
            current.display()
        )
    });
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    assert!(
        manifest.lines().any(|line| line.trim() == "[workspace]"),
        "{} must declare [workspace]: the root is the workspace, not a member crate",
        root.display()
    );
    root
}

/// Upper bound on the directories climbed while looking for the workspace root.
const ANCESTORS_MAX: u32 = 64;

/// The nearest ancestor of `path`, itself included, that is a workspace root: it holds
/// `Cargo.toml` and `xtask/Cargo.toml`. A member crate's `Cargo.toml` alone does not count.
///
/// # Panics
///
/// Panics if `path` is relative, or more than `ANCESTORS_MAX` directories deep.
#[must_use]
pub fn workspace_root_of(path: &Path) -> Option<PathBuf> {
    assert!(path.is_absolute(), "the search for the workspace root starts at an absolute path");
    let mut candidate = Some(path);
    for _ in 0..ANCESTORS_MAX {
        let directory = candidate?;
        if directory.join("Cargo.toml").is_file() && directory.join("xtask/Cargo.toml").is_file() {
            return Some(directory.to_path_buf());
        }
        candidate = directory.parent();
    }
    panic!("{} is more than {ANCESTORS_MAX} directories deep", path.display());
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    /// A scratch directory that is removed on drop, also when an assertion fails first.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            // Removal is best effort: a failed removal must not hide the test's own failure.
            let _removed = fs::remove_dir_all(&self.0);
        }
    }

    // Goal: the root search climbs from a member crate to the directory that holds both the
    // workspace manifest and xtask/, and finds nothing outside a checkout.
    #[test]
    fn workspace_root_is_found_from_any_directory_inside_it() {
        let root = std::env::temp_dir().join(format!("nami-root-{}", std::process::id()));
        let _scratch = Scratch(root.clone());
        let nested = root.join("crates/solver/src");
        fs::create_dir_all(&nested).expect("the temp directory is writable");
        fs::create_dir_all(root.join("xtask")).expect("the temp directory is writable");
        fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("the root is writable");
        fs::write(root.join("xtask/Cargo.toml"), "[package]\n").expect("xtask is writable");
        fs::write(root.join("crates/solver/Cargo.toml"), "[package]\n").expect("writable");
        assert_eq!(super::workspace_root_of(&nested), Some(root.clone()), "from a member");
        assert_eq!(super::workspace_root_of(&root), Some(root.clone()), "from the root");
        assert_eq!(super::workspace_root_of(std::path::Path::new("/")), None, "outside");
    }
}
