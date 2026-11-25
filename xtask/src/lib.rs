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
    let manifest_file = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_file).unwrap_or_else(|error| {
        panic!("cannot read the workspace manifest {}: {error}", manifest_file.display())
    });
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
    use crate::tidy::scratch::Scratch;

    // Goal: the root search climbs from a member crate to the directory that holds both the
    // workspace manifest and xtask/, and finds nothing outside a checkout.
    #[test]
    fn workspace_root_is_found_from_any_directory_inside_it() {
        let scratch = Scratch::new("workspace-root-of");
        scratch.write("Cargo.toml", "[workspace]\n");
        scratch.write("xtask/Cargo.toml", "[package]\n");
        scratch.write("crates/solver/Cargo.toml", "[package]\n");
        scratch.write("crates/solver/src/lib.rs", "");
        let root = scratch.root().to_path_buf();
        let nested = root.join("crates/solver/src");
        assert_eq!(super::workspace_root_of(&nested), Some(root.clone()), "from a member");
        assert_eq!(super::workspace_root_of(&root), Some(root.clone()), "from the root");
        assert_eq!(super::workspace_root_of(std::path::Path::new("/")), None, "outside");
    }
}
