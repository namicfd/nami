//! The two trees the ratchet compares, and the scratch copy it plants violations in.
//!
//! A tree is a directory of files, compared by content. Version control is not consulted: the
//! base is a `git worktree` in CI, the head a checkout, and the tests use plain directories.
//! The walk skips only what git ignores at the root (build output, mutation-testing output)
//! and nested checkouts, so a file committed anywhere else is seen.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Paths below the root that are never compared or copied: version control, build output and
/// cargo-mutants' output. All of them are ignored by git at the root.
const SKIPPED_AT_ROOT: [&str; 5] =
    [".git", "target", "lints/target", "mutants.out", "mutants.out.old"];

/// Upper bound on the directories one walk visits (`TIGER_STYLE.md`, Limits).
const DIRECTORIES_MAX: u32 = 100_000;

/// What sits at a path: a regular file with its bytes, or a symbolic link with its target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A regular file.
    File(Vec<u8>),
    /// A symbolic link, which is never followed.
    Link(PathBuf),
}

/// Every file and link below `root`, by path relative to `root`, with `/` as separator.
///
/// # Errors
///
/// Returns an error if a directory or file cannot be read.
pub fn entries(root: &Path) -> io::Result<BTreeMap<String, Entry>> {
    assert!(root.is_dir(), "a tree is a directory: {}", root.display());
    let mut pending = vec![PathBuf::new()];
    let mut found = BTreeMap::new();
    for _ in 0..DIRECTORIES_MAX {
        let Some(directory) = pending.pop() else {
            return Ok(found);
        };
        for entry in fs::read_dir(root.join(&directory))? {
            let entry = entry?;
            let relative = directory.join(entry.file_name());
            let kind = entry.file_type()?;
            // A worktree's `.git` is a file, and is skipped like the directory.
            if SKIPPED_AT_ROOT.iter().any(|skipped| relative == Path::new(skipped)) {
                continue;
            }
            if kind.is_symlink() {
                let _replaced =
                    found.insert(slashed(&relative), Entry::Link(fs::read_link(entry.path())?));
            } else if kind.is_dir() && !is_skipped(root, &relative) {
                pending.push(relative);
            } else if !kind.is_dir() {
                let _replaced =
                    found.insert(slashed(&relative), Entry::File(fs::read(entry.path())?));
            }
        }
    }
    panic!("more than {DIRECTORIES_MAX} directories below {}", root.display());
}

/// The paths whose entry differs between the two trees: changed, added or removed. Sorted.
///
/// # Errors
///
/// Returns an error if either tree cannot be read.
pub fn changed_paths(base: &Path, head: &Path) -> io::Result<Vec<String>> {
    let before = entries(base)?;
    let after = entries(head)?;
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let changed: Vec<String> =
        paths.into_iter().filter(|path| before.get(*path) != after.get(*path)).cloned().collect();
    assert!(changed.len() <= before.len() + after.len(), "a path changes at most once");
    Ok(changed)
}

/// A path relative to the root, spelled with `/` whatever the platform.
fn slashed(relative: &Path) -> String {
    let parts: Vec<String> =
        relative.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
    parts.join("/")
}

/// True for a directory the walk does not enter besides [`SKIPPED_AT_ROOT`]: another checkout
/// nested inside the tree, such as a Claude Code worktree.
fn is_skipped(root: &Path, relative: &Path) -> bool {
    root.join(relative).join(".git").exists()
}

/// True for a relative path made of plain names only: no root, no `.` and no `..`, so that a
/// path read from a fixture cannot leave the tree it is planted in.
#[must_use]
pub fn is_plain_relative(path: &Path) -> bool {
    let mut components = path.components().peekable();
    components.peek().is_some() && components.all(|part| matches!(part, Component::Normal(_)))
}

/// A scratch copy of a tree, made of regular files only: no symbolic links, no `.git` and no
/// build output, so that nothing the ratchet does to it reaches the original (PR #37).
#[derive(Debug)]
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// Copies the regular files of `source` to `destination`, which is emptied first.
    ///
    /// # Errors
    ///
    /// Returns an error if the source cannot be read or the destination cannot be written.
    pub fn copy_of(source: &Path, destination: &Path) -> io::Result<Self> {
        assert!(destination.is_absolute(), "the scratch tree has an absolute path");
        assert!(destination != source, "the scratch tree is never the source itself");
        if destination.exists() {
            fs::remove_dir_all(destination)?;
        }
        fs::create_dir_all(destination)?;
        let mut copied: u32 = 0;
        for (relative, entry) in entries(source)? {
            if let Entry::File(bytes) = entry {
                let target = destination.join(&relative);
                fs::create_dir_all(target.parent().expect("a file below the root has a parent"))?;
                fs::write(&target, bytes)?;
                copied += 1;
            }
        }
        assert!(copied > 0 || entries(destination)?.is_empty(), "the copy holds what was copied");
        Ok(Self { root: destination.to_path_buf() })
    }

    /// The root of the scratch tree.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes `text` at `relative`, keeping what was there, so that [`Planted::remove`] can put
    /// the tree back as it was.
    ///
    /// # Errors
    ///
    /// Returns an error if the file or its directories cannot be written.
    pub fn plant(&self, relative: &Path, text: &str) -> io::Result<Planted> {
        assert!(is_plain_relative(relative), "a planted path stays inside the scratch tree");
        let path = self.root.join(relative);
        let original = if path.is_file() { Some(fs::read(&path)?) } else { None };
        let mut created = Vec::new();
        for ancestor in relative.ancestors().skip(1) {
            let directory = self.root.join(ancestor);
            if ancestor.as_os_str().is_empty() || directory.exists() {
                break;
            }
            created.push(directory);
        }
        if let Some(outermost) = created.last() {
            assert!(outermost.starts_with(&self.root), "directories are created inside the tree");
        }
        fs::create_dir_all(path.parent().expect("a planted file has a parent"))?;
        fs::write(&path, text)?;
        Ok(Planted { path, original, created })
    }
}

/// A file planted in a [`Scratch`] tree, until it is removed.
#[derive(Debug)]
#[must_use = "a planted file stays in the tree until it is removed"]
pub struct Planted {
    path: PathBuf,
    original: Option<Vec<u8>>,
    /// The directories that planting created, innermost first.
    created: Vec<PathBuf>,
}

impl Planted {
    /// Puts back the file that was there, or deletes the planted file and the directories that
    /// planting created.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree cannot be written.
    pub fn remove(self) -> io::Result<()> {
        assert!(self.path.is_file(), "the planted file is still there");
        match &self.original {
            Some(bytes) => fs::write(&self.path, bytes)?,
            None => fs::remove_file(&self.path)?,
        }
        for directory in &self.created {
            fs::remove_dir(directory)?;
        }
        assert!(self.original.is_some() || !self.path.exists(), "the planted file is gone");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, Scratch, changed_paths, entries, is_plain_relative};
    use crate::tidy::scratch::Scratch as Temporary;
    use std::path::Path;

    // Goal: the walk lists files and links by slashed relative path, and skips version control,
    // build output at the root and nested checkouts, but not a `target` directory elsewhere.
    #[test]
    fn entries_skip_only_what_git_ignores_at_the_root() {
        let tree = Temporary::new("ratchet-entries");
        tree.write("xtask/src/a.rs", "a");
        tree.write(".git/HEAD", "ref");
        tree.write("target/debug/x", "built");
        tree.write("lints/target/release/y", "built");
        tree.write("crates/a/target/kept.rs", "kept");
        tree.write("nested/.git", "gitdir: elsewhere");
        tree.write("nested/inside.rs", "hidden");
        std::os::unix::fs::symlink("xtask/src/a.rs", tree.root().join("link")).unwrap();
        let found = entries(tree.root()).unwrap();
        let paths: Vec<&str> = found.keys().map(String::as_str).collect();
        assert_eq!(paths, ["crates/a/target/kept.rs", "link", "xtask/src/a.rs"]);
        assert_eq!(found.get("link"), Some(&Entry::Link("xtask/src/a.rs".into())));
        assert_eq!(found.get("xtask/src/a.rs"), Some(&Entry::File(b"a".to_vec())));
    }

    // Goal: a path counts as changed when its content differs, when it exists on one side
    // only, or when a file became a link; equal files do not count.
    #[test]
    fn changed_paths_compare_content() {
        let base = Temporary::new("ratchet-changed-base");
        let head = Temporary::new("ratchet-changed-head");
        for tree in [&base, &head] {
            tree.write("same.rs", "same");
            tree.write("Cargo.toml", "[workspace]\n");
        }
        base.write("edited.rs", "before");
        head.write("edited.rs", "after");
        base.write("removed.rs", "gone");
        head.write("added.rs", "new");
        base.write("linked.rs", "file");
        base.write(".git", "gitdir: a worktree's link to its repository");
        std::os::unix::fs::symlink("same.rs", head.root().join("linked.rs")).unwrap();
        let changed = changed_paths(base.root(), head.root()).unwrap();
        assert_eq!(changed, ["added.rs", "edited.rs", "linked.rs", "removed.rs"]);
        assert_eq!(changed_paths(base.root(), base.root()).unwrap(), Vec::<String>::new());
    }

    // Goal: the copy holds the regular files only, and planting a file over an existing one or
    // in new directories is undone exactly by removing it.
    #[test]
    fn scratch_copies_files_and_plants_reversibly() {
        let source = Temporary::new("ratchet-scratch-source");
        source.write("Cargo.toml", "[workspace]\n");
        source.write(".git/HEAD", "ref");
        std::os::unix::fs::symlink("Cargo.toml", source.root().join("link")).unwrap();
        let place = Temporary::new("ratchet-scratch-copy");
        let scratch = Scratch::copy_of(source.root(), &place.root().join("tree")).unwrap();
        let copied: Vec<String> = entries(scratch.root()).unwrap().into_keys().collect();
        assert_eq!(copied, ["Cargo.toml"], "no links and no version control");
        let before = entries(scratch.root()).unwrap();
        let over = scratch.plant(Path::new("Cargo.toml"), "planted").unwrap();
        let fresh = scratch.plant(Path::new("crates/new/src/lib.rs"), "planted").unwrap();
        let text = std::fs::read_to_string(scratch.root().join("Cargo.toml")).unwrap();
        assert_eq!(text, "planted");
        fresh.remove().unwrap();
        over.remove().unwrap();
        assert_eq!(entries(scratch.root()).unwrap(), before, "the tree is as it was");
        assert!(!scratch.root().join("crates").exists(), "created directories are removed");
    }

    // Goal: only plain relative paths may be planted: no root, no parent and no empty path.
    #[test]
    fn planted_paths_stay_inside_the_tree() {
        assert!(is_plain_relative(Path::new("crates/solver/src/example.rs")));
        assert!(!is_plain_relative(Path::new("/etc/passwd")));
        assert!(!is_plain_relative(Path::new("crates/../../escape.rs")));
        assert!(!is_plain_relative(Path::new("./crates/a.rs")));
        assert!(!is_plain_relative(Path::new("")));
    }
}
