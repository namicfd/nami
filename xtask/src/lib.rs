//! Library half of `cargo xtask`, so that checks also run as ordinary tests (ADR-0006).

use std::path::{Path, PathBuf};

pub mod hook;
pub mod json;
pub mod task;
pub mod tidy;

/// Returns the workspace root: the parent of this crate's directory.
#[must_use]
pub fn workspace_root() -> PathBuf {
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_directory.parent().expect("xtask lives one level below the root");
    assert!(root.join("Cargo.toml").is_file(), "workspace root must contain Cargo.toml");
    root.to_path_buf()
}
