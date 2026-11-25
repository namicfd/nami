//! Test support: a scratch tree on disk that is removed when the test ends.

use std::fs;
use std::path::{Path, PathBuf};

/// A directory under the system's temporary directory, deleted on drop.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// Creates an empty scratch tree. `name` must be unique among the tests of this crate,
    /// because tests may run as threads of one process.
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nami-tidy-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).expect("a stale scratch tree can be removed");
        }
        fs::create_dir_all(&root).expect("the temporary directory is writable");
        Self { root }
    }

    /// The root of the scratch tree.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes `text` to `relative`, creating the directories above it.
    pub fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        let parent = path.parent().expect("a file inside the scratch tree has a parent");
        fs::create_dir_all(parent).expect("the scratch tree is writable");
        fs::write(&path, text).expect("the scratch tree is writable");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover directory in the temporary directory is harmless.
        let _removed = fs::remove_dir_all(&self.root);
    }
}
