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
//! **Two classes.** Each rule-path diff is additive or an edit ([`class`]). An additive diff
//! adds rules and leaves every existing item as it is; it merges on the ratchet's proof alone,
//! with no review by the reviewer app needed, though none of the app's requests for changes or
//! threads may be open. An edit changes existing rule code; every check still runs, and the
//! review gate also needs the reviewer app's approval of the head commit, or until the app
//! exists (#59) a code owner's. The reason: every honest loosening that the review of #57 found
//! edited existing rule code together with its tests, which mutation testing cannot see, and
//! the independent review caught each one.
//!
//! **Threat model.** The ratchet is sound against honest mistakes, within what it checks. Its
//! `syn` checks are sound for files in the dialect of [`dialect`], and a rule-path file outside
//! it gives no verdict: for those files, the rule tables, scopes, allowed lists, the presence of
//! each `check_*` call site, and the lints' levels and registrations are what the compiler
//! builds. For an additive diff, the existing rule code is the base's, token for token, and
//! the new code can neither shadow it nor reach its state; that the new rules fire as their
//! cases say, the corpus, end to end and mutation testing of the new code show. Residual risk
//! there: new code that misbehaves through a path the class does not see, such as interior
//! mutability behind a shared reference, and a new rule that fires too little, which only
//! makes the guardrails less strict than they could be, never less strict than the base. For
//! an edit, soundness rests on the independent reviewer; the checks catch what they can (a
//! removed row or call, a case that no longer fires where the corpus plants it, a missed
//! mutant), and a narrowing that neither the corpus nor the edited tests meet is the residual
//! risk the reviewer carries.
//! Against head code written to deceive, the rest is defence in depth only; the reviewer app,
//! sampled audits and the trusted core back it. The reviewer app is the bot that
//! `NAMI_REVIEWER_LOGIN` names, and the review it posts runs in CI (pending #59), where no
//! authoring session holds its key.
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
//! change the ratchet cannot vouch for (no mutant, no reviewer app, an edit
//! without its approval) where no code owner reviews.

pub mod class;
pub mod cli;
mod corpus;
pub mod dialect;
pub mod hunks;
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

/// Where the frozen copies of rule-path files live, for the ratchet's own tests.
const FROZEN: &str = "xtask/fixtures/ratchet";

/// The frozen copy of the rule-path file `relative`: `xtask/fixtures/ratchet/<relative>.frozen`,
/// a snapshot that the ratchet's own tests read instead of the live file. The tests edit their
/// inputs by exact text, so on the live files an additive change, such as a dispatch call at
/// the tail of `text::check`, would break them, and the ratchet could never vouch for it
/// (review of #57, round 8). Only `the_rule_paths_are_in_the_dialect` and the invariant
/// `every_rule_in_a_rule_path_has_a_case` read the live tree, and neither anchors on text.
#[must_use]
pub fn frozen(relative: &str) -> String {
    assert!(!relative.starts_with('/'), "a frozen copy is named by a relative path");
    let path = crate::workspace_root().join(FROZEN).join(format!("{relative}.frozen"));
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("the frozen copy {}: {error}", path.display()));
    assert!(!text.is_empty(), "a frozen copy holds the file's text");
    text
}

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
    /// The seed of the placements; `None` derives it from the base's corpus and the head
    /// ([`plant::seed_of`]).
    pub seed: Option<u64>,
    /// The commits judged, as `base..head`, for the summary; `None` when only trees are given.
    pub commits: Option<String>,
    /// The pull request's head commit, which seeds the placements; `None` seeds them from the
    /// head tree.
    pub pr_head: Option<String>,
    /// The commit whose tree `observe` copies, such as GitHub's test merge, recorded so that a
    /// later judge can check it judges the same commit.
    pub commit: Option<String>,
}

impl Tools<'_> {
    /// The seed of the placements for the base corpus `corpus` and the head tree `head_tree`.
    fn seed(&self, corpus: &str, head_tree: &str) -> u64 {
        let head = self.pr_head.as_deref().unwrap_or(head_tree);
        self.seed.unwrap_or_else(|| plant::seed_of(corpus, head))
    }
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
    /// Whether the rule-path diff is additive or edits existing rule code; `None` when no rule
    /// path changed.
    pub class: Option<class::Class>,
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

    /// The verdict line with `unproven`, naming the class when a rule path changed:
    /// `ratchet: pass (additive)`, `ratchet: no verdict (edit), ..`.
    fn line_with(&self, unproven: &[String]) -> String {
        let class =
            self.class.as_ref().map_or(String::new(), |class| format!(" ({})", class.name()));
        if !self.findings.is_empty() {
            format!("ratchet: fail{class}, {} finding(s)", self.findings.len())
        } else if !unproven.is_empty() {
            format!(
                "ratchet: no verdict{class}, {} thing(s) unproven; the code owner decides",
                unproven.len()
            )
        } else if self.outside.is_empty() {
            format!("ratchet: pass{class}")
        } else {
            format!(
                "ratchet: pass{class}, outside the rule paths: {} changed path(s) that \
                 CODEOWNERS decides on",
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
        lines.push(
            "- Both verdicts count the reviews as they are now; the latest run for this head, \
             usually the one after the reviewer's review, is the evidence."
                .to_owned(),
        );
        lines.push(format!("- The code owner reviews this PR anyway: {}", self.owner_reviews));
        lines.push(format!("- ... and would after ADR-0022: {}", self.owner_reviews_after));
        lines.push(format!("- A code owner approved the head commit: {}", self.owner_approved));
        lines.push(format!("- Checks run: {ran}"));
        if let Some(class) = &self.class {
            lines.push(format!(
                "- Class: {} ({} change(s) to existing rule code)",
                class.name(),
                class.edits.len()
            ));
            lines.extend(class.edits.iter().map(|edit| format!("- **Edit:** {edit}")));
        }
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
    let flipped = crate::pr::parse_codeowners(&scope::flipped_codeowners(&codeowners))?;
    let owner_reviews_after = !crate::pr::protected_paths(&flipped, &changed).is_empty();
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
    let corpus = observations::corpus_fingerprint(base)?.0;
    let mut observed = Observations {
        skipped: true,
        seed: tools.seed(&corpus, &head_tree),
        corpus,
        head: head_tree,
        commit: tools.commit.clone().unwrap_or_default(),
        pr_head: tools.pr_head.clone().unwrap_or_default(),
        runs: Vec::new(),
    };
    if !changed.iter().any(|path| scope::is_tooling(path)) {
        return Ok(observed);
    }
    let head_xtask = match &tools.head_xtask {
        Some(binary) => binary.clone(),
        None => build_xtask(&tools.cargo, head)?,
    };
    fs::create_dir_all(&tools.work)
        .map_err(|error| format!("cannot create the work directory: {error}"))?;
    let scratch = Scratch::copy_of(head, &tools.work.join("head-tree"))
        .map_err(|error| format!("cannot copy the head tree: {error}"))?;
    let mut rng = plant::Rng::new(observed.seed);
    observed.runs = corpus::observe(base, &scratch, &head_xtask, &mut rng)?;
    if changed.iter().any(|path| scope::is_lint_tooling(path)) {
        observed.runs.extend(lint::observe(base, head, &scratch, &head_xtask, tools, &mut rng)?);
    }
    if tools.mutants {
        observed.runs.extend(mutants::observe(base, head, &verdict.rule_paths, tools)?);
    }
    observed.skipped = false;
    Ok(observed)
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
    same_commits(observed, tools)?;
    if observed.seed != tools.seed(&corpus, &head_tree) {
        return Err(format!(
            "the observations were drawn with the seed {}, not the one this base and head give; \
             the placements cannot be checked",
            observed.seed
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
    let mut rng = plant::Rng::new(observed.seed);
    verdict.add(Check::TidyCorpus, corpus::judge(base, head, observed, &mut rng)?);
    if changed.iter().any(|path| scope::is_lint_tooling(path)) {
        verdict.add(Check::LintCorpus, lint::judge(base, head, observed, tools, &mut rng)?);
    } else {
        verdict.notes.push("Lint checks skipped: nothing below `lints/` changed.".to_owned());
    }
    verdict.add(Check::EndToEnd, Vec::new());
    judge_proof(base, head, observed, tools, &mut verdict)?;
    verdict.findings.sort();
    Ok(verdict)
}

/// Fails, for no verdict, if the observations name another commit than the one judged, or
/// another pull request head.
fn same_commits(observed: &Observations, tools: &Tools<'_>) -> Result<(), String> {
    let judged =
        tools.commits.as_deref().and_then(|range| range.split_once("..")).map(|(_, head)| head);
    if let Some(judged) = judged
        && !observed.commit.is_empty()
        && observed.commit != judged
    {
        return Err(format!(
            "the observations are of the commit {}, and this judges {judged}; judge the commit \
             that was observed",
            observed.commit
        ));
    }
    let pr_head =
        tools.review.as_ref().map(|source| source.head.as_str()).or(tools.pr_head.as_deref());
    if let Some(pr_head) = pr_head
        && !observed.pr_head.is_empty()
        && observed.pr_head != pr_head
    {
        return Err(format!(
            "the observations are of the pull request head {}, and this judges {pr_head}",
            observed.pr_head
        ));
    }
    Ok(())
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
    let class = class::classify(base, head, &verdict.rule_paths)?;
    // What mutation testing could not show. For an edit, the reviewer app's approval stands in
    // for it: a test edited with the code it tests pins the edit, so mutants prove little there.
    let mut uncovered = Vec::new();
    if tools.mutants {
        let rule_paths = verdict.rule_paths.clone();
        let judged = mutants::judge(base, head, &rule_paths, observed, &tools.work, &class)?;
        verdict.notes.extend(judged.notes);
        uncovered = judged.unproven;
        verdict.add(Check::Mutants, judged.findings);
    } else {
        verdict.unprovable("mutation testing was skipped (`--no-mutants`)".to_owned());
    }
    let approved = review_gate(tools, verdict)?;
    if class.is_edit() && approved {
        verdict.notes.push(format!(
            "An edit to existing rule code: the reviewer app approved the head commit, which \
             stands in for the {} hunk(s) without a mutant of their own.",
            uncovered.len()
        ));
    } else {
        if let Some(what) = unapproved_edit(&class) {
            verdict.unprovable(what);
        }
        for what in uncovered {
            verdict.unprovable(what);
        }
    }
    verdict.class = Some(class);
    Ok(())
}

/// What an edit to existing rule code leaves unproven without the reviewer app's approval: the
/// first edit, and how many more the summary lists. `None` for an additive diff.
fn unapproved_edit(class: &class::Class) -> Option<String> {
    let (first, rest) = class.edits.split_first()?;
    let more = if rest.is_empty() { String::new() } else { format!(" (and {} more)", rest.len()) };
    Some(format!(
        "edit to existing rule code needs the reviewer app's approval or a code owner's: \
         {first}{more}"
    ))
}

/// The review gate ([`review::check`]): its findings go into `verdict`. An additive diff needs
/// nothing more, so a head that the reviewer app has not reviewed, or a repository without the
/// app, is a note. Returns true if the app approved the head commit with no finding, which an
/// edit to existing rule code needs.
fn review_gate(tools: &Tools<'_>, verdict: &mut Verdict) -> Result<bool, String> {
    let Some(source) = &tools.review else {
        verdict.unprovable(
            "the review check was skipped (`--no-review`), so a request for changes or an open \
             thread by the reviewer app is not seen"
                .to_owned(),
        );
        return Ok(false);
    };
    let Some(reviewer) = &source.reviewer else {
        verdict.notes.push(format!(
            "No reviewer app is configured (`NAMI_REVIEWER_LOGIN`, such as {}; it runs in CI, \
             #59). An additive diff needs none; an edit needs a code owner's approval.",
            review::REVIEWER_APP
        ));
        return Ok(false);
    };
    let gate = review::check(source.reviews, source.pull, &source.head, reviewer)?;
    let clean = gate.findings.is_empty();
    verdict.notes.push(format!(
        "{reviewer} {} the head commit.",
        match (gate.approved, gate.reviewed) {
            (true, _) => "approved",
            (false, true) => "reviewed, without approving,",
            (false, false) => "has not reviewed",
        }
    ));
    verdict.add(Check::Review, gate.findings);
    Ok(gate.approved && clean)
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
    use super::{Check, Finding, Tools, Verdict, same_commits, tail};
    use crate::ratchet::observations::Observations;

    fn tools(pr_head: Option<&str>, commits: Option<&str>) -> Tools<'static> {
        Tools {
            head_xtask: None,
            cargo: "cargo".into(),
            work: "/w".into(),
            mutants: false,
            review: None,
            seed: None,
            commits: commits.map(str::to_owned),
            pr_head: pr_head.map(str::to_owned),
            commit: None,
        }
    }

    // Goal: the seed comes from the base corpus and the PR head, or the head tree without one,
    // unless `--seed` gives it.
    #[test]
    fn the_seed_follows_the_base_and_the_head() {
        use crate::ratchet::plant::seed_of;
        assert_eq!(tools(Some("p"), None).seed("c", "t"), seed_of("c", "p"));
        assert_eq!(tools(None, None).seed("c", "t"), seed_of("c", "t"));
        assert_ne!(tools(Some("p"), None).seed("c", "t"), tools(Some("q"), None).seed("c", "t"));
        let fixed = Tools { seed: Some(3), ..tools(Some("p"), None) };
        assert_eq!(fixed.seed("c", "t"), 3);
    }

    // Goal: observations of another commit or another PR head are no verdict; observations
    // that record neither, or the same ones, are judged.
    #[test]
    fn observations_must_be_of_the_commit_judged() {
        let observed = Observations {
            commit: "h".to_owned(),
            pr_head: "p".to_owned(),
            ..Observations::default()
        };
        assert_eq!(same_commits(&observed, &tools(Some("p"), Some("b..h"))), Ok(()));
        let other_commit = same_commits(&observed, &tools(Some("p"), Some("b..x")));
        assert!(other_commit.unwrap_err().contains("judge the commit"));
        let other_head = same_commits(&observed, &tools(Some("q"), Some("b..h")));
        assert!(other_head.unwrap_err().contains("pull request head"));
        let unrecorded = Observations::default();
        assert_eq!(same_commits(&unrecorded, &tools(Some("q"), Some("b..x"))), Ok(()));
    }

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

    // Goal: an edit leaves its first change unproven, counting the rest; an additive diff leaves
    // nothing; and the verdict lines name the class.
    #[test]
    fn an_unapproved_edit_names_its_first_change() {
        use super::class::Class;
        assert_eq!(super::unapproved_edit(&Class::default()), None);
        let one = Class { edits: vec!["a.rs: x".to_owned()], ..Class::default() };
        let needs =
            "edit to existing rule code needs the reviewer app's approval or a code owner's";
        assert_eq!(super::unapproved_edit(&one), Some(format!("{needs}: a.rs: x")));
        let three = Class { edits: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()], ..one };
        assert_eq!(super::unapproved_edit(&three), Some(format!("{needs}: a (and 2 more)")));
        let verdict = Verdict { class: Some(three), ..Verdict::default() };
        assert_eq!(
            (verdict.line(), verdict.line_after()),
            ("ratchet: pass (edit)".to_owned(), "ratchet: pass (edit)".to_owned())
        );
        let additive = Verdict { class: Some(Class::default()), ..Verdict::default() };
        assert_eq!(additive.line(), "ratchet: pass (additive)");
    }

    // Goal: the tail keeps the last non-blank lines on one line.
    #[test]
    fn tails_keep_the_last_lines() {
        assert_eq!(tail("a\n\nb\nc\n", 2), "b | c");
        assert_eq!(tail("a\n", 5), "a");
        assert_eq!(tail("", 3), "");
    }
}
