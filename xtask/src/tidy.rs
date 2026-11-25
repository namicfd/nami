//! Tidy: style rules that clippy cannot express (ADR-0006).
//!
//! Each rule lives in exactly one tool, so tidy only checks what clippy and rustfmt do not:
//!
//! - text rules: line length, debt markers, a goal comment on every test;
//! - syntax rules: function length, assertion density, bounded loops, `usize` placement, no
//!   `Default`, no `Vec` fields, no `dyn` or function pointers in kernels, no `async`, no
//!   `#[ignore]` without a reason;
//! - workspace rules: runtime dependencies, the ADR index, the `#[expect]` budget, unreviewed
//!   snapshots.
//!
//! Every violation reads `path:line: error[rule]: what; fix (reference)`, so the message tells
//! the reader how to fix it.

mod rust_source;
mod text;
mod workspace;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use rust_source::AssertionStats;

/// Upper bound on directories visited, so the walk is bounded (`TIGER_STYLE.md`, Limits).
const DIRECTORIES_MAX: u32 = 100_000;

/// Directories that the source walk never enters. ADRs and plans are checked separately.
const SKIPPED_DIRECTORIES: [&str; 3] = ["target", ".git", "docs"];

/// Skipped only at the workspace root: agent configuration and Claude Code's worktrees, which
/// are other checkouts. Copies of the tree (cargo-mutants) drop the worktrees' `.git` files, so
/// the nested-checkout rule alone does not catch them there.
const ROOT_AGENT_DIRECTORY: &str = ".claude";

/// The one file allowed to use `usize` in signatures and fields (ADR-0002).
const INDEX_MODULE: &str = "crates/base/src/index.rs";

/// One rule violation at one place in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// One-based line number, or 0 for a whole-file violation.
    pub line: u32,
    /// Short rule identifier.
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
        Self { path: path.to_path_buf(), line, rule, message: message.into() }
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

/// Checks the whole workspace and returns every violation, sorted by path and line.
///
/// # Errors
///
/// Returns an error if a directory or file cannot be read.
pub fn check(root: &Path) -> io::Result<Vec<Violation>> {
    assert!(root.is_dir(), "tidy root must be a directory");
    let mut violations = Vec::new();
    let mut expect_count: u32 = 0;
    let mut stats_by_crate: BTreeMap<PathBuf, AssertionStats> = BTreeMap::new();
    for path in files(root)? {
        let relative = path.strip_prefix(root).expect("walk stays below the root").to_path_buf();
        if workspace::is_snapshot_draft(&relative) {
            violations.push(workspace::snapshot_draft_violation(&relative));
        } else if relative.extension().is_some_and(|extension| extension == "rs") {
            let text = fs::read_to_string(&path)?;
            expect_count += text::expect_count(&text);
            let (file_violations, stats) = check_rust_source(&relative, &text);
            violations.extend(file_violations);
            if let Some(crate_root) = crate_root(&relative) {
                stats_by_crate.entry(crate_root).or_insert_with(AssertionStats::zero).add(stats);
            }
        }
    }
    violations.extend(density_violations(&stats_by_crate));
    violations.extend(workspace::check_expect_budget(root, expect_count)?);
    violations.extend(workspace::check_manifests(root)?);
    violations.extend(workspace::check_adrs(root)?);
    violations.extend(crate::task::plan::check(root)?);
    violations.sort_by(|a, b| (&a.path, a.line, a.rule).cmp(&(&b.path, b.line, b.rule)));
    Ok(violations)
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

/// Returns `crates/<name>` for files inside a nami crate.
fn crate_root(relative: &Path) -> Option<PathBuf> {
    let mut components = relative.components();
    let first = components.next()?;
    let second = components.next()?;
    (first.as_os_str() == "crates").then(|| Path::new(&first).join(second))
}

/// Assertion density: at least two assertions per non-trivial function, per crate (ADR-0004).
fn density_violations(stats_by_crate: &BTreeMap<PathBuf, AssertionStats>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (crate_root, stats) in stats_by_crate {
        if !stats.meets_density() {
            let message = format!(
                "{} assertions across {} non-trivial functions; the minimum is 2 per function on \
                 average. Assert arguments, results and invariants (ADR-0004)",
                stats.assertions, stats.functions
            );
            violations.push(Violation::new(crate_root, 0, "assertion-density", message));
        }
    }
    violations
}

/// Lists every file below `root`, skipping build output, docs and nested checkouts (git
/// worktrees such as `.claude/worktrees/*`), in sorted order.
fn files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut found = Vec::new();
    for _ in 0..DIRECTORIES_MAX {
        let Some(directory) = pending.pop() else {
            found.sort();
            return Ok(found);
        };
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if !path.is_dir() {
                found.push(path);
            } else if !is_skipped(&path) && path != root.join(ROOT_AGENT_DIRECTORY) {
                pending.push(path);
            }
        }
    }
    panic!("tidy visited more than {DIRECTORIES_MAX} directories under {}", root.display());
}

fn is_skipped(directory: &Path) -> bool {
    let name = directory.file_name().map(|name| name.to_string_lossy());
    let is_listed = name.is_some_and(|name| SKIPPED_DIRECTORIES.contains(&name.as_ref()));
    // A nested `.git` (a directory, or the file a worktree has) marks another checkout.
    is_listed || directory.join(".git").exists()
}

#[cfg(test)]
mod tests {
    use super::{Scope, check};
    use std::path::Path;

    // Goal: the repository itself is tidy. This is what makes tidy part of every test run.
    #[test]
    fn workspace_is_tidy() {
        let violations = check(&crate::workspace_root()).expect("workspace is readable");
        let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
        assert_eq!(report.join("\n"), "", "tidy found violations");
    }

    // Goal: a workspace with violations reports them, through every layer of the walk.
    #[test]
    fn check_reports_violations_in_a_fixture_workspace() {
        let root = std::env::temp_dir().join(format!("nami-tidy-fixture-{}", std::process::id()));
        let source = root.join("crates/demo/src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(root.join("xtask")).unwrap();
        std::fs::create_dir_all(root.join("docs/design")).unwrap();
        std::fs::create_dir_all(root.join("docs/exec-plans/active/p")).unwrap();
        std::fs::create_dir_all(root.join("target/ignored")).unwrap();
        std::fs::write(root.join("xtask/expect-budget.txt"), "1\n").unwrap();
        std::fs::write(root.join("docs/design/README.md"), "").unwrap();
        std::fs::write(root.join("docs/design/0001-x.md"), "- Status: Accepted\n").unwrap();
        std::fs::write(root.join("crates/demo/Cargo.toml"), "[dependencies]\nserde = \"1\"\n")
            .unwrap();
        let unasserted = format!("fn a() {{\n{}}}\n", "    let _x = 1;\n".repeat(9));
        std::fs::write(source.join("lib.rs"), unasserted).unwrap();
        std::fs::write(root.join("target/ignored/skipped.rs"), "fn a() { loop {} }").unwrap();
        // Only the root `.claude/` is skipped; one inside a crate is checked like any source.
        std::fs::create_dir_all(source.join(".claude")).unwrap();
        std::fs::write(source.join(".claude/b.rs"), "fn b() { loop {} }").unwrap();
        std::fs::create_dir_all(root.join(".claude/agents")).unwrap();
        std::fs::write(root.join(".claude/agents/c.rs"), "fn c() { loop {} }").unwrap();
        // A nested checkout outside `.claude/`, so that the nested-`.git` rule itself is tested.
        let worktree = root.join("other-checkout");
        std::fs::create_dir_all(worktree.join("crates/old/src")).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        std::fs::write(worktree.join("crates/old/src/lib.rs"), "fn a() { loop {} }").unwrap();
        std::fs::write(source.join("a.snap.new"), "").unwrap();
        let violations = check(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let rules: Vec<&str> = violations.iter().map(|violation| violation.rule).collect();
        assert_eq!(
            rules,
            ["assertion-density", "runtime-dependency", "unbounded-loop", "snapshot-draft"]
                .into_iter()
                .chain(["unasserted-function", "adr-index"])
                .chain(["adr-section"; 6])
                .chain(["plan-files", "expect-budget"])
                .collect::<Vec<_>>()
        );
        assert_eq!(violations[0].path, Path::new("crates/demo"));
        assert_eq!(violations[0].to_string().split(':').next(), Some("crates/demo"));
    }

    // Goal: a violation prints as `path:line: error[rule]: message`.
    #[test]
    fn violations_print_in_compiler_style() {
        let violation = super::Violation::new(Path::new("a.rs"), 3, "rule", "what; fix");
        assert_eq!(violation.to_string(), "a.rs:3: error[rule]: what; fix");
    }

    // Goal: crate roots and skipped directories are recognised from paths alone.
    #[test]
    fn path_helpers_classify_paths() {
        let lib = Path::new("crates/base/src/lib.rs");
        assert_eq!(super::crate_root(lib).as_deref(), Some(Path::new("crates/base")));
        assert_eq!(super::crate_root(Path::new("xtask/src/tidy.rs")), None);
        assert!(super::is_skipped(Path::new("/repo/target")), "build output is skipped");
        assert!(!super::is_skipped(Path::new("/repo/crates")), "sources are walked");
    }

    // Goal: paths map to the scopes whose rules apply to them.
    #[test]
    fn scopes_follow_the_crate_layout() {
        assert_eq!(Scope::of(Path::new("crates/base/src/index.rs")), Scope::IndexModule);
        assert_eq!(Scope::of(Path::new("crates/base/src/lib.rs")), Scope::Crate);
        assert_eq!(Scope::of(Path::new("crates/kernels/src/reduce.rs")), Scope::Kernels);
        assert_eq!(Scope::of(Path::new("xtask/src/tidy.rs")), Scope::Tooling);
    }
}
