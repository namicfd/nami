//! `cargo xtask ratchet`: proves that a pull request which changes rule paths only tightens
//! the rules, so that it can merge without the code owner (proposed in ADR-0022).
//!
//! It runs in shadow mode until that policy is accepted: CI shows its verdict on every pull
//! request, nothing requires it, and every guardrail path keeps its code owner.
//!
//! The work is split in two, so that the judge never runs the pull request's code:
//!
//! - **observe** (`cargo xtask ratchet observe`) runs in the job that builds the head. It plants
//!   the base's cases in a scratch copy of the head tree, runs the head's tools on them, and
//!   writes what they printed as plain data ([`observations`]).
//! - **judge** (`cargo xtask ratchet judge`) runs in a job that runs no head code. It reads the
//!   base's corpus from its own checkout, the head's source as data ([`source`]), and the
//!   observations, and decides.
//!
//! Without a subcommand, both run in one process, for local use and the tests. See [`cli`] for
//! the options.
//!
//! **Threat model.** The ratchet is sound against honest mistakes, and against any loosening
//! that shows without running head code: rule tables, scopes, allowed lists and `check_*` call
//! sites are read from the head's source by the base. Against head code written to deceive, it
//! is defence in depth only; the reviewer app, sampled audits and the trusted core back it. The
//! reviewer app is the bot that `NAMI_REVIEWER_LOGIN` names, and the review it posts runs in CI
//! (pending #59), where no authoring session holds its key.
//!
//! **One tree.** The observer and the judge read the same head: both refuse a head with a
//! symbolic link, so the observer's scratch copy, which holds regular files only, has every
//! file of the head; and the observer records the head's fingerprint, which the judge checks.
//!
//! **Output.** Each finding is one line on stderr, `place: error[ratchet-<check>]: what; fix`.
//! A Markdown summary goes to stdout; its last line is the verdict.
//!
//! **Exit code.** 0: pass, also when paths outside the rule paths changed (`pass, outside the
//! rule paths`: CODEOWNERS decides on those). 1: fail, at least one finding. 2: no verdict:
//! bad arguments, a tool that could not run or answer, an output that cannot be read, or a
//! change the ratchet cannot vouch for (no mutant, no reviewer app) where no code owner reviews.

pub mod cli;
mod corpus;
mod form;
mod lint;
mod mutants;
pub mod observations;
pub mod plant;
pub mod review;
pub mod scope;
pub mod source;
pub mod tree;

use observations::Observations;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tree::Scratch;

/// Variables that the tools the ratchet runs never inherit: the GitHub token, which only the
/// review check needs, and the overrides of rustup and cargo, so that each tree's pinned
/// toolchain and own target directory apply.
const WITHHELD_VARIABLES: [&str; 5] =
    ["GH_TOKEN", "GITHUB_TOKEN", "RUSTUP_TOOLCHAIN", "RUSTUP_TOOLCHAIN_SOURCE", "CARGO_TARGET_DIR"];

/// The base's CODEOWNERS, which decides whether the owner reviews a pull request anyway.
const CODEOWNERS: &str = ".github/CODEOWNERS";

/// The checks, as they are named in findings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
    /// Every base rule, ban row, `check_*` call and enforced lint is still there, no looser.
    RuleSet,
    /// The base's tidy fixtures hold through the head's tidy.
    TidyCorpus,
    /// The base's lint UI cases hold through the head's lint library.
    LintCorpus,
    /// The head's commands report the base's cases planted in the head tree.
    EndToEnd,
    /// Mutation testing of the rule-path diff misses nothing.
    Mutants,
    /// The reviewer app reviewed the head, and resolved every thread.
    Review,
}

impl Check {
    /// The name printed as `error[ratchet-<name>]`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::RuleSet => "rule-set",
            Self::TidyCorpus => "tidy-corpus",
            Self::LintCorpus => "lint-corpus",
            Self::EndToEnd => "end-to-end",
            Self::Mutants => "mutants",
            Self::Review => "review",
        }
    }
}

/// One way in which the head is looser than the base, or not proven.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    /// Where: a path, `path:line`, or the pull request.
    pub place: String,
    /// The check that found it.
    pub check: Check,
    /// What is wrong and how to fix it.
    pub message: String,
}

impl Finding {
    /// A finding at `place`.
    #[must_use]
    pub fn new(place: &str, check: Check, message: String) -> Self {
        assert!(!place.is_empty(), "a finding names its place");
        assert!(!message.is_empty(), "a finding says what is wrong");
        Self { place: place.to_owned(), check, message }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let place = &self.place;
        write!(formatter, "{place}: error[ratchet-{}]: {}", self.check.name(), self.message)
    }
}

/// The review check's inputs: where to read the reviews, of which pull request, and the
/// reviewer app's login if the maintainer has set one up.
pub struct ReviewSource<'a> {
    /// GitHub, or a stand-in.
    pub reviews: &'a dyn review::Reviews,
    /// The pull request.
    pub pull: u32,
    /// The pull request's head commit that was judged. Reviews count only on it, and it must
    /// still be the head when they are read.
    pub head: String,
    /// The reviewer app's login; `None` until the app exists.
    pub reviewer: Option<String>,
}

/// What the ratchet needs besides the two trees.
pub struct Tools<'a> {
    /// The head's xtask binary. `None` builds it from the head.
    pub head_xtask: Option<PathBuf>,
    /// The cargo to run `cargo dylint` and `cargo mutants` with.
    pub cargo: OsString,
    /// A directory for scratch trees and build output. It is created if needed.
    pub work: PathBuf,
    /// False skips mutation testing.
    pub mutants: bool,
    /// The review check's inputs; `None` skips the review check.
    pub review: Option<ReviewSource<'a>>,
    /// The seed of the placements; `None` derives it from the head's content
    /// ([`plant::seed_of`]).
    pub seed: Option<u64>,
    /// The commits judged, as `base..head`, for the summary; `None` when only trees are given.
    pub commits: Option<String>,
}

/// What the ratchet found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Changed paths that are rule paths.
    pub rule_paths: Vec<String>,
    /// Changed paths outside the rule paths.
    pub outside: Vec<String>,
    /// True if the base's CODEOWNERS owns a changed path, so the owner reviews the PR anyway.
    pub owner_reviews: bool,
    /// True if the base's CODEOWNERS would still own a changed path once the rule paths are
    /// unowned (ADR-0022): the owner reviews the PR after the policy flip too.
    pub owner_reviews_after: bool,
    /// True if a code owner approved the head commit of the pull request.
    pub owner_approved: bool,
    /// The checks that ran.
    pub checks: Vec<Check>,
    /// Everything that failed, sorted.
    pub findings: Vec<Finding>,
    /// What the ratchet could not vouch for, with no owner to stand in: no verdict.
    pub unproven: Vec<String>,
    /// What would be unproven after the policy flip, when the rule paths have no owner.
    pub unproven_after: Vec<String>,
    /// The commits judged, as `base..head`, for the summary.
    pub range: String,
    /// What the summary should say besides the findings: counts, skipped checks and why.
    pub notes: Vec<String>,
}

impl Verdict {
    /// The verdict line: the last line of the summary. It is the verdict under today's
    /// CODEOWNERS, which decides the exit code.
    #[must_use]
    pub fn line(&self) -> String {
        self.line_with(&self.unproven)
    }

    /// The verdict the PR would get after the policy flip of ADR-0022, when the rule paths have
    /// no owner, so that shadow mode shows the evidence the flip asks for.
    #[must_use]
    pub fn line_after(&self) -> String {
        self.line_with(&self.unproven_after)
    }

    fn line_with(&self, unproven: &[String]) -> String {
        if !self.findings.is_empty() {
            format!("ratchet: fail, {} finding(s)", self.findings.len())
        } else if !unproven.is_empty() {
            format!(
                "ratchet: no verdict, {} thing(s) unproven; the code owner decides",
                unproven.len()
            )
        } else if self.outside.is_empty() {
            "ratchet: pass".to_owned()
        } else {
            format!(
                "ratchet: pass, outside the rule paths: {} changed path(s) that CODEOWNERS \
                 decides on",
                self.outside.len()
            )
        }
    }

    /// The Markdown summary for the CI job, ending with [`Verdict::line`].
    #[must_use]
    pub fn summary(&self) -> String {
        let mut lines = vec!["### Ratchet (ADR-0022)".to_owned(), String::new()];
        let names: Vec<&str> = self.checks.iter().map(|check| check.name()).collect();
        let ran = if names.is_empty() { "none".to_owned() } else { names.join(", ") };
        lines.push(format!("- Rule paths changed: {}", self.rule_paths.len()));
        lines.push(format!("- Changed outside the rule paths: {}", self.outside.len()));
        lines.push(format!("- Judged: {}", self.range));
        lines.push(format!("- Verdict today (every guardrail path owned): `{}`", self.line()));
        lines.push(format!(
            "- Verdict after ADR-0022 (rule paths unowned): `{}`",
            self.line_after()
        ));
        lines.push(format!("- The code owner reviews this PR anyway: {}", self.owner_reviews));
        lines.push(format!("- ... and would after ADR-0022: {}", self.owner_reviews_after));
        lines.push(format!("- A code owner approved the head commit: {}", self.owner_approved));
        lines.push(format!("- Checks run: {ran}"));
        lines.extend(self.notes.iter().map(|note| format!("- {note}")));
        lines.extend(self.unproven_after.iter().map(|what| format!("- **Unproven:** {what}")));
        if !self.outside.is_empty() {
            lines.extend([String::new(), "Outside the rule paths:".to_owned(), String::new()]);
            lines.extend(self.outside.iter().map(|path| format!("- `{path}`")));
        }
        if !self.findings.is_empty() {
            lines.extend([String::new(), "```".to_owned()]);
            lines.extend(self.findings.iter().map(ToString::to_string));
            lines.push("```".to_owned());
        }
        lines.extend([String::new(), self.line()]);
        lines.join("\n") + "\n"
    }

    fn add(&mut self, check: Check, findings: Vec<Finding>) {
        assert!(!self.checks.contains(&check), "each check runs once");
        self.checks.push(check);
        self.findings.extend(findings);
    }

    /// Something the ratchet cannot vouch for: no verdict, unless a code owner approved the
    /// head or reviews the PR anyway.
    fn unprovable(&mut self, what: String) {
        if !self.owner_approved && !self.owner_reviews_after {
            self.unproven_after.push(what.clone());
        }
        if self.owner_approved {
            self.notes.push(format!("{what}; a code owner's approval of the head stands in."));
        } else if self.owner_reviews {
            self.notes.push(format!("{what}; the code owner's review stands in."));
        } else {
            self.unproven.push(what);
        }
    }
}

/// The changed paths, split into rule paths and the rest, whether the base's CODEOWNERS owns
/// any of them, and the fingerprint of the head tree. A head with a symbolic link is no
/// verdict.
fn scope_of(base: &Path, head: &Path) -> Result<(Vec<String>, Verdict, String), String> {
    let changed = tree::changed_paths(base, head)
        .map_err(|error| format!("cannot compare the trees: {error}"))?;
    let (rule_paths, outside): (Vec<String>, Vec<String>) =
        changed.iter().cloned().partition(|path| scope::is_rule_path(path));
    let codeowners = fs::read_to_string(base.join(CODEOWNERS))
        .map_err(|error| format!("cannot read the base's {CODEOWNERS}: {error}"))?;
    let lines = crate::pr::parse_codeowners(&codeowners)?;
    let owner_reviews = !crate::pr::protected_paths(&lines, &changed).is_empty();
    let owner_reviews_after = !crate::pr::protected_paths(&lines, &outside).is_empty();
    let (head_tree, links) =
        tree::fingerprint(head).map_err(|error| format!("cannot read the head tree: {error}"))?;
    if let Some(link) = links.first() {
        return Err(format!(
            "the head holds {} symbolic link(s), such as `{link}`; the ratchet never follows \
             links, so that observe's copy and the judge read the same regular files",
            links.len()
        ));
    }
    let verdict =
        Verdict { rule_paths, outside, owner_reviews, owner_reviews_after, ..Verdict::default() };
    Ok((changed, verdict, head_tree))
}

/// Runs the head's tools on the base's cases and records what they printed. Runs head code.
///
/// # Errors
///
/// Returns what failed: the trees cannot be read, the head's xtask does not build, or a tool
/// cannot be started.
pub fn observe(base: &Path, head: &Path, tools: &Tools<'_>) -> Result<Observations, String> {
    assert!(base.is_absolute() && head.is_absolute(), "the trees have absolute paths");
    let (changed, verdict, head_tree) = scope_of(base, head)?;
    let seed = tools.seed.unwrap_or_else(|| plant::seed_of(&head_tree));
    let corpus = observations::corpus_fingerprint(base)?.0;
    if !changed.iter().any(|path| scope::is_tooling(path)) {
        return Ok(Observations { skipped: true, seed, corpus, head: head_tree, runs: Vec::new() });
    }
    let head_xtask = match &tools.head_xtask {
        Some(binary) => binary.clone(),
        None => build_xtask(&tools.cargo, head)?,
    };
    fs::create_dir_all(&tools.work)
        .map_err(|error| format!("cannot create the work directory: {error}"))?;
    let scratch = Scratch::copy_of(head, &tools.work.join("head-tree"))
        .map_err(|error| format!("cannot copy the head tree: {error}"))?;
    let mut rng = plant::Rng::new(seed);
    let mut runs = corpus::observe(base, &scratch, &head_xtask, &mut rng)?;
    if changed.iter().any(|path| scope::is_lint_tooling(path)) {
        runs.extend(lint::observe(base, head, &scratch, &head_xtask, tools, &mut rng)?);
    }
    if tools.mutants {
        runs.extend(mutants::observe(base, head, &verdict.rule_paths, tools)?);
    }
    Ok(Observations { skipped: false, seed, corpus, head: head_tree, runs })
}

/// Judges the observations against the base's corpus and the head's source. Runs no head
/// code: the head tree is only read.
///
/// # Errors
///
/// No verdict: the trees or the observations cannot be read, the base's corpus is empty or
/// differs from the one observed, or a check cannot reach a result.
pub fn judge(
    base: &Path,
    head: &Path,
    observed: &Observations,
    tools: &Tools<'_>,
) -> Result<Verdict, String> {
    assert!(base.is_absolute() && head.is_absolute(), "the trees have absolute paths");
    let (changed, mut verdict, head_tree) = scope_of(base, head)?;
    verdict.range = tools.commits.clone().unwrap_or_else(|| "the two trees given".to_owned());
    if !changed.iter().any(|path| scope::is_tooling(path)) {
        verdict
            .notes
            .push("No change to `xtask/`, `lints/` or a rule path: nothing to prove.".to_owned());
        return Ok(verdict);
    }
    if observed.head != head_tree {
        return Err(format!(
            "the observations are of another head tree (fingerprint {}, here {head_tree}); the \
             head moved, or the base it merges with did, between the two jobs",
            observed.head
        ));
    }
    let (corpus, files) = observations::corpus_fingerprint(base)?;
    if observed.skipped || observed.corpus != corpus || files == 0 {
        return Err(format!(
            "the observations do not cover the base's corpus ({files} files, fingerprint \
             {corpus}; observed {}{}); the head job may have changed it",
            observed.corpus,
            if observed.skipped { ", skipped" } else { "" }
        ));
    }
    verdict.notes.push(format!(
        "Base corpus: {files} files, fingerprint {corpus}, placement seed {}.",
        observed.seed
    ));
    let mut rule_set = source::check(base, head)?;
    for tree in [base, head] {
        for uncovered in source::rules_without_cases(base, tree)? {
            let message = format!(
                "rule {uncovered} has code in a rule path and no firing case in the base; \
                 give it a fixture first, or move its code to the core (ADR-0022)"
            );
            rule_set.push(Finding::new(source::REGISTRY, Check::RuleSet, message));
        }
    }
    verdict.add(Check::RuleSet, rule_set);
    // Each finding names its own check; corpus and end-to-end findings come from one pass.
    verdict.add(Check::TidyCorpus, corpus::judge(base, head, observed)?);
    if changed.iter().any(|path| scope::is_lint_tooling(path)) {
        verdict.add(Check::LintCorpus, lint::judge(base, observed, tools)?);
    } else {
        verdict.notes.push("Lint checks skipped: nothing below `lints/` changed.".to_owned());
    }
    verdict.add(Check::EndToEnd, Vec::new());
    judge_proof(base, head, observed, tools, &mut verdict)?;
    verdict.findings.sort();
    Ok(verdict)
}

/// The checks that stand in for the owner's review of rule paths: mutation testing of the
/// rule-path diff, and the reviewer app. Both run only when a rule path changed.
fn judge_proof(
    base: &Path,
    head: &Path,
    observed: &Observations,
    tools: &Tools<'_>,
    verdict: &mut Verdict,
) -> Result<(), String> {
    if let Some(source) = &tools.review {
        let codeowners = fs::read_to_string(base.join(CODEOWNERS))
            .map_err(|error| format!("cannot read the base's {CODEOWNERS}: {error}"))?;
        verdict.owner_approved =
            review::owner_approved(source.reviews, source.pull, &source.head, &codeowners)?;
    }
    if verdict.rule_paths.is_empty() {
        verdict.notes.push(
            "Mutation testing and the review check skipped: no rule path changed.".to_owned(),
        );
        return Ok(());
    }
    if tools.mutants {
        let judged = mutants::judge(head, &verdict.rule_paths.clone(), observed)?;
        verdict.notes.extend(judged.notes);
        for what in judged.unproven {
            verdict.unprovable(what);
        }
        verdict.add(Check::Mutants, judged.findings);
    } else {
        verdict.unprovable("mutation testing was skipped (`--no-mutants`)".to_owned());
    }
    match &tools.review {
        Some(source) => match &source.reviewer {
            Some(reviewer) => {
                let findings = review::check(source.reviews, source.pull, &source.head, reviewer)?;
                verdict.add(Check::Review, findings);
            }
            None => verdict.unprovable(format!(
                "no reviewer app is configured (`NAMI_REVIEWER_LOGIN`, such as {}), so no review \
                 independent of the author is shown; the reviewer runs in CI (pending #59)",
                review::REVIEWER_APP
            )),
        },
        None => verdict.unprovable("the review check was skipped (`--no-review`)".to_owned()),
    }
    Ok(())
}

/// Observes and judges in one process: for local runs and the tests.
///
/// # Errors
///
/// No verdict, as for [`observe`] and [`judge`].
pub fn check(base: &Path, head: &Path, tools: &Tools<'_>) -> Result<Verdict, String> {
    let observed = observe(base, head, tools)?;
    judge(base, head, &observed, tools)
}

/// Builds the head's xtask with `cargo` and returns the binary.
fn build_xtask(cargo: &OsStr, head: &Path) -> Result<PathBuf, String> {
    let mut command = command(cargo, head);
    let _command =
        command.args(["build", "--locked", "--quiet", "--package", "xtask", "--bin", "xtask"]);
    let output = output(&mut command, "`cargo build` of the head's xtask")?;
    if !output.status.success() {
        return Err(format!(
            "the head's xtask does not build: {}",
            tail(&String::from_utf8_lossy(&output.stderr), 12)
        ));
    }
    let binary = head.join("target/debug/xtask");
    if !binary.is_file() {
        return Err(format!(
            "the head's xtask built, but not to {}; the head's cargo configuration moves the \
             target directory, so pass the binary with `--head-xtask`",
            binary.display()
        ));
    }
    Ok(binary)
}

/// A command for `program` in `directory`, with no input and without [`WITHHELD_VARIABLES`].
fn command(program: impl AsRef<OsStr>, directory: &Path) -> Command {
    assert!(directory.is_dir(), "commands run in a directory that exists");
    let mut command = Command::new(program);
    let _configured = command.current_dir(directory).stdin(Stdio::null());
    for variable in WITHHELD_VARIABLES {
        let _withheld = command.env_remove(variable);
    }
    command
}

/// Runs `command` to the end and returns its output; `what` names it in the error.
fn output(command: &mut Command, what: &str) -> Result<Output, String> {
    command.output().map_err(|error| format!("cannot run {what}: {error}"))
}

/// The last `lines` lines of `text`, joined by ` | ` so that a finding stays on one line.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> =
        text.lines().map(str::trim_end).filter(|line| !line.trim().is_empty()).collect();
    let start = all.len().saturating_sub(lines);
    all.get(start..).unwrap_or_default().join(" | ")
}

#[cfg(test)]
mod tests {
    use super::{Check, Finding, Verdict, tail};

    // Goal: the verdict line says pass, pass outside the rule paths, no verdict, or fail with
    // the count; the summary ends with it and lists the findings and what is unproven; an
    // unprovable change is a note when the owner reviews anyway.
    #[test]
    fn verdicts_print_their_outcome() {
        let mut verdict =
            Verdict { rule_paths: vec!["lints/src/a.rs".to_owned()], ..Verdict::default() };
        assert_eq!(verdict.line(), "ratchet: pass");
        verdict.outside.push("docs/TIGER_STYLE.md".to_owned());
        assert!(verdict.line().starts_with("ratchet: pass, outside the rule paths: 1"));
        verdict.unprovable("no mutant".to_owned());
        assert!(verdict.line().starts_with("ratchet: no verdict, 1 thing(s) unproven"));
        let finding = Finding::new("a.rs:3", Check::TidyCorpus, "what; fix".to_owned());
        assert_eq!(finding.to_string(), "a.rs:3: error[ratchet-tidy-corpus]: what; fix");
        verdict.findings.push(finding);
        assert_eq!(verdict.line(), "ratchet: fail, 1 finding(s)");
        let summary = verdict.summary();
        assert!(summary.ends_with("ratchet: fail, 1 finding(s)\n"), "{summary}");
        assert!(summary.contains("a.rs:3: error[ratchet-tidy-corpus]"), "{summary}");
        assert!(summary.contains("- **Unproven:** no mutant"), "{summary}");
        assert!(summary.contains("- `docs/TIGER_STYLE.md`"), "{summary}");
        let mut owned = Verdict { owner_reviews: true, ..Verdict::default() };
        owned.unprovable("no mutant".to_owned());
        assert_eq!((owned.unproven.len(), owned.notes.len()), (0, 1));
        assert_eq!(owned.line(), "ratchet: pass");
        assert!(owned.line_after().starts_with("ratchet: no verdict, 1"), "unowned after the flip");
        let shown = owned.summary();
        assert!(shown.contains("- Verdict today (every guardrail path owned): `ratchet: pass`"));
        assert!(shown.contains("after ADR-0022 (rule paths unowned): `ratchet: no verdict"));
        assert!(shown.ends_with("\nratchet: pass\n"), "the exit code follows today's verdict");
        let mut still = Verdict { owner_reviews: true, owner_reviews_after: true, ..owned };
        still.unprovable("no reviewer".to_owned());
        assert_eq!(still.unproven_after.len(), 1, "only the earlier item; the owner still reviews");
        let mut approved = Verdict { owner_approved: true, ..Verdict::default() };
        approved.unprovable("no mutant".to_owned());
        assert!(approved.unproven.is_empty() && approved.unproven_after.is_empty());
    }

    // Goal: the tail keeps the last non-blank lines on one line.
    #[test]
    fn tails_keep_the_last_lines() {
        assert_eq!(tail("a\n\nb\nc\n", 2), "b | c");
        assert_eq!(tail("a\n", 5), "a");
        assert_eq!(tail("", 3), "");
    }
}
