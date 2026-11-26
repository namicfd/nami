//! Tidy: style rules that clippy cannot express (ADR-0006).
//!
//! Each rule lives in exactly one tool, so tidy only checks what clippy and rustfmt do not. Every
//! rule has a row in [`registry`], and a firing and a passing example in `xtask/fixtures/tidy/`:
//!
//! - source rules are decided from one file ([`check_single_file`]): Rust sources, manifests,
//!   ADRs and the style guide;
//! - workspace rules need the whole tree ([`check`]): the `#[expect]` budget, the assertion
//!   density of each crate, and the documents generated from the registry.
//!
//! Most new guardrails are a ban: one row in [`registry::PATH_BANS`] or
//! [`registry::MACRO_BANS`] plus two fixtures.
//!
//! Every violation reads `path:line: error[rule]: what; fix (reference)`, so the message tells
//! the reader how to fix it.

mod assertions;
pub mod cli;
mod docs;
#[cfg(test)]
mod fixtures;
mod lints;
mod manifest;
pub mod registry;
mod rules_doc;
mod rust_source;
#[cfg(test)]
pub(crate) mod scratch;
mod style_guide;
mod text;
mod tokens;
mod workspace;

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use assertions::AssertionStats;

/// Upper bound on directories visited, so the walk is bounded (`TIGER_STYLE.md`, Limits).
const DIRECTORIES_MAX: u32 = 100_000;

/// Directory names that the source walk never enters. Documents are listed separately.
const SKIPPED_DIRECTORIES: [&str; 3] = ["target", ".git", "docs"];

/// Tidy's own fixtures hold deliberate violations, so the walk skips them.
const FIXTURE_DIRECTORY: &str = "xtask/fixtures";

/// Skipped only at the workspace root: agent configuration and Claude Code's worktrees, which
/// are other checkouts. Copies of the tree (cargo-mutants) drop the worktrees' `.git` files, so
/// the nested-checkout rule alone does not catch them there.
const ROOT_AGENT_DIRECTORY: &str = ".claude";

/// Directory of the ADRs and their index (ADR-0000). File kinds are core: they decide which
/// files the rules check (ADR-0022, pending in #55).
pub const ADR_DIRECTORY: &str = "docs/design";

/// The style guide, relative to the root.
pub const STYLE_GUIDE: &str = "docs/TIGER_STYLE.md";

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

/// True for `NNNN-<title>.md`, the name of an ADR file.
#[must_use]
pub fn is_adr_file_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let has_number = bytes.iter().take(4).filter(|byte| byte.is_ascii_digit()).count() == 4;
    let is_markdown = Path::new(name).extension().is_some_and(|extension| extension == "md");
    has_number && bytes.get(4) == Some(&b'-') && is_markdown
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
    /// The root `Cargo.toml`.
    WorkspaceManifest,
    /// `docs/design/NNNN-<title>.md`.
    Adr,
    /// `docs/TIGER_STYLE.md`.
    StyleGuide,
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
        } else if relative == Path::new("Cargo.toml") {
            Self::WorkspaceManifest
        } else if directory == Path::new(ADR_DIRECTORY) && is_adr_file_name(&name) {
            Self::Adr
        } else if relative == Path::new(STYLE_GUIDE) {
            Self::StyleGuide
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
    check_tree(root, None)
}

/// Checks the tree at `root` as a foreign gate: this binary judges another revision's tree. CI
/// runs the base branch's tidy on a pull request's tree, so that a pull request cannot pass by
/// weakening its own gate.
///
/// Two things differ from [`check`]:
///
/// - the `#[expect]` budget is read from `policy_root`, not from the tree;
/// - the rules in [`registry::REGISTRY_BOUND_RULES`] are skipped. They compare the tree's
///   documents with this binary's own registry, and a tree whose registry differs fails them by
///   construction, which would make every guardrail change look like a loosening.
///
/// Every other rule runs as usual.
///
/// # Errors
///
/// Returns an error if a directory or file cannot be read.
pub fn check_with_policy(root: &Path, policy_root: &Path) -> io::Result<Vec<Violation>> {
    assert!(policy_root.is_dir(), "tidy policy root must be a directory");
    check_tree(root, Some(policy_root))
}

fn check_tree(root: &Path, policy_root: Option<&Path>) -> io::Result<Vec<Violation>> {
    assert!(root.is_dir(), "tidy root must be a directory");
    let is_foreign = policy_root.is_some();
    let mut violations = Vec::new();
    let mut tally = workspace::Tally::new();
    let mut paths = files_below(root, Path::new(""))?;
    paths.extend(document_files(root)?);
    for relative in &paths {
        let kind = FileKind::of(relative);
        if is_foreign && kind == FileKind::StyleGuide {
            continue;
        }
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
    let budget_root = policy_root.unwrap_or(root);
    violations.extend(workspace::check_expect_budget(budget_root, tally.expect_count)?);
    violations.extend(crate::task::plan::check(root)?);
    if !is_foreign {
        violations.extend(rules_doc::check(root)?);
        violations.extend(rules_doc::check_planned_tasks(root)?);
    }
    let is_registry_bound = |rule: &str| registry::REGISTRY_BOUND_RULES.contains(&rule);
    assert!(
        !is_foreign || !violations.iter().any(|violation| is_registry_bound(violation.rule)),
        "a foreign gate skips the rules that are bound to the registry"
    );
    violations.sort_by(|a, b| (&a.path, a.line, a.rule).cmp(&(&b.path, b.line, b.rule)));
    Ok(violations)
}

/// Runs every source rule on one file: every rule that can be decided from that file alone.
///
/// `relative` is the file's path below `root` and decides which rules apply: a Rust source, a
/// crate or workspace manifest, an ADR or the style guide. Any other file has no rules and
/// yields nothing. `text` is the file's content, which need not be on disk yet.
/// Context that a rule needs, such as the ADR index, the names of the workspace crates or the
/// lint tables, is read from `root`.
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
            report.expect_count = workspace::expect_count(text);
        }
        FileKind::CrateManifest => {
            let crates = manifest::workspace_crates(root)?;
            report.violations = manifest::check_manifest(relative, text, &crates);
        }
        FileKind::WorkspaceManifest => {
            report.violations = manifest::check_workspace_manifest(relative, text);
        }
        FileKind::Adr => {
            report.violations = docs::check_adr(relative, text, &docs::adr_index(root)?);
            let names = docs::adr_file_names(root)?;
            report.violations.extend(docs::check_adr_number(relative, text, &names));
        }
        FileKind::StyleGuide => {
            let mechanisms = style_guide::Mechanisms::load(root)?;
            report.violations = style_guide::check(relative, text, &mechanisms);
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
    (violations, assertions::stats(text, scope))
}

/// The documents that have rules. They live under `docs/`, which the source walk skips. The
/// ADR directory must exist.
fn document_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = entries_of(root, ADR_DIRECTORY)?;
    paths.push(PathBuf::from(STYLE_GUIDE));
    paths.retain(|relative| root.join(relative).is_file());
    Ok(paths)
}

/// The entries directly inside `directory`, as paths relative to `root`, sorted.
pub(crate) fn entries_of(root: &Path, directory: &str) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(root.join(directory))?
        .map(|entry| entry.map(|entry| Path::new(directory).join(entry.file_name())))
        .collect::<io::Result<_>>()?;
    paths.sort();
    assert!(
        paths.iter().all(|path| path.starts_with(directory)),
        "entries are below the directory"
    );
    Ok(paths)
}

/// Lists every file below the directory `start` of `root` as a path relative to `root`, in
/// sorted order. Skips build output, documents, tidy's fixtures and nested checkouts (git
/// worktrees such as `.claude/worktrees/*`).
fn files_below(root: &Path, start: &Path) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![root.join(start)];
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
    let is_root_only = [FIXTURE_DIRECTORY, ROOT_AGENT_DIRECTORY].map(Path::new).contains(&relative);
    is_listed || is_checkout || is_root_only
}

#[cfg(test)]
mod tests {
    use super::scratch::Scratch;
    use super::{FileKind, Scope, check, check_single_file, check_with_policy, registry};
    use super::{rules_doc, style_guide};
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
        scratch.write("Cargo.toml", "[workspace.dependencies]\nlibm = { package = \"serde\" }\n");
        scratch.write(style_guide::STYLE_GUIDE, "## Rules\n\n- A rule. `[tidy:no-such-rule]`\n");
        scratch.write("target/ignored/skipped.rs", "fn a() { loop {} }");
        scratch.write("xtask/fixtures/tidy/skipped.rs", "fn a() { loop {} }");
        // Only the root `.claude/` is skipped; one inside a crate is checked like any source.
        scratch.write("crates/demo/src/.claude/b.rs", "fn b() { loop {} }");
        scratch.write(".claude/agents/c.rs", "fn c() { loop {} }");
        // A nested checkout outside `.claude/`, so that the nested-`.git` rule itself is tested.
        scratch.write("other-checkout/.git", "gitdir: elsewhere\n");
        scratch.write("other-checkout/crates/old/src/lib.rs", "fn a() { loop {} }");
        write_broken_plans(&scratch);
        scratch
    }

    fn task_json(id: &str, depends_on: &str, owner_path: &str) -> String {
        format!(
            "{{\"id\": \"{id}\", \"title\": \"A task\", \"depends_on\": [{depends_on}], \
             \"owner_paths\": [\"{owner_path}\"], \"acceptance\": [\"a_test\"], \
             \"passes\": false}}"
        )
    }

    /// Plans that break the plan rules: a directory without its two files, a task list that is
    /// not JSON, a task without a section that depends on a task that does not exist, and a
    /// completed plan with an open task.
    fn write_broken_plans(scratch: &Scratch) {
        scratch.write("docs/exec-plans/active/empty/notes.md", "Neither a plan nor a task list.\n");
        scratch.write("docs/exec-plans/active/garbled/plan.md", "# A plan\n");
        scratch.write("docs/exec-plans/active/garbled/tasks.json", "{\n");
        let orphan = task_json("orphan", "\"missing\"", "crates/demo");
        scratch.write("docs/exec-plans/active/graph/plan.md", "# A plan without sections\n");
        scratch.write(
            "docs/exec-plans/active/graph/tasks.json",
            &format!("{{\"tasks\": [{orphan}]}}\n"),
        );
        let open = task_json("open", "", "crates/other");
        scratch.write("docs/exec-plans/completed/old/plan.md", "### open\n");
        scratch.write(
            "docs/exec-plans/completed/old/tasks.json",
            &format!("{{\"tasks\": [{open}]}}\n"),
        );
    }

    /// Replaces the task with the missing dependency by two tasks that own the same path and
    /// may run at the same time. The owner-path rule runs only on a graph without other faults.
    fn write_overlapping_tasks(scratch: &Scratch) {
        let first = task_json("first", "", "crates/demo");
        let second = task_json("second", "", "crates/demo/src");
        scratch.write(
            "docs/exec-plans/active/graph/tasks.json",
            &format!("{{\"tasks\": [{first}, {second}]}}\n"),
        );
    }

    // Goal: a workspace with violations reports them, through every layer of the walk, sorted
    // by path, and skips build output, fixtures and nested checkouts.
    #[test]
    fn check_reports_violations_in_a_fixture_workspace() {
        let scratch = broken_workspace("walk");
        let violations = check(scratch.root()).unwrap();
        let rules: Vec<&str> = violations.iter().map(|violation| violation.rule).collect();
        let planned_tasks = vec!["planned-task"; registry::PLANNED.len()];
        assert_eq!(
            rules,
            ["runtime-dependency", "assertion-density", "runtime-dependency", "unbounded-loop"]
                .into_iter()
                .chain(["snapshot-draft", "unasserted-function", "rules-doc", "doc-tag"])
                .chain(["adr-index", "adr-number"])
                .chain(["adr-section"; 6])
                .chain(planned_tasks)
                .chain(["plan-files", "tasks-json", "plan-section", "task-graph", "plan-stage"])
                .chain(["expect-budget"])
                .collect::<Vec<_>>()
        );
        assert_eq!(violations[0].path, Path::new("Cargo.toml"));
        assert_eq!(violations[1].path, Path::new("crates/demo"));
        assert_eq!(violations[1].to_string().split(':').next(), Some("crates/demo"));
    }

    // Goal: every workspace rule in the registry fires on the broken workspace, and no workspace
    // rule fires that the registry does not know. A new workspace rule must be added to
    // `broken_workspace`, or this test fails. The owner-path rule needs a second look: it runs
    // only once the task graph has no other fault.
    #[test]
    fn every_workspace_rule_fires_in_a_broken_workspace() {
        let scratch = broken_workspace("workspace-rules");
        let mut violations = check(scratch.root()).unwrap();
        assert!(!fired(&violations).contains("task-owner-paths"), "the graph fault comes first");
        write_overlapping_tasks(&scratch);
        let overlapping = check(scratch.root()).unwrap();
        assert!(!fired(&overlapping).contains("task-graph"), "the graph fault is gone");
        violations.extend(overlapping);
        let registered: BTreeSet<&str> =
            registry::rule_ids_of(registry::RuleKind::Workspace).collect();
        let fired: BTreeSet<&str> =
            fired(&violations).into_iter().filter(|rule| registered.contains(rule)).collect();
        assert_eq!(fired, registered, "workspace rules that fired, against those registered");
    }

    /// A tree on which every rule passes: one planned task per planned mechanism, a style guide
    /// with a valid tag, the generated rules document, and a budget of zero suppressions.
    fn clean_workspace(name: &str) -> Scratch {
        let scratch = Scratch::new(name);
        scratch.write("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n");
        scratch.write("crates/demo/src/lib.rs", "//! A crate without violations.\n");
        scratch.write("xtask/expect-budget.txt", "0\n");
        scratch.write("docs/design/README.md", "No decisions yet.\n");
        scratch.write(style_guide::STYLE_GUIDE, "## Rules\n\n- A rule. `[review]`\n");
        let tasks: Vec<String> = registry::PLANNED
            .iter()
            .map(|planned| task_json(planned.id, "", &format!("crates/{}", planned.id)))
            .collect();
        let sections: Vec<String> =
            registry::PLANNED.iter().map(|planned| format!("### {}\n", planned.id)).collect();
        scratch.write("docs/exec-plans/active/all/plan.md", &sections.concat());
        scratch.write(
            "docs/exec-plans/active/all/tasks.json",
            &format!("{{\"tasks\": [{}]}}\n", tasks.join(", ")),
        );
        scratch.write(rules_doc::RULES_DOC, &rules_doc::render(scratch.root()).unwrap());
        scratch
    }

    fn fired(violations: &[super::Violation]) -> BTreeSet<&'static str> {
        violations.iter().map(|violation| violation.rule).collect()
    }

    // Goal: a foreign gate skips exactly the rules that are bound to the registry. A tree with a
    // stale `docs/RULES.md`, a tag for a rule that this binary does not know and no tasks fails
    // those three rules on its own, and passes when it is judged against a policy directory.
    #[test]
    fn foreign_gate_skips_the_registry_bound_rules() {
        let tree = clean_workspace("foreign-tree");
        let policy = Scratch::new("foreign-policy");
        policy.write("xtask/expect-budget.txt", "0\n");
        assert_eq!(check(tree.root()).unwrap(), Vec::new(), "the tree starts clean");
        tree.write(rules_doc::RULES_DOC, "# Rules of another revision\n");
        tree.write(style_guide::STYLE_GUIDE, "## Rules\n\n- A rule. `[tidy:rule-of-the-future]`\n");
        tree.write("docs/exec-plans/active/all/tasks.json", "{\"tasks\": []}\n");
        let own = check(tree.root()).unwrap();
        assert_eq!(fired(&own), BTreeSet::from(registry::REGISTRY_BOUND_RULES));
        let foreign = check_with_policy(tree.root(), policy.root()).unwrap();
        assert_eq!(foreign, Vec::new(), "a foreign gate does not judge another registry");
    }

    // Goal: a foreign gate still runs every other rule, and reads the `#[expect]` budget from the
    // policy directory, not from the tree it judges.
    #[test]
    fn foreign_gate_runs_the_other_rules_with_the_policy_budget() {
        let tree = clean_workspace("policy-tree");
        let policy = Scratch::new("policy-budget");
        policy.write("xtask/expect-budget.txt", "0\n");
        tree.write("xtask/expect-budget.txt", "7\n");
        let foreign = check_with_policy(tree.root(), policy.root()).unwrap();
        assert_eq!(foreign, Vec::new(), "the tree cannot raise its own budget");
        assert_eq!(fired(&check(tree.root()).unwrap()), BTreeSet::from(["expect-budget"]));
        policy.write("xtask/expect-budget.txt", "1\n");
        let over = check_with_policy(tree.root(), policy.root()).unwrap();
        assert_eq!(fired(&over), BTreeSet::from(["expect-budget"]), "the policy's budget counts");
        policy.write("xtask/expect-budget.txt", "0\n");
        tree.write("crates/demo/src/lib.rs", "fn a() { loop {} }\n");
        tree.write("crates/demo/Cargo.toml", "[dependencies]\nserde = \"1\"\n");
        tree.write("docs/design/0001-x.md", "- Status: Accepted\n");
        let broken = check_with_policy(tree.root(), policy.root()).unwrap();
        let expected =
            ["adr-index", "adr-number", "adr-section", "runtime-dependency", "unbounded-loop"];
        assert_eq!(fired(&broken), BTreeSet::from(expected), "source rules still run");
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
        assert_eq!(rules("docs/design/0001-x.md", "- Status: Accepted\n").len(), 8);
        assert_eq!(rules("crates/demo/src/a.snap.new", ""), ["snapshot-draft"]);
        assert_eq!(
            rules("Cargo.toml", "[workspace.dependencies.libm]\npackage = \"a\"\n").len(),
            1
        );
        assert_eq!(
            rules(style_guide::STYLE_GUIDE, "## Rules\n\n- A rule. `[tidy]`\n"),
            ["doc-tag"]
        );
        assert_eq!(rules("docs/RULES.md", "stale"), Vec::<&str>::new(), "a workspace rule");
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
        assert!(super::is_skipped(&root, Path::new(".claude")), "agent configuration is skipped");
        assert!(!super::is_skipped(&root, Path::new("crates/.claude")), "only at the root");
    }

    // Goal: paths map to the scopes whose rules apply to them.
    #[test]
    fn scopes_follow_the_crate_layout() {
        assert_eq!(Scope::of(Path::new("crates/base/src/index.rs")), Scope::IndexModule);
        assert_eq!(Scope::of(Path::new("crates/base/src/lib.rs")), Scope::Crate);
        assert_eq!(Scope::of(Path::new("crates/kernels/src/reduce.rs")), Scope::Kernels);
        assert_eq!(Scope::of(Path::new("xtask/src/tidy.rs")), Scope::Tooling);
    }

    // Goal: `unasserted-function` (rust_source.rs) and `assertion-density` (assertions.rs)
    // exempt the same test code, so that their two walks cannot drift apart. Method: one long
    // unasserted function in every position; the first rule fires exactly where the second
    // counts the function.
    #[test]
    fn test_code_exemptions_agree() {
        let body = format!("fn f() {{\n{}}}\n", "    let _x = 1;\n".repeat(11));
        let cases = [
            ("crates/solver/src/a.rs", body.clone()),
            ("crates/solver/src/a.rs", format!("#[cfg(test)]\nmod tests {{\n{body}}}\n")),
            ("crates/solver/src/a.rs", format!("#[test]\n{body}")),
            ("crates/solver/src/a.rs", format!("#[cfg(unix)]\nmod m {{\n{body}}}\n")),
            ("crates/solver/src/a.rs", format!("#[cfg(all(test, unix))]\nmod m {{\n{body}}}\n")),
            ("crates/solver/src/a.rs", format!("impl A {{\n{body}}}\n")),
            ("crates/solver/src/a.rs", format!("trait T {{\n{body}}}\n")),
            ("crates/solver/src/a.rs", format!("#[test]\nfn outer() {{\n{body}}}\n")),
            ("crates/solver/tests/a.rs", body.clone()),
            ("xtask/src/a.rs", body.clone()),
        ];
        let mut counted = 0;
        for (path, text) in &cases {
            let (violations, stats) = super::check_rust_source(Path::new(path), text);
            let fires = violations.iter().any(|violation| violation.rule == "unasserted-function");
            assert_eq!(fires, stats.functions > 0, "{path}: {text}");
            counted += u32::from(fires);
        }
        assert_eq!(counted, 7, "both exempt only test modules, test functions and tooling");
    }

    // Goal: ADR files are recognised by their four-digit prefix.
    #[test]
    fn adr_file_names_are_recognised() {
        use super::is_adr_file_name;
        assert!(is_adr_file_name("0013-lint-table-refinements.md"));
        assert!(!is_adr_file_name("README.md"));
        assert!(!is_adr_file_name("13-x.md"));
        assert!(!is_adr_file_name("0013_x.md"), "the number must be followed by a dash");
        assert!(!is_adr_file_name("abcd-x.md"), "the prefix must be digits");
        assert!(!is_adr_file_name("0013-x.txt"), "ADRs are Markdown");
    }

    // Goal: paths map to the file kinds whose rules apply to them, and near misses do not.
    #[test]
    fn file_kinds_follow_the_layout() {
        let kind = |path: &str| FileKind::of(Path::new(path));
        assert_eq!(kind("crates/base/src/lib.rs"), FileKind::RustSource);
        assert_eq!(kind("build.rs"), FileKind::RustSource);
        assert_eq!(kind("crates/base/Cargo.toml"), FileKind::CrateManifest);
        assert_eq!(kind("Cargo.toml"), FileKind::WorkspaceManifest);
        assert_eq!(kind("xtask/Cargo.toml"), FileKind::Other);
        assert_eq!(kind("crates/base/sub/Cargo.toml"), FileKind::Other);
        assert_eq!(kind("docs/design/0001-dependency-policy.md"), FileKind::Adr);
        assert_eq!(kind("docs/design/README.md"), FileKind::Other);
        assert_eq!(kind("docs/0001-dependency-policy.md"), FileKind::Other);
        assert_eq!(kind("docs/learnings/2026-11-25-a.md"), FileKind::Other, "retired (ADR-0021)");
        assert_eq!(kind("docs/TIGER_STYLE.md"), FileKind::StyleGuide);
        assert_eq!(kind("TIGER_STYLE.md"), FileKind::Other);
        assert_eq!(kind("crates/a/tests/snapshots/a.snap.new"), FileKind::SnapshotDraft);
        assert_eq!(kind("crates/a/tests/snapshots/a.snap"), FileKind::Other);
        assert!(FileKind::RustSource.reads_text(), "source rules read the text");
        assert!(!FileKind::SnapshotDraft.reads_text(), "a draft is reported by its name");
    }
}
