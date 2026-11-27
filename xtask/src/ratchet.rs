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
//! **One tree.** The observer and the judge read the same head: the observer records the head's
//! fingerprint, which the judge checks, and both leave symbolic links out of what they plant and
//! mutate, since the observer's scratch copy holds regular files only. The judge refuses a head
//! with a link, a form it cannot vouch for, and still judges what was observed of it.
//!
//! **Settling.** A code owner's approval of the head settles a no verdict only when each reason
//! is unprovable though the evidence is complete ([`Reason::settleable`]); missing or broken
//! evidence is fixed by fixing the PR or re-running the job, never settled (review of #57,
//! round 14).
//!
//! **Output.** Each finding is one line on stderr, `place: error[ratchet-<check>]: what; fix`.
//! A Markdown summary goes to stdout; its last line is the verdict.
//!
//! **Exit code.** 0: pass, also when paths outside the rule paths changed (`pass, outside the
//! rule paths`: CODEOWNERS decides on those). 1: fail, at least one finding. 2: no verdict, in
//! three kinds:
//! - in all cases, whoever reviews or approved: bad arguments, a tool that could not run or
//!   answer, an output or observation that cannot be read or is of other trees, a skipped
//!   check, a failed null-mutant run, or a merge group whose rule-path diff is not the carried
//!   one. The fix is a change to the PR or a re-run, never an approval;
//! - until a code owner approves the head commit: a form the ratchet refuses, such as a file
//!   outside the dialect, a symbolic link or a name that is not valid UTF-8;
//! - where no code owner reviews or has approved the head commit: something the ratchet cannot
//!   vouch for though its evidence is complete, a hunk without a mutant, an edit without the
//!   reviewer app's approval, or a build file.

pub mod canonical;
pub mod carry;
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

/// Why a step that reads the observations skips: there are none of these trees.
const NO_OBSERVATIONS: &str = "no observations of these trees; the observations step says why";

/// Where the frozen copies of rule-path files live, for the ratchet's own tests.
const FROZEN: &str = "xtask/fixtures/ratchet";

/// The frozen copy of the rule-path file `relative`: `xtask/fixtures/ratchet/<relative>.frozen`,
/// a snapshot that the ratchet's own tests read instead of the live file. The tests edit their
/// inputs by exact text, so on the live files an additive change, such as a dispatch call at
/// the tail of `text::check`, would break them, and the ratchet could never vouch for it
/// (review of #57, round 8). Only the guard `the_rule_paths_are_in_the_dialect` reads the live
/// rule files, and it anchors on no text. The canonical end-to-end tests copy the live tree on
/// purpose, and are run by hand: they apply the diffs of [`canonical`] to the frozen copies,
/// and stop with what to update if a live file they edit no longer equals its copy.
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

/// A step of the judge, and the phase of `observe` that gathers its evidence. Every step has
/// the form `fn step(.., verdict: &mut Verdict)` and returns nothing: it records each finding
/// into the verdict the moment it is known, and a failure as [`Verdict::fail`], so that no `?`
/// can cross a step and drop what another step found (review of #57, round 13). Each phase of
/// `observe` records its failure in the observations and the others go on, and the judge's
/// step fails on it (round 14). The fault-injection test plants failures and findings at the
/// steps' real read and record points ([`Faults`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// The changed paths, CODEOWNERS and the head tree's fingerprint.
    Scope,
    /// The rule tables, check calls, lint declarations and cases, read as data.
    RuleSet,
    /// Whether a code owner approved the head commit.
    OwnerApproval,
    /// That the observations are of these trees.
    Observations,
    /// The tidy corpus, with end to end.
    TidyCorpus,
    /// The lint corpus's cases and the base library's expectations.
    LintCorpus,
    /// The head's lint library on the base's cases.
    LintLibrary,
    /// The head's `xtask dylint` on each placed case.
    LintCases,
    /// The class of the rule-path diff.
    Classify,
    /// The carried mutation evidence of a merge group.
    Carry,
    /// The mutation run of a workspace.
    Mutants,
    /// The null-mutant run of a workspace.
    NullRun,
    /// The review gate: the reviewer app's reviews, dismissals and threads.
    ReviewGate,
}

/// Every step, in the order the judge runs them.
pub const STEPS: [Step; 13] = [
    Step::Scope,
    Step::RuleSet,
    Step::OwnerApproval,
    Step::Observations,
    Step::TidyCorpus,
    Step::LintCorpus,
    Step::LintLibrary,
    Step::LintCases,
    Step::Classify,
    Step::Carry,
    Step::Mutants,
    Step::NullRun,
    Step::ReviewGate,
];

impl Step {
    /// The step's name, as failures name it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Scope => "scope",
            Self::RuleSet => "rule set",
            Self::OwnerApproval => "owner approval",
            Self::Observations => "observations",
            Self::TidyCorpus => "tidy corpus",
            Self::LintCorpus => "lint corpus",
            Self::LintLibrary => "lint library",
            Self::LintCases => "lint cases",
            Self::Classify => "classify",
            Self::Carry => "carried evidence",
            Self::Mutants => "mutants",
            Self::NullRun => "null mutant",
            Self::ReviewGate => "review gate",
        }
    }

    /// The step named `name`, as [`Step::name`] gives it.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        STEPS.into_iter().find(|step| step.name() == name)
    }
}

/// Faults that the fault-injection test plants; the command line never sets any. They act at
/// the steps' real points: `find` records a finding where its step reaches its body
/// ([`Verdict::reach`]); `fail` makes a read of its step fail where the step reads its input
/// ([`Verdict::inject`]), the first read after that finding if the step also finds, so that the
/// step finds and then fails; and `fail_observing` makes that phase of `observe` fail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// The step whose read fails.
    pub fail: Option<Step>,
    /// The step that finds something.
    pub find: Option<Step>,
    /// The phase of `observe` that fails.
    pub fail_observing: Option<Step>,
    /// The kind `fail` is recorded as, whatever the step's own error path says: `None` keeps
    /// the step's kind, mostly [`Reason::Evidence`]; [`Reason::Refused`] tests the settleable
    /// path.
    pub fail_as: Option<Reason>,
}

/// Why the ratchet could not reach a verdict on something, and whether a code owner's approval
/// of the head settles it. Only what is unprovable though the evidence is complete is settled;
/// evidence that is missing, unreadable or mismatched, or a tool that failed, never is: the PR
/// is fixed or the job re-run (review of #57, round 14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Evidence that is missing, unreadable or mismatched, a check that was skipped, or a tool
    /// that failed or was killed. What [`Verdict::fail`] records.
    Evidence,
    /// An edit to existing rule code without the reviewer app's approval of the head.
    UnapprovedEdit,
    /// A changed hunk, or a workspace's change, without a mutant of its own.
    NoMutant,
    /// A manifest or build file in the rule paths.
    BuildFile,
    /// A form the ratchet refuses to read: a file outside the dialect, a symbolic link, a diff
    /// the class does not read.
    Refused,
    /// A merge group whose rule-path diff differs from the one the carried evidence mutated.
    /// Never settled: no approval merges rule text that was not mutation-tested in that exact
    /// form; the PR is rebased and pushed, so that it is observed on its new base (maintainer's
    /// priority, review of #57, round 15).
    CarriedDiff,
}

impl Reason {
    /// True if a code owner's approval of the head settles it. Every reason is named here, so
    /// a new one does not compile until it is placed.
    #[must_use]
    pub fn settleable(self) -> bool {
        match self {
            Self::Evidence | Self::CarriedDiff => false,
            Self::UnapprovedEdit | Self::NoMutant | Self::BuildFile | Self::Refused => true,
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
    /// For a merge group: the mutation evidence of the pull request's own observation, which
    /// the judge reads instead of the observations' when the rule-path diffs are identical
    /// ([`carry`]), or why it could not be read; `None` reads the observations'.
    pub carried: Option<Result<carry::Carried, String>>,
    /// Faults for the fault-injection test; [`Faults::default`] everywhere else.
    pub faults: Faults,
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
    /// Why a step could not reach a result: no verdict. A step that fails adds its reason here
    /// and the judge goes on, so that no failure drops a finding of an earlier or later step
    /// (review of #57, round 11). Only [`settled`] turns these into notes, and only when there
    /// is no finding and each is settleable ([`Reason::settleable`]).
    pub failed: Vec<String>,
    /// Every step that the judge entered, in order.
    pub visited: Vec<Step>,
    /// Every step that reached its body with its inputs ([`Verdict::reach`]).
    pub reached: Vec<Step>,
    /// Every step that returned before its body, with why: its inputs are missing, which a
    /// failure of another step says, or it does not apply to this diff.
    pub skipped: Vec<(Step, String)>,
    /// The steps that could not reach a result, each once.
    broken: Vec<Step>,
    /// Every failure and unproven item that no approval settles ([`Reason::settleable`]).
    unsettled: Vec<String>,
    /// Every changed path, rule path or not; empty if the scope could not be read.
    changed: Vec<String>,
    /// The fingerprint of the head tree; empty if the scope could not be read.
    head_tree: String,
    /// What the steps found out that later steps read.
    progress: Progress,
    /// The hunks without a mutant of their own, which an approved edit needs none for.
    uncovered: Vec<String>,
}

/// What the steps of a verdict found out that later steps read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Progress {
    /// True once the changed paths are known.
    scoped: bool,
    /// True once the observations are known to be of these trees.
    observed: bool,
    /// True if the reviewer app approved the head commit, and the review gate read everything
    /// and found nothing.
    app_approved: bool,
    /// The kind of the planted failure that the step records next ([`Faults::fail_as`]).
    injected_as: Option<Reason>,
}

impl Verdict {
    /// Records that `step` could not reach a result, for `reason`: no verdict, beside whatever
    /// the other steps found. Its evidence is missing or broken ([`Reason::Evidence`]), which no
    /// approval settles.
    pub fn fail(&mut self, step: Step, reason: impl fmt::Display) {
        self.fail_for(step, Reason::Evidence, reason);
    }

    /// Records that `step` could not reach a result, for `reason`, of the kind `why`.
    pub fn fail_for(&mut self, step: Step, why: Reason, reason: impl fmt::Display) {
        let why = self.progress.injected_as.take().unwrap_or(why);
        let reason = reason.to_string();
        assert!(!reason.is_empty(), "a failure says why");
        let line = format!("{}: {reason}", step.name());
        if !why.settleable() {
            self.unsettled.push(line.clone());
        }
        self.failed.push(line);
        if !self.broken.contains(&step) {
            self.broken.push(step);
        }
    }

    /// True if a code owner's approval may settle this verdict's failures and unproven items:
    /// each is unprovable though the evidence is complete ([`Reason::settleable`]).
    #[must_use]
    pub fn is_settleable(&self) -> bool {
        self.unsettled.is_empty()
    }

    /// True if `step` could not reach a result.
    #[must_use]
    pub fn has_failed(&self, step: Step) -> bool {
        self.broken.contains(&step)
    }

    /// Records a finding.
    fn record(&mut self, finding: Finding) {
        self.ran(finding.check);
        self.findings.push(finding);
    }

    /// Records that `check` ran, once.
    fn ran(&mut self, check: Check) {
        if !self.checks.contains(&check) {
            self.checks.push(check);
        }
    }

    /// Enters `step`: records the visit.
    fn enter(&mut self, step: Step) {
        if !self.visited.contains(&step) {
            self.visited.push(step);
        }
    }

    /// Records that `step` reached its body with its inputs, where it records what it finds. A
    /// finding that `tools` plants for the step is recorded here.
    fn reach(&mut self, step: Step, tools: &Tools<'_>) {
        if self.reached.contains(&step) {
            return;
        }
        self.reached.push(step);
        if tools.faults.find == Some(step) {
            let message = format!("an injected finding of the step `{}`", step.name());
            let place = format!("injected/{}", step.name());
            self.record(Finding::new(&place, Check::RuleSet, message));
        }
    }

    /// Records that `step` returns before its body, for `why`.
    fn skip(&mut self, step: Step, why: &str) {
        assert!(!self.reached.contains(&step), "a step that reached its body did not skip it");
        self.skipped.push((step, why.to_owned()));
    }

    /// What `step` read, as `read`, unless `tools` plants a failure of the step here: then an
    /// error, which the step handles as it handles a real one. A step that also finds fails
    /// only once it has reached its body, so that it finds and then fails.
    fn inject<T, E: fmt::Display>(
        &mut self,
        step: Step,
        tools: &Tools<'_>,
        read: Result<T, E>,
    ) -> Result<T, String> {
        let faults = tools.faults;
        let after_finding = faults.find != Some(step) || self.reached.contains(&step);
        if faults.fail == Some(step) && after_finding {
            self.progress.injected_as = faults.fail_as;
            return Err("an injected failure".to_owned());
        }
        read.map_err(|error| error.to_string())
    }

    /// True, having recorded the failure, if `tools` plants a failure of `step` at the read
    /// that comes next, as [`Verdict::inject`] decides.
    fn faulted(&mut self, step: Step, tools: &Tools<'_>) -> bool {
        match self.inject(step, tools, Ok::<(), String>(())) {
            Ok(()) => false,
            Err(reason) => {
                self.fail(step, reason);
                true
            }
        }
    }

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
        // Missing or broken evidence is no question for the code owner (review of #57, round
        // 15): only what is settleable is theirs to decide.
        let remedy =
            if self.is_settleable() { "the code owner decides" } else { "fix the PR or re-run" };
        if !self.findings.is_empty() {
            format!("ratchet: fail{class}, {} finding(s)", self.findings.len())
        } else if !self.failed.is_empty() {
            format!(
                "ratchet: no verdict{class}, {} step(s) could not be judged; {remedy}",
                self.failed.len()
            )
        } else if !unproven.is_empty() {
            format!("ratchet: no verdict{class}, {} thing(s) unproven; {remedy}", unproven.len())
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
        lines.extend(self.failed.iter().map(|why| format!("- **Not judged (no verdict):** {why}")));
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

    /// Something the ratchet cannot vouch for, of the kind `why`: no verdict, unless a code
    /// owner approved the head or reviews the PR anyway and `why` is settleable. Evidence that
    /// is missing, such as a skipped check, stays unproven whoever approved.
    fn unprovable(&mut self, why: Reason, what: String) {
        if !why.settleable() {
            self.unsettled.push(what.clone());
            self.unproven_after.push(what.clone());
            self.unproven.push(what);
            return;
        }
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

/// The step [`Step::Scope`]: the changed paths, split into rule paths and the rest, whether
/// the base's CODEOWNERS owns any of them, and the fingerprint of the head tree. A part that
/// cannot be read fails the step and leaves the others to be read. A head with a symbolic link
/// is a form the ratchet refuses ([`Reason::Refused`]); its fingerprint still counts, so that
/// what was observed of it is judged, and its findings fail it.
fn scope_step(base: &Path, head: &Path, tools: &Tools<'_>, verdict: &mut Verdict) {
    verdict.enter(Step::Scope);
    verdict.reach(Step::Scope, tools);
    match verdict.inject(Step::Scope, tools, tree::changed_paths(base, head)) {
        Ok(changed) => {
            let (rule_paths, outside): (Vec<String>, Vec<String>) =
                changed.iter().cloned().partition(|path| scope::is_rule_path(path));
            (verdict.rule_paths, verdict.outside, verdict.changed) = (rule_paths, outside, changed);
            verdict.progress.scoped = true;
        }
        Err(error) => verdict.fail(Step::Scope, format!("cannot compare the trees: {error}")),
    }
    match owners_of(base, &verdict.changed) {
        Ok(owned) => (verdict.owner_reviews, verdict.owner_reviews_after) = owned,
        Err(reason) => verdict.fail(Step::Scope, reason),
    }
    match tree::fingerprint(head) {
        Ok((head_tree, links)) => {
            verdict.head_tree = head_tree;
            if let Some(link) = links.first() {
                let reason = format!(
                    "the head holds {} symbolic link(s), such as `{link}`; the ratchet never \
                     follows links, and the compiler would",
                    links.len()
                );
                verdict.fail_for(Step::Scope, Reason::Refused, reason);
            }
        }
        Err(error) => verdict.fail(Step::Scope, format!("cannot read the head tree: {error}")),
    }
    // A name that is not valid UTF-8 has no text that every reader agrees on: it is refused
    // before anything converts it, and no step reads it (review of #57, round 15).
    match tree::refused(head) {
        Ok(refused) => {
            for (path, why) in refused {
                let reason = format!(
                    "the head's path `{path}` {why}, which the ratchet cannot name soundly; \
                     rename it"
                );
                verdict.fail_for(Step::Scope, Reason::Refused, reason);
            }
        }
        Err(error) => verdict.fail(Step::Scope, format!("cannot read the head tree: {error}")),
    }
    assert_eq!(
        verdict.rule_paths.len() + verdict.outside.len(),
        verdict.changed.len(),
        "every changed path is a rule path or outside them"
    );
}

/// Whether the base's CODEOWNERS owns a path of `changed`: today, and once the rule paths are
/// unowned (ADR-0022).
///
/// # Errors
///
/// The base's CODEOWNERS cannot be read or parsed.
fn owners_of(base: &Path, changed: &[String]) -> Result<(bool, bool), String> {
    let codeowners = fs::read_to_string(base.join(CODEOWNERS))
        .map_err(|error| format!("cannot read the base's {CODEOWNERS}: {error}"))?;
    let lines = crate::pr::parse_codeowners(&codeowners)?;
    let flipped = crate::pr::parse_codeowners(&scope::flipped_codeowners(&codeowners))?;
    let now = !crate::pr::protected_paths(&lines, changed).is_empty();
    let after = !crate::pr::protected_paths(&flipped, changed).is_empty();
    assert!(now || !after, "unowning the rule paths never adds an owner");
    Ok((now, after))
}

/// Runs the head's tools on the base's cases and records what they printed. Runs head code.
/// Each phase that fails records why in the observations, as [`Observations::failures`], and
/// the others go on, so that a failure the head chooses never drops what was already observed
/// (review of #57, round 14): the fingerprints, the tidy corpus with end to end, the lint
/// library, the lint cases, and the mutation and null-mutant runs of each workspace. The
/// judge's step for a phase fails on its recorded failure.
#[must_use]
pub fn observe(base: &Path, head: &Path, tools: &Tools<'_>) -> Observations {
    assert!(base.is_absolute() && head.is_absolute(), "the trees have absolute paths");
    let mut scoped = Verdict::default();
    scope_step(base, head, tools, &mut scoped);
    let mut observed = Observations {
        skipped: true,
        commit: tools.commit.clone().unwrap_or_default(),
        pr_head: tools.pr_head.clone().unwrap_or_default(),
        ..Observations::default()
    };
    let fingerprinted = observations::corpus_fingerprint(base).and_then(|(corpus, _)| {
        let head_tree = Some(scoped.head_tree.clone()).filter(|tree| !tree.is_empty());
        Ok((corpus, head_tree.ok_or("cannot read the head tree")?))
    });
    match observing(tools, Step::Observations, fingerprinted) {
        Ok((corpus, head_tree)) => (observed.corpus, observed.head) = (corpus, head_tree),
        Err(reason) => {
            // Without the fingerprints no seed can be drawn, and the judge refuses observations
            // of trees it cannot check, so nothing more is observed; the failure is recorded,
            // never asserted (review of #57, round 15).
            observed.fail(Step::Observations, reason);
            return observed;
        }
    }
    observed.seed = tools.seed(&observed.corpus, &observed.head);
    let known = scoped.progress.scoped;
    let touches = |is: fn(&str) -> bool| !known || scoped.changed.iter().any(|path| is(path));
    if !touches(scope::is_tooling) {
        return observed;
    }
    observed.skipped = false;
    let head_xtask = match &tools.head_xtask {
        Some(binary) => Ok(binary.clone()),
        None => build_xtask(&tools.cargo, head),
    };
    let scratch = fs::create_dir_all(&tools.work)
        .map_err(|error| format!("cannot create the work directory: {error}"))
        .and_then(|()| {
            Scratch::copy_of(head, &tools.work.join("head-tree"))
                .map_err(|error| format!("cannot copy the head tree: {error}"))
        });
    let ran = head_xtask.as_deref().map_err(Clone::clone);
    let planted = scratch.as_ref().map_err(Clone::clone);
    corpus::observe(base, planted.clone(), ran.clone(), tools, &mut observed);
    if touches(scope::is_lint_tooling) {
        lint::observe(base, head, planted, ran, tools, &mut observed);
    }
    if tools.mutants {
        mutants::observe(base, head, &scoped.rule_paths, tools, &mut observed);
    }
    observed
}

/// What a phase of `observe` read, as `read`, unless `tools` plants a failure of the phase
/// `step`: then an error, which the phase records as it records a real one.
fn observing<T, E: fmt::Display>(
    tools: &Tools<'_>,
    step: Step,
    read: Result<T, E>,
) -> Result<T, String> {
    if tools.faults.fail_observing == Some(step) {
        return Err("an injected failure".to_owned());
    }
    read.map_err(|error| error.to_string())
}

/// Judges the observations against the base's corpus and the head's source. Runs no head
/// code: the head tree is only read.
///
/// # Errors
///
/// No verdict, naming every step that could not reach a result, when one could not and
/// nothing was found. A verdict with a finding is a verdict even then: it fails, and its
/// summary lists the steps that were not judged ([`judge_all`]).
pub fn judge(
    base: &Path,
    head: &Path,
    observed: &Observations,
    tools: &Tools<'_>,
) -> Result<Verdict, String> {
    into_result(judge_all(base, head, observed, tools))
}

/// `verdict` as [`judge`] and [`check`] give it: a verdict when it holds a finding or when
/// every step reached a result, and otherwise no verdict, naming every failed step.
fn into_result(verdict: Verdict) -> Result<Verdict, String> {
    if verdict.failed.is_empty() || !verdict.findings.is_empty() {
        Ok(verdict)
    } else {
        Err(verdict.failed.join("; and "))
    }
}

/// Judges the observations into one accumulator ([`judge_with`]).
#[must_use]
pub fn judge_all(base: &Path, head: &Path, observed: &Observations, tools: &Tools<'_>) -> Verdict {
    judge_with(base, head, Ok(observed), tools)
}

/// The verdict when there are no observations to judge, for `reason`: the observing job
/// failed, or its record cannot be read. Every step that needs no observations still runs
/// ([`judge_with`]), so that its findings fail the PR whatever the owner approved.
#[must_use]
pub fn judge_unobserved(base: &Path, head: &Path, tools: &Tools<'_>, reason: &str) -> Verdict {
    judge_with(base, head, Err(reason), tools)
}

/// Judges `observed`, or the reason there are none, into one accumulator. Every step of
/// [`STEPS`] has the form `fn step(.., verdict: &mut Verdict)` and returns nothing: it records
/// each finding the moment it is known, and a failure as [`Verdict::fail`], and the judge goes
/// on to the next step whatever failed. So no failure drops a finding (review of #57, rounds 11
/// to 13), and the fault-injection test checks that for every pair of steps. A step whose
/// inputs a failed step did not give, such as the corpus without observations of these trees,
/// is entered and finds nothing.
#[must_use]
pub fn judge_with(
    base: &Path,
    head: &Path,
    observed: Result<&Observations, &str>,
    tools: &Tools<'_>,
) -> Verdict {
    assert!(base.is_absolute() && head.is_absolute(), "the trees have absolute paths");
    let range = tools.commits.clone().unwrap_or_else(|| "the two trees given".to_owned());
    let mut verdict = Verdict { range, ..Verdict::default() };
    scope_step(base, head, tools, &mut verdict);
    let scoped = verdict.progress.scoped;
    if scoped && !verdict.changed.iter().any(|path| scope::is_tooling(path)) {
        if let Err(reason) = observed {
            verdict.fail(Step::Observations, reason);
        }
        let nothing = "No change to `xtask/`, `lints/` or a rule path: nothing to prove.";
        for step in STEPS.into_iter().skip(1) {
            verdict.enter(step);
            verdict.skip(step, nothing);
        }
        verdict.notes.push(nothing.to_owned());
        return verdict;
    }
    rule_set_step(base, head, tools, &mut verdict);
    owner_approval_step(base, tools, &mut verdict);
    observations_step(base, observed, tools, &mut verdict);
    let here = observed.ok().filter(|_| verdict.progress.observed);
    corpus::judge(base, head, here, tools, &mut verdict);
    let lint_changed = !scoped || verdict.changed.iter().any(|path| scope::is_lint_tooling(path));
    lint::judge(base, head, here, tools, lint_changed, &mut verdict);
    judge_proof(base, head, here, tools, &mut verdict);
    verdict.findings.sort();
    assert!(STEPS.starts_with(&verdict.visited), "the steps are entered once each, in order");
    verdict
}

/// The step [`Step::Observations`]: that `observed` are the observations of these trees.
fn observations_step(
    base: &Path,
    observed: Result<&Observations, &str>,
    tools: &Tools<'_>,
    verdict: &mut Verdict,
) {
    verdict.enter(Step::Observations);
    verdict.reach(Step::Observations, tools);
    let observed = match verdict.inject(Step::Observations, tools, observed) {
        Ok(observed) => observed,
        Err(reason) => return verdict.fail(Step::Observations, reason),
    };
    for reason in observed.failures_of(Step::Observations) {
        verdict.fail(Step::Observations, format!("observing failed: {reason}"));
    }
    if verdict.head_tree.is_empty() {
        return verdict.fail(
            Step::Observations,
            "they cannot be checked against the head tree, whose fingerprint is not known",
        );
    }
    match observed_here(base, observed, tools, &verdict.head_tree) {
        Ok(note) => {
            verdict.notes.push(note);
            verdict.progress.observed = true;
        }
        Err(reason) => verdict.fail(Step::Observations, reason),
    }
}

/// The step [`Step::OwnerApproval`]: whether a code owner approved the head commit, which turns
/// what the ratchet cannot prove into notes ([`Verdict::unprovable`]).
fn owner_approval_step(base: &Path, tools: &Tools<'_>, verdict: &mut Verdict) {
    verdict.enter(Step::OwnerApproval);
    let Some(source) = &tools.review else {
        return verdict.skip(Step::OwnerApproval, "no reviews are read (`--no-review`)");
    };
    verdict.reach(Step::OwnerApproval, tools);
    match verdict.inject(Step::OwnerApproval, tools, owner_approval(base, source)) {
        Ok(approved) => verdict.owner_approved = approved,
        Err(reason) => verdict.fail(Step::OwnerApproval, reason),
    }
}

/// Checks that `observed` are the observations of these trees: of the head tree `head_tree`,
/// of the base's corpus, of the commits judged, and drawn with the seed they give. Returns the
/// summary's note about the corpus.
///
/// # Errors
///
/// No verdict, before any finding: the observations are of something else.
fn observed_here(
    base: &Path,
    observed: &Observations,
    tools: &Tools<'_>,
    head_tree: &str,
) -> Result<String, String> {
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
    if observed.seed != tools.seed(&corpus, head_tree) {
        return Err(format!(
            "the observations were drawn with the seed {}, not the one this base and head give; \
             the placements cannot be checked",
            observed.seed
        ));
    }
    Ok(format!(
        "Base corpus: {files} files, fingerprint {corpus}, placement seed {}.",
        observed.seed
    ))
}

/// The step [`Step::RuleSet`], read from the trees as data: the tables only tighten, every
/// check call stays, and every rule with code in a rule path has a firing case, in the base for
/// a rule of the base and in the head for a new one. Each of the three reads fails alone: a
/// tree that cannot be read as the dialect allows.
fn rule_set_step(base: &Path, head: &Path, tools: &Tools<'_>, verdict: &mut Verdict) {
    verdict.enter(Step::RuleSet);
    verdict.reach(Step::RuleSet, tools);
    verdict.ran(Check::RuleSet);
    source::check(base, head, tools, verdict);
    // A tree that cannot be read as the dialect allows: the head's is a form the ratchet
    // refuses, the base's is missing evidence.
    for (tree, why) in [(base, Reason::Evidence), (head, Reason::Refused)] {
        match source::rules_without_cases(base, tree) {
            Err(reason) => verdict.fail_for(Step::RuleSet, why, reason),
            Ok(uncovered) => {
                for rule in uncovered {
                    let message = format!(
                        "rule {rule} has code in a rule path and no firing case in the base; \
                         give it a fixture first, or move its code to the core (ADR-0022)"
                    );
                    verdict.record(Finding::new(source::REGISTRY, Check::RuleSet, message));
                }
            }
        }
    }
    match source::new_rules_without_cases(base, head) {
        Err(reason) => verdict.fail_for(Step::RuleSet, Reason::Refused, reason),
        Ok(uncovered) => {
            for rule in uncovered {
                let message = format!(
                    "the new rule {rule} has code in a rule path and no firing case in the head; \
                     add its fixture to `xtask/fixtures/tidy/` in this PR (ADR-0022)"
                );
                verdict.record(Finding::new(source::REGISTRY, Check::RuleSet, message));
            }
        }
    }
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

/// The steps that stand in for the owner's review of rule paths: the class of the rule-path
/// diff, mutation testing of it from the observations or the carried evidence, and the review
/// gate. They run when a rule path changed, or when the scope could not be read, which leaves
/// that unknown: the review gate needs no rule path to find a request for changes.
fn judge_proof(
    base: &Path,
    head: &Path,
    observed: Option<&Observations>,
    tools: &Tools<'_>,
    verdict: &mut Verdict,
) {
    if verdict.progress.scoped && verdict.rule_paths.is_empty() {
        let skipped = "Mutation testing and the review check skipped: no rule path changed.";
        for step in [Step::Classify, Step::Carry, Step::Mutants, Step::NullRun, Step::ReviewGate] {
            verdict.enter(step);
            verdict.skip(step, skipped);
        }
        verdict.notes.push(skipped.to_owned());
        return;
    }
    classify_step(base, head, tools, verdict);
    carry_step(base, head, tools, verdict);
    let evidence = match &tools.carried {
        None => observed,
        Some(Ok(carried)) if !verdict.has_failed(Step::Carry) => Some(&carried.observations),
        Some(_) => None,
    };
    let (rule_paths, class) = (verdict.rule_paths.clone(), verdict.class.clone());
    let class = class.as_ref();
    let inputs = mutants::Inputs { base, head, rule_paths: &rule_paths, evidence, tools, class };
    mutants::judge(&inputs, verdict);
    review_gate(tools, verdict);
    conclude(verdict);
}

/// The step [`Step::Classify`]: whether the rule-path diff is additive or an edit. A diff that
/// cannot be classified is no verdict, but its missed mutants and the review gate's findings
/// still count (review of #57, round 12): the class is only for the coverage of the hunks and
/// for the edit gate. A diff the class cannot read is a form the ratchet refuses.
fn classify_step(base: &Path, head: &Path, tools: &Tools<'_>, verdict: &mut Verdict) {
    verdict.enter(Step::Classify);
    if verdict.rule_paths.is_empty() {
        return verdict.skip(Step::Classify, "the rule paths are not known; the scope says why");
    }
    verdict.reach(Step::Classify, tools);
    if verdict.faulted(Step::Classify, tools) {
        return;
    }
    match class::classify(base, head, &verdict.rule_paths) {
        Ok(class) => verdict.class = Some(class),
        Err(reason) => verdict.fail_for(Step::Classify, Reason::Refused, reason),
    }
}

/// The step [`Step::Carry`]: for a merge group, that the carried mutation evidence can be read
/// and is of this rule-path diff ([`carry::check`]). When it fails, mutation testing has no
/// evidence, and every other step is judged on. Neither evidence that cannot be read nor
/// another rule-path diff than the carried one ([`Reason::CarriedDiff`]) ever settles: the PR is
/// rebased and pushed (review of #57, round 15).
fn carry_step(base: &Path, head: &Path, tools: &Tools<'_>, verdict: &mut Verdict) {
    verdict.enter(Step::Carry);
    if !tools.mutants {
        return verdict.skip(Step::Carry, "mutation testing is skipped (`--no-mutants`)");
    }
    let Some(carried) = &tools.carried else {
        return verdict.skip(Step::Carry, "no evidence is carried: not a merge group");
    };
    verdict.reach(Step::Carry, tools);
    let read = verdict.inject(Step::Carry, tools, carried.as_ref().map_err(Clone::clone));
    let checked = read
        .map_err(|reason| (Reason::Evidence, reason))
        .and_then(|evidence| carry::check(base, head, evidence, tools.pr_head.as_deref()));
    match checked {
        Ok(note) => verdict.notes.push(note),
        Err((why, reason)) => verdict.fail_for(Step::Carry, why, reason),
    }
}

/// What the class leaves unproven, once every step has run. For an edit, the reviewer app's
/// approval stands in for the hunks without a mutant: a test edited with the code it tests pins
/// the edit, so mutants prove little there. Records nothing that can fail.
fn conclude(verdict: &mut Verdict) {
    let uncovered = std::mem::take(&mut verdict.uncovered);
    let Some(class) = verdict.class.clone() else {
        return;
    };
    if class.is_edit() && verdict.progress.app_approved {
        verdict.notes.push(format!(
            "An edit to existing rule code: the reviewer app approved the head commit, which \
             stands in for the {} hunk(s) without a mutant of their own.",
            uncovered.len()
        ));
    } else {
        if let Some(what) = unapproved_edit(&class) {
            verdict.unprovable(Reason::UnapprovedEdit, what);
        }
        for what in uncovered {
            verdict.unprovable(Reason::NoMutant, what);
        }
    }
    // A manifest or build file decides how the rule code builds, which nothing here reads: only
    // a code owner's approval settles it, never the reviewer app's (maintainer's decision,
    // review of #55, round 3).
    for path in &class.build_files {
        verdict.unprovable(
            Reason::BuildFile,
            format!(
                "{path} is a manifest or build file in the rule paths, which only a code owner's \
             approval of the head commit settles"
            ),
        );
    }
}

/// Whether a code owner approved the head commit of the pull request that `source` names.
///
/// # Errors
///
/// No verdict if the base's CODEOWNERS or the reviews cannot be read.
fn owner_approval(base: &Path, source: &ReviewSource<'_>) -> Result<bool, String> {
    let codeowners = fs::read_to_string(base.join(CODEOWNERS))
        .map_err(|error| format!("cannot read the base's {CODEOWNERS}: {error}"))?;
    review::owner_approved(source.reviews, source.pull, &source.head, &codeowners)
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

/// The step [`Step::ReviewGate`] ([`review::check`]): its findings go into `verdict`. An
/// additive diff needs nothing more, so a head that the reviewer app has not reviewed, or a
/// repository without the app, is a note. An edit to existing rule code needs the app's
/// approval of the head commit with no finding, which the step records for [`conclude`].
fn review_gate(tools: &Tools<'_>, verdict: &mut Verdict) {
    verdict.enter(Step::ReviewGate);
    let Some(source) = &tools.review else {
        verdict.skip(Step::ReviewGate, "no reviews are read (`--no-review`)");
        return verdict.unprovable(
            Reason::Evidence,
            "the review check was skipped (`--no-review`), so a request for changes or an open \
             thread by the reviewer app is not seen"
                .to_owned(),
        );
    };
    let Some(reviewer) = &source.reviewer else {
        verdict.skip(Step::ReviewGate, "no reviewer app is configured");
        return verdict.notes.push(format!(
            "No reviewer app is configured (`NAMI_REVIEWER_LOGIN`, such as {}; it runs in CI, \
             #59). An additive diff needs none; an edit needs a code owner's approval.",
            review::REVIEWER_APP
        ));
    };
    review::check(source.reviews, source.pull, &source.head, reviewer, tools, verdict);
}

/// Observes and judges in one process: for local runs and the tests.
///
/// # Errors
///
/// No verdict, as for [`observe`] and [`judge`].
pub fn check(base: &Path, head: &Path, tools: &Tools<'_>) -> Result<Verdict, String> {
    into_result(check_all(base, head, tools))
}

/// [`check`] as one accumulator ([`judge_all`]): a phase of [`observe`] that fails is
/// recorded, and its step fails in the judge.
#[must_use]
pub fn check_all(base: &Path, head: &Path, tools: &Tools<'_>) -> Verdict {
    judge_all(base, head, &observe(base, head, tools), tools)
}

/// `verdict`, settled by a code owner if it can be. A code owner's approval of the head commit
/// settles a no verdict only when each of its reasons is unprovable though the evidence is
/// complete ([`Reason::settleable`]): a form the ratchet refuses, such as a file outside the
/// dialect, a symbolic link or a name that is not valid UTF-8. Each becomes a note, and so does
/// each unproven item, and the verdict passes. Evidence that is missing, unreadable or
/// mismatched, a tool that failed, and a merge group whose rule-path diff is not the carried
/// one are never settled: an honest PR's remedy is a change to the PR, or a re-run (review of
/// #57, rounds 14 and 15). A verdict with any finding is never settled: it fails, and its
/// summary lists what was not judged as well (round 11).
///
/// # Errors
///
/// No verdict, naming every failed step, when `verdict` has no finding and holds missing
/// evidence, or no code owner approved the head, or no review source is given; with the reason
/// too if the approval cannot be read.
pub fn settled(base: &Path, tools: &Tools<'_>, mut verdict: Verdict) -> Result<Verdict, String> {
    if verdict.failed.is_empty() || !verdict.findings.is_empty() {
        return Ok(verdict);
    }
    let reasons = verdict.failed.join("; and ");
    if !verdict.is_settleable() {
        return Err(format!(
            "{reasons}. No approval settles missing or broken evidence: fix the PR, or re-run \
             the job (ADR-0022)"
        ));
    }
    let Some(source) = &tools.review else {
        return Err(reasons);
    };
    match owner_approval(base, source) {
        Ok(true) => {}
        Ok(false) => return Err(reasons),
        Err(problem) => {
            return Err(format!(
                "{reasons}; and the code owner's approval cannot be read: {problem}"
            ));
        }
    }
    verdict.owner_approved = true;
    for reason in std::mem::take(&mut verdict.failed) {
        verdict.notes.push(format!(
            "No verdict: {reason}. A code owner approved the head commit, which settles it \
             (ADR-0022)."
        ));
    }
    for what in std::mem::take(&mut verdict.unproven) {
        verdict.notes.push(format!("{what}; a code owner's approval of the head stands in."));
    }
    verdict.unproven_after.clear();
    Ok(verdict)
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

/// Tools that run nothing and read no reviews, for unit tests of single steps.
#[cfg(test)]
pub(crate) fn plain_tools() -> Tools<'static> {
    Tools {
        head_xtask: None,
        cargo: "cargo".into(),
        work: "/w".into(),
        mutants: false,
        review: None,
        seed: None,
        commits: None,
        pr_head: None,
        commit: None,
        carried: None,
        faults: Faults::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Check, Finding, Reason, Step, Tools, Verdict, same_commits, tail};
    use crate::ratchet::observations::Observations;

    fn tools(pr_head: Option<&str>, commits: Option<&str>) -> Tools<'static> {
        Tools {
            commits: commits.map(str::to_owned),
            pr_head: pr_head.map(str::to_owned),
            ..super::plain_tools()
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
        verdict.unprovable(Reason::NoMutant, "no mutant".to_owned());
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
        owned.unprovable(Reason::NoMutant, "no mutant".to_owned());
        assert_eq!((owned.unproven.len(), owned.notes.len()), (0, 1));
        assert_eq!(owned.line(), "ratchet: pass");
        assert!(owned.line_after().starts_with("ratchet: no verdict, 1"), "unowned after the flip");
        let shown = owned.summary();
        assert!(shown.contains("- Verdict today (every guardrail path owned): `ratchet: pass`"));
        assert!(shown.contains("after ADR-0022 (rule paths unowned): `ratchet: no verdict"));
        assert!(shown.ends_with("\nratchet: pass\n"), "the exit code follows today's verdict");
        let mut still = Verdict { owner_reviews: true, owner_reviews_after: true, ..owned };
        still.unprovable(Reason::UnapprovedEdit, "no reviewer".to_owned());
        assert_eq!(still.unproven_after.len(), 1, "only the earlier item; the owner still reviews");
        let mut approved = Verdict { owner_approved: true, ..Verdict::default() };
        approved.unprovable(Reason::NoMutant, "no mutant".to_owned());
        assert!(approved.unproven.is_empty() && approved.unproven_after.is_empty());
        assert!(approved.is_settleable(), "a hunk without a mutant is settleable");
        approved.unprovable(Reason::Evidence, "mutation testing was skipped".to_owned());
        assert_eq!(approved.unproven, ["mutation testing was skipped"], "missing evidence stays");
        assert!(!approved.is_settleable(), "missing evidence is never settled");
    }

    // Goal: rounds 14 and 15 of #57's review: an approval settles only what is unprovable
    // though the evidence is complete; missing or broken evidence, and a merge group's carried
    // rule-path diff that differs, never. The verdict line asks the code owner only for what is
    // theirs to settle, and otherwise for a fix or a re-run.
    #[test]
    fn only_complete_evidence_settles() {
        let settleable =
            [Reason::UnapprovedEdit, Reason::NoMutant, Reason::BuildFile, Reason::Refused];
        assert!(settleable.iter().all(|why| why.settleable()));
        assert!(!Reason::Evidence.settleable() && !Reason::CarriedDiff.settleable());
        let mut refused = Verdict::default();
        refused.fail_for(Step::Scope, Reason::Refused, "a symbolic link");
        assert!(refused.is_settleable() && refused.has_failed(Step::Scope));
        assert!(refused.line().ends_with("; the code owner decides"), "{}", refused.line());
        refused.fail(Step::LintLibrary, "the library run was killed");
        assert!(!refused.is_settleable(), "{:?}", refused.failed);
        assert!(refused.line().ends_with("; fix the PR or re-run"), "{}", refused.line());
        assert_eq!(
            refused.failed,
            ["scope: a symbolic link", "lint library: the library run was killed"]
        );
        let mut carried = Verdict::default();
        carried.fail_for(Step::Carry, Reason::CarriedDiff, "another rule-path diff");
        assert!(!carried.is_settleable(), "rebase and push instead");
        let mut skipped = Verdict::default();
        skipped.unprovable(Reason::Evidence, "mutation testing was skipped".to_owned());
        assert!(skipped.line().ends_with("thing(s) unproven; fix the PR or re-run"));
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
