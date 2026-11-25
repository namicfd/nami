//! Tidy: style rules that clippy cannot express (ADR-0006).
//!
//! Each rule lives in exactly one tool, so tidy only checks what clippy and rustfmt do not. Every
//! rule has a row in [`registry`], and a firing and a passing example in `xtask/fixtures/tidy/`:
//!
//! - source rules are decided from one file ([`check_single_file`]): Rust sources, crate
//!   manifests and ADRs;
//! - workspace rules need the whole tree ([`check`]): the `#[expect]` budget and the assertion
//!   density of each crate.
//!
//! Every violation reads `path:line: error[rule]: what; fix (reference)`, so the message tells
//! the reader how to fix it.

mod docs;
#[cfg(test)]
mod fixtures;
mod manifest;
pub mod registry;
mod rust_source;
#[cfg(test)]
mod scratch;
mod text;
mod workspace;

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use rust_source::AssertionStats;

/// Upper bound on directories visited, so the walk is bounded (`TIGER_STYLE.md`, Limits).
const DIRECTORIES_MAX: u32 = 100_000;

/// Directory names that the source walk never enters. Documents are listed separately.
const SKIPPED_DIRECTORIES: [&str; 3] = ["target", ".git", "docs"];

/// Tidy's own fixtures hold deliberate violations, so the walk skips them.
const FIXTURE_DIRECTORY: &str = "xtask/fixtures";

/// The one file allowed to use `usize` in signatures and fields (ADR-0002).
const INDEX_MODULE: &str = "crates/base/src/index.rs";

/// One rule violation at one place in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// One-based line number, or 0 for a whole-file violation.
    pub line: u32,
    /// Rule identifier, as registered in [`registry`].
    pub rule: &'static str,
    /// What is wrong and how to fix it.
    pub message: String,
}

impl Violation {
    pub(crate) fn new(
        path: &Path,
        line: u32,
        rule: &'static str,
        message: impl Into<String>,
    ) -> Self {
        assert!(registry::is_registered(rule), "tidy rule `{rule}` is not in the registry");
        let message = message.into();
        assert!(!message.is_empty(), "a violation says what is wrong and how to fix it");
        Self { path: path.to_path_buf(), line, rule, message }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        write!(formatter, "{path}:{}: error[{}]: {}", self.line, self.rule, self.message)
    }
}

/// Where a source file lives, which decides the rules that apply to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// A nami crate under `crates/`, other than the two special cases below.
    Crate,
    /// `crates/kernels`: additionally no `dyn` and no function pointers (ADR-0003).
    Kernels,
    /// The index module: the only place where `usize` may appear in signatures (ADR-0002).
    IndexModule,
    /// Tooling (`xtask/`) and anything else outside `crates/`.
    Tooling,
}

impl Scope {
    /// Classifies a path relative to the workspace root.
    #[must_use]
    pub fn of(relative: &Path) -> Self {
        if relative == Path::new(INDEX_MODULE) {
            Self::IndexModule
        } else if relative.starts_with("crates/kernels") {
            Self::Kernels
        } else if relative.starts_with("crates") {
            Self::Crate
        } else {
            Self::Tooling
        }
    }

    /// True for nami's own crates, where the full style applies.
    #[must_use]
    pub fn is_nami(self) -> bool {
        self != Self::Tooling
    }
}

/// The kinds of file that tidy has source rules for, decided from the path alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileKind {
    /// A `.rs` file anywhere.
    RustSource,
    /// `crates/<name>/Cargo.toml`.
    CrateManifest,
    /// `docs/design/NNNN-<title>.md`.
    Adr,
    /// `*.snap.new`: an insta snapshot that nobody reviewed.
    SnapshotDraft,
    /// Anything else: no rule applies.
    Other,
}

impl FileKind {
    fn of(relative: &Path) -> Self {
        let name = relative.file_name().map_or_default(|name| name.to_string_lossy());
        let directory = relative.parent().unwrap_or_else(|| Path::new(""));
        if workspace::is_snapshot_draft(relative) {
            Self::SnapshotDraft
        } else if relative.extension().is_some_and(|extension| extension == "rs") {
            Self::RustSource
        } else if name == "Cargo.toml" && directory.parent() == Some(Path::new("crates")) {
            Self::CrateManifest
        } else if directory == Path::new(docs::ADR_DIRECTORY) && docs::is_adr_file_name(&name) {
            Self::Adr
        } else {
            Self::Other
        }
    }

    /// True if the rules for this kind look at the file's text.
    fn reads_text(self) -> bool {
        !matches!(self, Self::SnapshotDraft | Self::Other)
    }
}

/// What the source rules found in one file, with the counts that the workspace rules add up.
struct FileReport {
    violations: Vec<Violation>,
    stats: AssertionStats,
    expect_count: u32,
}

/// Checks the whole workspace and returns every violation, sorted by path and line.
///
/// # Errors
///
/// Returns an error if a directory or file cannot be read.
pub fn check(root: &Path) -> io::Result<Vec<Violation>> {
    assert!(root.is_dir(), "tidy root must be a directory");
    let mut violations = Vec::new();
    let mut tally = workspace::Tally::new();
    let mut paths = source_files(root)?;
    paths.extend(document_files(root)?);
    for relative in &paths {
        let kind = FileKind::of(relative);
        let text = if kind.reads_text() {
            fs::read_to_string(root.join(relative))?
        } else {
            String::new()
        };
        let report = check_file(root, relative, &text)?;
        violations.extend(report.violations);
        tally.add(relative, report.expect_count, report.stats);
    }
    violations.extend(workspace::density_violations(&tally.stats_by_crate));
    violations.extend(workspace::check_expect_budget(root, tally.expect_count)?);
    violations.sort_by(|a, b| (&a.path, a.line, a.rule).cmp(&(&b.path, b.line, b.rule)));
    Ok(violations)
}

/// Runs every source rule on one file: every rule that can be decided from that file alone.
///
/// `relative` is the file's path below `root` and decides which rules apply: a Rust source, a
/// crate manifest or an ADR. Any other file has no rules and yields nothing. `text` is the
/// file's content, which need not be on disk yet. Context that a rule needs, such as the ADR
/// index or the names of the workspace crates, is read from `root`.
///
/// # Errors
///
/// Returns an error if that context cannot be read.
pub fn check_single_file(root: &Path, relative: &Path, text: &str) -> io::Result<Vec<Violation>> {
    Ok(check_file(root, relative, text)?.violations)
}

fn check_file(root: &Path, relative: &Path, text: &str) -> io::Result<FileReport> {
    assert!(relative.is_relative(), "the path must be relative to the workspace root");
    let mut report =
        FileReport { violations: Vec::new(), stats: AssertionStats::zero(), expect_count: 0 };
    match FileKind::of(relative) {
        FileKind::RustSource => {
            (report.violations, report.stats) = check_rust_source(relative, text);
            report.expect_count = text::expect_count(text);
        }
        FileKind::CrateManifest => {
            let crates = manifest::workspace_crates(root)?;
            report.violations = manifest::check_manifest(relative, text, &crates);
        }
        FileKind::Adr => {
            report.violations = docs::check_adr(relative, text, &docs::adr_index(root)?);
        }
        FileKind::SnapshotDraft => {
            report.violations.push(workspace::snapshot_draft_violation(relative));
        }
        FileKind::Other => {}
    }
    Ok(report)
}

/// Checks one Rust source file. Returns its violations and its assertion statistics.
#[must_use]
pub fn check_rust_source(relative: &Path, text: &str) -> (Vec<Violation>, AssertionStats) {
    let scope = Scope::of(relative);
    let mut violations = text::check(relative, text);
    let report = rust_source::check(relative, text, scope);
    violations.extend(report.violations);
    (violations, report.stats)
}

/// The documents that have rules. They live under `docs/`, which the source walk skips.
fn document_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    docs::entries_of(root, docs::ADR_DIRECTORY)
}

/// Lists every file below `root` as a path relative to it, in sorted order. Skips build output,
/// documents, tidy's fixtures and nested checkouts (git worktrees such as `.claude/worktrees/*`).
fn source_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut found = Vec::new();
    for _ in 0..DIRECTORIES_MAX {
        let Some(directory) = pending.pop() else {
            found.sort();
            return Ok(found);
        };
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let relative = path.strip_prefix(root).expect("walk stays below the root");
            if !path.is_dir() {
                found.push(relative.to_path_buf());
            } else if !is_skipped(root, relative) {
                pending.push(path);
            }
        }
    }
    panic!("tidy visited more than {DIRECTORIES_MAX} directories under {}", root.display());
}

fn is_skipped(root: &Path, relative: &Path) -> bool {
    let name = relative.file_name().map(|name| name.to_string_lossy());
    let is_listed = name.is_some_and(|name| SKIPPED_DIRECTORIES.contains(&name.as_ref()));
    // A nested `.git` (a directory, or the file a worktree has) marks another checkout.
    let is_checkout = root.join(relative).join(".git").exists();
    is_listed || is_checkout || relative == Path::new(FIXTURE_DIRECTORY)
}

#[cfg(test)]
mod tests {
    use super::scratch::Scratch;
    use super::{FileKind, Scope, check, check_single_file, registry};
    use std::collections::BTreeSet;
    use std::path::Path;

    // Goal: the repository itself is tidy. This is what makes tidy part of every test run.
    #[test]
    fn workspace_is_tidy() {
        let violations = check(&crate::workspace_root()).expect("workspace is readable");
        let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
        assert_eq!(report.join("\n"), "", "tidy found violations");
    }

    /// A workspace in which every workspace rule fires, with files that the walk must skip.
    fn broken_workspace(name: &str) -> Scratch {
        let scratch = Scratch::new(name);
        scratch.write("xtask/expect-budget.txt", "1\n");
        scratch.write("docs/design/README.md", "");
        scratch.write("docs/design/0001-x.md", "- Status: Accepted\n");
        scratch.write("crates/demo/Cargo.toml", "[dependencies]\nserde = \"1\"\n");
        let unasserted = format!("fn a() {{\n{}}}\n", "    let _x = 1;\n".repeat(9));
        scratch.write("crates/demo/src/lib.rs", &unasserted);
        scratch.write("crates/demo/src/a.snap.new", "");
        scratch.write("target/ignored/skipped.rs", "fn a() { loop {} }");
        scratch.write("xtask/fixtures/tidy/skipped.rs", "fn a() { loop {} }");
        scratch.write(".claude/worktrees/other/.git", "gitdir: elsewhere\n");
        scratch.write(".claude/worktrees/other/crates/old/src/lib.rs", "fn a() { loop {} }");
        scratch
    }

    // Goal: a workspace with violations reports them, through every layer of the walk, sorted
    // by path, and skips build output, fixtures and nested checkouts.
    #[test]
    fn check_reports_violations_in_a_fixture_workspace() {
        let scratch = broken_workspace("walk");
        let violations = check(scratch.root()).unwrap();
        let rules: Vec<&str> = violations.iter().map(|violation| violation.rule).collect();
        assert_eq!(
            rules,
            ["assertion-density", "runtime-dependency", "snapshot-draft", "unasserted-function"]
                .into_iter()
                .chain(["adr-index"])
                .chain(["adr-section"; 6])
                .chain(["expect-budget"])
                .collect::<Vec<_>>()
        );
        assert_eq!(violations[0].path, Path::new("crates/demo"));
        assert_eq!(violations[0].to_string().split(':').next(), Some("crates/demo"));
    }

    // Goal: every workspace rule in the registry fires on the broken workspace, and no workspace
    // rule fires that the registry does not know. A new workspace rule must be added to
    // `broken_workspace`, or this test fails.
    #[test]
    fn every_workspace_rule_fires_in_a_broken_workspace() {
        let scratch = broken_workspace("workspace-rules");
        let violations = check(scratch.root()).unwrap();
        let registered: BTreeSet<&str> =
            registry::rule_ids_of(registry::RuleKind::Workspace).collect();
        let fired: BTreeSet<&str> = violations
            .iter()
            .map(|violation| violation.rule)
            .filter(|rule| registered.contains(rule))
            .collect();
        assert_eq!(fired, registered, "workspace rules that fired, against those registered");
    }

    // Goal: the single-file entry point reports what the whole-tree check reports for that
    // file, and nothing for files without rules.
    #[test]
    fn single_file_check_matches_the_walk() {
        let scratch = broken_workspace("single-file");
        let rules = |relative: &str, text: &str| -> Vec<&'static str> {
            let violations = check_single_file(scratch.root(), Path::new(relative), text).unwrap();
            violations.iter().map(|violation| violation.rule).collect()
        };
        assert_eq!(rules("crates/demo/Cargo.toml", "[dependencies]\nserde = \"1\"\n").len(), 1);
        assert_eq!(rules("crates/demo/Cargo.toml", "[dependencies]\ndemo = \"1\"\n").len(), 0);
        assert_eq!(rules("crates/demo/src/lib.rs", "fn a() { loop {} }"), ["unbounded-loop"]);
        assert_eq!(rules("docs/design/0001-x.md", "- Status: Accepted\n").len(), 7);
        assert_eq!(rules("crates/demo/src/a.snap.new", ""), ["snapshot-draft"]);
        assert_eq!(rules("README.md", "loop {}"), Vec::<&str>::new());
    }

    // Goal: a violation prints as `path:line: error[rule]: message`.
    #[test]
    fn violations_print_in_compiler_style() {
        let violation = super::Violation::new(Path::new("a.rs"), 3, "line-length", "what; fix");
        assert_eq!(violation.to_string(), "a.rs:3: error[line-length]: what; fix");
    }

    // Goal: a rule id that is not in the registry cannot be reported.
    #[test]
    #[should_panic(expected = "tidy rule `no-such-rule` is not in the registry")]
    fn unregistered_rules_cannot_be_reported() {
        let _violation = super::Violation::new(Path::new("a.rs"), 3, "no-such-rule", "what; fix");
    }

    // Goal: skipped directories are recognised from paths relative to the root.
    #[test]
    fn skipped_directories_are_recognised() {
        let root = crate::workspace_root();
        assert!(super::is_skipped(&root, Path::new("target")), "build output is skipped");
        assert!(super::is_skipped(&root, Path::new("xtask/fixtures")), "fixtures are skipped");
        assert!(!super::is_skipped(&root, Path::new("crates")), "sources are walked");
        assert!(!super::is_skipped(&root, Path::new("crates/fixtures")), "only tidy's fixtures");
    }

    // Goal: paths map to the scopes whose rules apply to them.
    #[test]
    fn scopes_follow_the_crate_layout() {
        assert_eq!(Scope::of(Path::new("crates/base/src/index.rs")), Scope::IndexModule);
        assert_eq!(Scope::of(Path::new("crates/base/src/lib.rs")), Scope::Crate);
        assert_eq!(Scope::of(Path::new("crates/kernels/src/reduce.rs")), Scope::Kernels);
        assert_eq!(Scope::of(Path::new("xtask/src/tidy.rs")), Scope::Tooling);
    }

    // Goal: paths map to the file kinds whose rules apply to them, and near misses do not.
    #[test]
    fn file_kinds_follow_the_layout() {
        let kind = |path: &str| FileKind::of(Path::new(path));
        assert_eq!(kind("crates/base/src/lib.rs"), FileKind::RustSource);
        assert_eq!(kind("build.rs"), FileKind::RustSource);
        assert_eq!(kind("crates/base/Cargo.toml"), FileKind::CrateManifest);
        assert_eq!(kind("Cargo.toml"), FileKind::Other);
        assert_eq!(kind("crates/base/sub/Cargo.toml"), FileKind::Other);
        assert_eq!(kind("docs/design/0001-dependency-policy.md"), FileKind::Adr);
        assert_eq!(kind("docs/design/README.md"), FileKind::Other);
        assert_eq!(kind("docs/0001-dependency-policy.md"), FileKind::Other);
        assert_eq!(kind("crates/a/tests/snapshots/a.snap.new"), FileKind::SnapshotDraft);
        assert_eq!(kind("crates/a/tests/snapshots/a.snap"), FileKind::Other);
        assert!(FileKind::RustSource.reads_text(), "source rules read the text");
        assert!(!FileKind::SnapshotDraft.reads_text(), "a draft is reported by its name");
    }
}
