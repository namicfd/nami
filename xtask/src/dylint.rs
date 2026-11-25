//! `cargo xtask dylint`: runs the dylint lints, and never skips a crate after a lint changed.
//!
//! cargo decides whether to check a crate again from that crate's own inputs. The lint library
//! is not one of them. After a change in `lints/`, cargo therefore calls every unchanged crate
//! fresh, and the new lint never runs on it: locally, and in CI when `target/` comes from a
//! cache. The dylint driver does tell cargo to track the environment variable
//! `DYLINT_RUSTFLAGS`. This command puts a fingerprint of the lint library into that variable,
//! so that a changed library makes cargo check every crate again (ADR-0017).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Files of the lint library outside `lints/src` that decide what the lints do.
const LINT_FILES: [&str; 3] = ["lints/Cargo.toml", "lints/Cargo.lock", "lints/rust-toolchain.toml"];

/// Directory holding the lint sources.
const LINT_SOURCES: &str = "lints/src";

/// Upper bound on directories walked below `lints/src`.
const DIRECTORIES_MAX: u32 = 1_000;

/// Offset basis of the 64-bit FNV-1a hash.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Prime of the 64-bit FNV-1a hash.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Runs `cargo dylint` over the workspace with the library's fingerprint in `DYLINT_RUSTFLAGS`.
pub fn run(root: &Path) -> ExitCode {
    let fingerprint = match fingerprint(root) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            eprintln!("dylint: cannot read the lint library under lints/: {error}");
            return ExitCode::FAILURE;
        }
    };
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let status = Command::new(cargo)
        .args(["dylint", "--all", "--", "--workspace", "--all-targets", "--locked"])
        .env("DYLINT_RUSTFLAGS", rustflags(std::env::var("DYLINT_RUSTFLAGS").ok(), fingerprint))
        .current_dir(root)
        .status();
    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("dylint: cannot run `cargo dylint`: {error}; is cargo-dylint installed?");
            ExitCode::FAILURE
        }
    }
}

/// The value of `DYLINT_RUSTFLAGS`: what the caller had set, plus a `--cfg` that names the
/// library's fingerprint. No code reads that cfg; it only makes cargo notice the change.
#[must_use]
pub fn rustflags(existing: Option<String>, fingerprint: u64) -> String {
    let marker = format!("--cfg nami_lints_{fingerprint:016x}");
    match existing {
        Some(flags) if !flags.trim().is_empty() => format!("{} {marker}", flags.trim()),
        _ => marker,
    }
}

/// Hashes the path and the contents of every file that decides what the lints do.
///
/// # Errors
///
/// Returns an error if a file of the lint library cannot be read.
pub fn fingerprint(root: &Path) -> io::Result<u64> {
    let mut files: Vec<PathBuf> = LINT_FILES.iter().map(PathBuf::from).collect();
    files.extend(source_files(root)?);
    files.sort();
    assert!(files.len() > LINT_FILES.len(), "the lint library has at least one source file");
    let mut hash = FNV_OFFSET;
    for relative in &files {
        hash = fnv1a(hash, relative.to_string_lossy().as_bytes());
        hash = fnv1a(hash, &[0]);
        hash = fnv1a(hash, &fs::read(root.join(relative))?);
        hash = fnv1a(hash, &[0]);
    }
    Ok(hash)
}

/// Lists every file below `lints/src`, relative to `root`.
fn source_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![PathBuf::from(LINT_SOURCES)];
    let mut found = Vec::new();
    for _ in 0..DIRECTORIES_MAX {
        let Some(directory) = pending.pop() else {
            return Ok(found);
        };
        for entry in fs::read_dir(root.join(&directory))? {
            let relative = directory.join(entry?.file_name());
            if root.join(&relative).is_dir() {
                pending.push(relative);
            } else {
                found.push(relative);
            }
        }
    }
    panic!("lints/src holds more than {DIRECTORIES_MAX} directories");
}

/// Continues a 64-bit FNV-1a hash over `bytes`.
fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    // FNV multiplies modulo 2^64 by definition, so wrapping is the intended arithmetic.
    bytes.iter().fold(hash, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME))
}

#[cfg(test)]
mod tests {
    use super::{FNV_OFFSET, fingerprint, fnv1a, rustflags};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn library(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("nami-dylint-{name}-{}", std::process::id()));
        fs::create_dir_all(root.join("lints/src/nested")).unwrap();
        fs::write(root.join("lints/Cargo.toml"), "[package]\n").unwrap();
        fs::write(root.join("lints/Cargo.lock"), "version = 4\n").unwrap();
        fs::write(root.join("lints/rust-toolchain.toml"), "[toolchain]\n").unwrap();
        fs::write(root.join("lints/src/lib.rs"), "mod a;\n").unwrap();
        fs::write(root.join("lints/src/nested/a.rs"), "fn a() {}\n").unwrap();
        root
    }

    fn fingerprint_after(name: &str, change: impl FnOnce(&Path)) -> (u64, u64) {
        let root = library(name);
        let before = fingerprint(&root).unwrap();
        change(&root);
        let after = fingerprint(&root).unwrap();
        fs::remove_dir_all(&root).unwrap();
        (before, after)
    }

    // Goal: the hash is FNV-1a. The expected values are the published test vectors for the
    // empty string, "a" and "foobar".
    #[test]
    fn fnv1a_matches_the_published_vectors() {
        assert_eq!(fnv1a(FNV_OFFSET, b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(FNV_OFFSET, b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(FNV_OFFSET, b"foobar"), 0x8594_4171_f739_67e8);
    }

    // Goal: the fingerprint stays the same while nothing changes, so cargo keeps its cache.
    #[test]
    fn fingerprint_is_stable() {
        let (before, after) = fingerprint_after("stable", |_| ());
        assert_eq!(before, after);
    }

    // Goal: a change to a lint source, at any depth, changes the fingerprint.
    #[test]
    fn fingerprint_follows_the_sources() {
        let (before, after) = fingerprint_after("source", |root| {
            fs::write(root.join("lints/src/nested/a.rs"), "fn a() { }\n").unwrap();
        });
        assert_ne!(before, after);
    }

    // Goal: a new source file changes the fingerprint, even an empty one.
    #[test]
    fn fingerprint_follows_new_files() {
        let (before, after) = fingerprint_after("added", |root| {
            fs::write(root.join("lints/src/b.rs"), "").unwrap();
        });
        assert_ne!(before, after);
    }

    // Goal: a toolchain bump changes the fingerprint: the lints link against that compiler.
    #[test]
    fn fingerprint_follows_the_toolchain() {
        let (before, after) = fingerprint_after("toolchain", |root| {
            fs::write(root.join("lints/rust-toolchain.toml"), "[toolchain]\nchannel = \"x\"\n")
                .unwrap();
        });
        assert_ne!(before, after);
    }

    // Goal: moving text from one file to the next changes the fingerprint: file boundaries
    // are part of the hash.
    #[test]
    fn fingerprint_separates_files() {
        let (before, after) = fingerprint_after("boundary", |root| {
            fs::write(root.join("lints/Cargo.lock"), "version = 4\n[").unwrap();
            fs::write(root.join("lints/Cargo.toml"), "package]\n").unwrap();
        });
        assert_ne!(before, after);
    }

    // Goal: the caller's own flags are kept, and the marker names the fingerprint in 16 hex
    // digits.
    #[test]
    fn rustflags_keep_what_the_caller_set() {
        assert_eq!(rustflags(None, 0xab), "--cfg nami_lints_00000000000000ab");
        assert_eq!(rustflags(Some("  ".to_owned()), 1), "--cfg nami_lints_0000000000000001");
        assert_eq!(
            rustflags(Some("-Dwarnings ".to_owned()), u64::MAX),
            "-Dwarnings --cfg nami_lints_ffffffffffffffff"
        );
    }
}
