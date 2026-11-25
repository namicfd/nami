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
    assert!(current.starts_with(&root), "the workspace root contains the current directory");
    root
}

/// The nearest ancestor of `path`, itself included, that is a workspace root: it holds
/// `Cargo.toml` and `xtask/Cargo.toml`. A member crate's `Cargo.toml` alone does not count.
#[must_use]
pub fn workspace_root_of(path: &Path) -> Option<PathBuf> {
    assert!(path.is_absolute(), "the search for the workspace root starts at an absolute path");
    let root = path.ancestors().find(|directory| {
        directory.join("Cargo.toml").is_file() && directory.join("xtask/Cargo.toml").is_file()
    })?;
    assert!(path.starts_with(root), "the workspace root is an ancestor of the start");
    Some(root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;

    // Goal: the root search climbs from a member crate to the directory that holds both the
    // workspace manifest and xtask/, and finds nothing outside a checkout.
    #[test]
    fn workspace_root_is_found_from_any_directory_inside_it() {
        let root = std::env::temp_dir().join(format!("nami-root-{}", std::process::id()));
        let nested = root.join("crates/solver/src");
        fs::create_dir_all(&nested).expect("the temp directory is writable");
        fs::create_dir_all(root.join("xtask")).expect("the temp directory is writable");
        fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("the root is writable");
        fs::write(root.join("xtask/Cargo.toml"), "[package]\n").expect("xtask is writable");
        fs::write(root.join("crates/solver/Cargo.toml"), "[package]\n").expect("writable");
        assert_eq!(super::workspace_root_of(&nested), Some(root.clone()), "from a member");
        assert_eq!(super::workspace_root_of(&root), Some(root.clone()), "from the root");
        assert_eq!(super::workspace_root_of(std::path::Path::new("/")), None, "outside");
        fs::remove_dir_all(&root).expect("the scratch root is removable");
    }
}
