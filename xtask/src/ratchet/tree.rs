//! The two trees the ratchet compares, and the scratch copy it plants violations in.
//!
//! A tree is a directory of files, compared by content. Version control is not consulted: the
//! base is a `git worktree` in CI, the head a checkout, and the tests use plain directories.
//! The walk skips only what git ignores at the root (build output, mutation-testing output)
//! and nested checkouts, so a file committed anywhere else is seen.
//!
//! **Refused names.** Every path is keyed by text, and a name that is not valid UTF-8 by its
//! bytes, escaped (`\xff`), so it has no lossy text two names could share. An escaped key is
//! printable ASCII with a `\`, so a valid name could still spell it. The ratchet refuses the
//! forms that can take part in such a clash wherever it vouches for the content, so that one key
//! always means one file in every tree it judges (review of #57, rounds 15 and 16):
//! - a name that is not valid UTF-8, anywhere;
//! - a rule path ([`super::scope::is_rule_path`]) whose name holds a `\` or a character outside
//!   printable ASCII, in the base as in the head;
//! - a key that two names map to.

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

/// What sits at a path: a regular file with its bytes, a symbolic link with its target, or a
/// path the ratchet refuses to name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A regular file.
    File(Vec<u8>),
    /// A symbolic link, which is never followed.
    Link(PathBuf),
    /// A path in one of the refused forms of the module docs, with why and the bytes of the
    /// file if it is one. A name that is not valid UTF-8 is keyed by its bytes, escaped, so
    /// that no lossy text stands for it (review of #57, round 15). It is never read, copied or
    /// compared as a changed path; the judge refuses the head that holds it ([`refused`]).
    Refused {
        /// Why the path is refused.
        why: String,
        /// The file's bytes; empty for a directory or a link, which is not entered. For a key
        /// that two names map to, the bytes of both, the smaller first, so that the
        /// fingerprint counts every byte whatever order the directory lists them in.
        content: Vec<u8>,
    },
}

/// Every file and link below `root`, by path relative to `root`, with `/` as separator. A name
/// in a refused form (module docs) is refused, keyed by its escaped bytes if it is not valid
/// UTF-8, and so is a key that two names map to, whatever order the directory lists them in.
/// A refused directory is not entered.
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
            visit(root, &directory, &entry?, &mut found, &mut pending)?;
        }
    }
    panic!("more than {DIRECTORIES_MAX} directories below {}", root.display());
}

/// Records one directory entry below `root`, in `directory`, into `found`, or queues it in
/// `pending` if it is a directory to walk.
///
/// # Errors
///
/// Returns an error if the entry cannot be read.
fn visit(
    root: &Path,
    directory: &Path,
    entry: &fs::DirEntry,
    found: &mut BTreeMap<String, Entry>,
    pending: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let relative = directory.join(entry.file_name());
    let kind = entry.file_type()?;
    // A worktree's `.git` is a file, and is skipped like the directory.
    if SKIPPED_AT_ROOT.iter().any(|skipped| relative == Path::new(skipped)) {
        return Ok(());
    }
    let (key, named) = key_of(&relative);
    let why = if named { refused_rule_name(&key) } else { Some("is not valid UTF-8".to_owned()) };
    if let Some(why) = why {
        let content = if kind.is_file() { fs::read(entry.path())? } else { Vec::new() };
        insert(found, key, Entry::Refused { why, content });
    } else if kind.is_symlink() {
        insert(found, key, Entry::Link(fs::read_link(entry.path())?));
    } else if kind.is_dir() && !is_skipped(root, &relative) {
        pending.push(relative);
    } else if !kind.is_dir() {
        insert(found, key, Entry::File(fs::read(entry.path())?));
    }
    Ok(())
}

/// Why the rule path `key`, a valid UTF-8 name, is refused, if it is: a `\` or a character
/// outside printable ASCII in it. An escaped key is printable ASCII with a `\`, so a valid name
/// without one never spells it, and the blunter rule leaves no other text to doubt (review of
/// #57, round 16). Names outside the rule paths are left as they are: no reader opens them by
/// their key, and a clash there is refused in the head and dropped by [`changed_paths`].
fn refused_rule_name(key: &str) -> Option<String> {
    let plain = key.chars().all(|character| matches!(character, ' '..='~') && character != '\\');
    assert!(!plain || key.is_ascii(), "a plain name is ASCII");
    if plain || !super::scope::is_rule_path(key) {
        return None;
    }
    Some(
        "is a rule path whose name holds a `\\` or a character outside printable ASCII, as an \
         escaped name does"
            .to_owned(),
    )
}

/// Records `entry` at `key`; a key that is already taken is refused, whichever came first,
/// with the bytes of both entries. At most two names share a key, one that is valid UTF-8
/// and one that is not, since the escape is one to one and a valid name is its own key.
fn insert(found: &mut BTreeMap<String, Entry>, key: String, entry: Entry) {
    match found.entry(key) {
        std::collections::btree_map::Entry::Vacant(vacant) => {
            let _inserted = vacant.insert(entry);
        }
        std::collections::btree_map::Entry::Occupied(mut taken) => {
            let why = format!("is the key of two names, `{}`", taken.key());
            let mut both = [bytes_of(taken.get()), bytes_of(&entry)];
            both.sort();
            let content = both.concat();
            assert!(
                content.len() == both[0].len() + both[1].len(),
                "the content keeps both entries' bytes"
            );
            let _replaced = taken.insert(Entry::Refused { why, content });
        }
    }
}

/// The bytes an entry holds: a file's content, a link's target, or a refused path's content.
fn bytes_of(entry: &Entry) -> Vec<u8> {
    match entry {
        Entry::File(bytes) | Entry::Refused { content: bytes, .. } => bytes.clone(),
        Entry::Link(target) => target.as_os_str().as_encoded_bytes().to_vec(),
    }
}

/// The paths below `root` that the ratchet refuses ([`Entry::Refused`]), with why. Sorted.
///
/// # Errors
///
/// Returns an error if the tree cannot be read.
pub fn refused(root: &Path) -> io::Result<Vec<(String, String)>> {
    let found = entries(root)?
        .into_iter()
        .filter_map(|(path, entry)| match entry {
            Entry::Refused { why, .. } => Some((path, why)),
            Entry::File(_) | Entry::Link(_) => None,
        })
        .collect();
    Ok(found)
}

/// The paths whose entry differs between the two trees: changed, added or removed. Sorted.
/// Refused paths are never among them: no reader can open them by their key. A key that either
/// tree refuses is dropped from both, so that a file the other tree holds under it is not
/// compared against nothing (review of #57, round 16); the judge refuses the head instead.
///
/// # Errors
///
/// Returns an error if either tree cannot be read.
pub fn changed_paths(base: &Path, head: &Path) -> io::Result<Vec<String>> {
    let mut before = entries(base)?;
    let mut after = entries(head)?;
    let refused: BTreeSet<String> = before
        .iter()
        .chain(after.iter())
        .filter(|(_, entry)| matches!(entry, Entry::Refused { .. }))
        .map(|(key, _)| key.clone())
        .collect();
    before.retain(|key, _| !refused.contains(key));
    after.retain(|key, _| !refused.contains(key));
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    assert!(paths.iter().all(|path| !refused.contains(*path)), "no refused key is compared");
    let changed: Vec<String> =
        paths.into_iter().filter(|path| before.get(*path) != after.get(*path)).cloned().collect();
    assert!(
        changed.windows(2).all(|pair| matches!(pair, [first, second] if first < second)),
        "the changed paths are sorted and unique"
    );
    Ok(changed)
}

/// A fingerprint of the tree below `root` (FNV-1a over every path and its content, in order),
/// and its symbolic links, sorted. The observer records the fingerprint of the head it ran on,
/// and the judge compares it with that of the head it reads, so that both saw the same files.
///
/// # Errors
///
/// Returns an error if the tree cannot be read.
pub fn fingerprint(root: &Path) -> io::Result<(String, Vec<String>)> {
    let mut hash = crate::dylint::FNV_OFFSET;
    let mut links = Vec::new();
    for (path, entry) in entries(root)? {
        hash = crate::dylint::fnv1a(hash, path.as_bytes());
        match entry {
            Entry::File(bytes) => {
                hash = crate::dylint::fnv1a(hash, &[0]);
                hash = crate::dylint::fnv1a(hash, &bytes);
            }
            Entry::Link(target) => {
                hash = crate::dylint::fnv1a(hash, &[1]);
                hash = crate::dylint::fnv1a(hash, target.as_os_str().as_encoded_bytes());
                links.push(path);
            }
            Entry::Refused { why, content } => {
                hash = crate::dylint::fnv1a(hash, &[2]);
                hash = crate::dylint::fnv1a(hash, why.as_bytes());
                hash = crate::dylint::fnv1a(hash, &content);
            }
        }
        hash = crate::dylint::fnv1a(hash, &[0xff]);
    }
    Ok((format!("{hash:016x}"), links))
}

/// A path relative to the root, spelled with `/` whatever the platform, and whether every part
/// is valid UTF-8. A part that is not is spelled by its bytes, escaped (`\xff`), never by a
/// lossy replacement that two names could share.
fn key_of(relative: &Path) -> (String, bool) {
    let mut named = true;
    let parts: Vec<String> = relative
        .components()
        .map(|part| {
            if let Some(text) = part.as_os_str().to_str() {
                return text.to_owned();
            }
            named = false;
            part.as_os_str().as_encoded_bytes().escape_ascii().to_string()
        })
        .collect();
    assert!(!parts.is_empty(), "a path below the root has a part");
    (parts.join("/"), named)
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
    /// Copies the regular files of `source` to `destination`, which is emptied first, with
    /// their permissions: a script that a test runs must stay executable in the copy, or the
    /// null mutant's run of the tests fails for a reason that is no source text (review of
    /// #57, round 8).
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
                fs::set_permissions(&target, fs::metadata(source.join(&relative))?.permissions())?;
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
    use std::collections::BTreeMap;
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

    // Goal: review of #57, round 15: a name that is not valid UTF-8 is keyed by its escaped
    // bytes and refused, never turned into lossy text; a valid name that spells the same key is
    // refused with it, whichever the directory lists first. Refused paths are no changed paths,
    // no copied files, and still count in the fingerprint.
    #[test]
    fn names_that_are_not_utf8_are_refused() {
        use std::os::unix::ffi::OsStrExt;
        let base = Temporary::new("ratchet-names-base");
        let head = Temporary::new("ratchet-names-head");
        for tree in [&base, &head] {
            tree.write("lints/src/lib.rs", "kept");
        }
        let odd = std::ffi::OsStr::from_bytes(b"\xff.rs");
        std::fs::write(head.root().join("lints/src").join(odd), "hidden").unwrap();
        let refused = super::refused(head.root()).unwrap();
        assert_eq!(refused, [("lints/src/\\xff.rs".to_owned(), "is not valid UTF-8".to_owned())]);
        assert_eq!(changed_paths(base.root(), head.root()).unwrap(), Vec::<String>::new());
        let before = super::fingerprint(head.root()).unwrap().0;
        std::fs::write(head.root().join("lints/src").join(odd), "changed").unwrap();
        assert_ne!(super::fingerprint(head.root()).unwrap().0, before, "its bytes count");
        head.write("lints/src/\\xff.rs", "spelled like the escape");
        let clash = super::refused(head.root()).unwrap();
        assert_eq!(clash.len(), 1, "{clash:?}");
        assert!(clash[0].1.starts_with("is the key of two names"), "{clash:?}");
        let clashed = super::fingerprint(head.root()).unwrap().0;
        head.write("lints/src/\\xff.rs", "spelled like the escape, edited");
        assert_ne!(super::fingerprint(head.root()).unwrap().0, clashed, "both names' bytes count");
        let place = Temporary::new("ratchet-names-copy");
        let copy = Scratch::copy_of(head.root(), &place.root().join("tree")).unwrap();
        let copied: Vec<String> = entries(copy.root()).unwrap().into_keys().collect();
        assert_eq!(copied, ["lints/src/lib.rs"], "refused paths are not copied");
    }

    // Goal: review of #57, round 16: the base holds a valid name that spells an escape, and
    // the head keeps it and adds the name that is not valid UTF-8 with the same key. In the
    // rule paths the base refuses the valid name already; outside them the base holds it as a
    // file. Either way the key is no changed path, since a key that either tree refuses is
    // dropped from both, and the head refuses it as a clash.
    #[test]
    fn a_clash_against_the_base_is_no_changed_path() {
        use std::os::unix::ffi::OsStrExt;
        let base = Temporary::new("ratchet-clash-base");
        let head = Temporary::new("ratchet-clash-head");
        let odd = std::ffi::OsStr::from_bytes(b"\xff.rs");
        for tree in [&base, &head] {
            tree.write("lints/src/lib.rs", "kept");
            tree.write("lints/src/\\xff.rs", "spelled like the escape");
            tree.write("crates/a/\\xff.rs", "spelled like the escape");
        }
        for directory in ["lints/src", "crates/a"] {
            std::fs::write(head.root().join(directory).join(odd), "hidden").unwrap();
        }
        let found = entries(base.root()).unwrap();
        let outside = found.get("crates/a/\\xff.rs");
        assert_eq!(outside, Some(&Entry::File(b"spelled like the escape".to_vec())), "outside");
        let refused_in_base: Vec<String> =
            super::refused(base.root()).unwrap().into_iter().map(|(path, _)| path).collect();
        assert_eq!(refused_in_base, ["lints/src/\\xff.rs"], "the base refuses the rule path");
        let clashes = super::refused(head.root()).unwrap();
        let keys: Vec<&str> = clashes.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(keys, ["crates/a/\\xff.rs", "lints/src/\\xff.rs"], "{clashes:?}");
        assert!(clashes.iter().all(|(_, why)| why.starts_with("is the key of two names")));
        assert_eq!(changed_paths(base.root(), head.root()).unwrap(), Vec::<String>::new());
        assert_eq!(changed_paths(head.root(), base.root()).unwrap(), Vec::<String>::new());
    }

    // Goal: review of #57, round 16: a rule path whose name holds a `\` or a character outside
    // printable ASCII is refused, a directory without being entered; the same names outside
    // the rule paths, and a plain rule path with a space, are files as before.
    #[test]
    fn rule_path_names_that_could_spell_an_escape_are_refused() {
        let tree = Temporary::new("ratchet-rule-names");
        tree.write("lints/src/a\\b.rs", "backslash");
        tree.write("lints/src/caf\u{e9}.rs", "not ASCII");
        tree.write("xtask/fixtures/tidy/bell\u{7}.fires.fixture", "a control character");
        tree.write("lints/ui/odd\\dir/inside.rs", "not entered");
        tree.write("lints/src/plain name.rs", "plain");
        tree.write("crates/a/a\\b.rs", "outside");
        tree.write("crates/a/caf\u{e9}.rs", "outside");
        let refused = super::refused(tree.root()).unwrap();
        let keys: Vec<&str> = refused.iter().map(|(path, _)| path.as_str()).collect();
        let expected = [
            "lints/src/a\\b.rs",
            "lints/src/caf\u{e9}.rs",
            "lints/ui/odd\\dir",
            "xtask/fixtures/tidy/bell\u{7}.fires.fixture",
        ];
        assert_eq!(keys, expected, "{refused:?}");
        assert!(refused.iter().all(|(_, why)| why.starts_with("is a rule path whose name")));
        let found = entries(tree.root()).unwrap();
        let refused_file = found.get("lints/src/caf\u{e9}.rs");
        assert!(
            matches!(refused_file, Some(Entry::Refused { content, .. }) if content == b"not ASCII"),
            "a refused file keeps its bytes: {refused_file:?}"
        );
        assert!(!found.contains_key("lints/ui/odd\\dir/inside.rs"), "a refused directory");
        for kept in ["lints/src/plain name.rs", "crates/a/a\\b.rs", "crates/a/caf\u{e9}.rs"] {
            assert!(matches!(found.get(kept), Some(Entry::File(_))), "{kept} is a file");
        }
    }

    // Goal: review of #57, round 16: a key clash keeps the bytes of both entries, the same
    // whichever the directory lists first, so the fingerprint counts every byte.
    #[test]
    fn a_clash_keeps_both_entries_bytes_in_any_order() {
        let valid = Entry::File(b"valid".to_vec());
        let other =
            Entry::Refused { why: "is not valid UTF-8".to_owned(), content: b"other".to_vec() };
        let mut found = [BTreeMap::new(), BTreeMap::new()];
        super::insert(&mut found[0], "k".to_owned(), valid.clone());
        super::insert(&mut found[0], "k".to_owned(), other.clone());
        super::insert(&mut found[1], "k".to_owned(), other);
        super::insert(&mut found[1], "k".to_owned(), valid);
        assert_eq!(found[0], found[1], "the order of the listing does not count");
        let Some(Entry::Refused { why, content }) = found[0].get("k") else {
            panic!("a clash is refused: {found:?}");
        };
        assert_eq!(why, "is the key of two names, `k`");
        assert_eq!(content, b"othervalid", "both entries' bytes, the smaller first");
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

    // Goal: the copy keeps each file's permissions, so an executable script runs from the copy
    // as from the source, and a read-only file stays read-only.
    #[cfg(unix)]
    #[test]
    fn scratch_copies_keep_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let source = Temporary::new("ratchet-tree-modes");
        source.write("tools/run.sh", "#!/bin/sh\necho ran\n");
        source.write("tools/plain.txt", "plain\n");
        let script = source.root().join("tools/run.sh");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = source.root().join("tools/plain.txt");
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o444)).unwrap();
        let target = Temporary::new("ratchet-tree-modes-copy");
        let copy = Scratch::copy_of(source.root(), &target.root().join("copy")).unwrap();
        let output = std::process::Command::new(copy.root().join("tools/run.sh")).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "ran\n");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&copy.root().join("tools/run.sh")), 0o755);
        assert_eq!(mode(&copy.root().join("tools/plain.txt")), 0o444);
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let copied = copy.root().join("tools/plain.txt");
        std::fs::set_permissions(&copied, std::fs::Permissions::from_mode(0o644)).unwrap();
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
