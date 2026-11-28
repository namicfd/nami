//! Setting `passes` (ADR-0011, ADR-0017): the post-merge workflow's two jobs, the PR they open,
//! and what `check-pr` asks of that PR.
//!
//! - `passes [--output <file>]`, the `acceptance` job: on `main`, runs the acceptance tests of
//!   every active task that does not pass yet and whose dependencies pass. Prints
//!   `passes<TAB><id>` or `fails<TAB><id><TAB><reason>` for each, and writes the IDs of the
//!   tasks that fully pass to `<file>` as `{"passing": [...]}`. It asks GitHub nothing and
//!   holds no secret.
//! - `passes --open-pr <file>`, the `open passes PR` job: reads that file as data and keeps
//!   the tasks that may be set: active, not passing, every dependency passing, and a merged PR
//!   from `task/<id>`. It builds the commit that [`BOT_BRANCH`] should hold, those flips on
//!   top of `HEAD`, and force-pushes it only when the branch's own change is not those flips,
//!   no longer applies cleanly to `HEAD`, or sits on a parent that is not on `main`; a branch
//!   on an older `main` is left alone, unless the checks that `main` requires failed on it,
//!   and then a new commit is pushed so that they run again. Then it opens the PR if there is
//!   none, and reconciles it every run: the title and description match the flips, and
//!   auto-merge is on. When no task is left, it closes the open PR, if there is one. An open
//!   PR from [`BOT_BRANCH`] that [`CI_LOGIN`] did not open is never adopted: the job fails and
//!   changes nothing. It runs no tests, so the bot's token never meets a test binary. Only the
//!   push and the PR writes act as the app's token, [`WRITE_TOKEN`]; every read, of the checks
//!   and of `main`'s history too, acts as `GH_TOKEN` (`super::pulls`), and no other git
//!   command sees the token.
//! - `check-pr` lets a change set `passes` only when [`flip_problems`] finds nothing, and
//!   rejects a PR from [`BOT_BRANCH`] or by [`CI_LOGIN`] that sets none ([`no_flip_problems`]).
//!   On `pull_request` events CI passes the PR's head from the event, so a change that sets no
//!   `passes` is checked without GitHub.
//!
//! The merged PR is a weak condition: any agent can merge a trivial PR from `task/<id>`. The
//! acceptance tests carry the weight; the merged PR only keeps a task from passing on a test
//! that a later task wrote, or that two tasks share.

use super::acceptance::Acceptance;
use super::claims::{git, git_command};
use super::plan::{self, Task};
use super::pulls::WRITE_TOKEN;
use crate::json::{self, Value};
use crate::queue::failure::{pull_of_message, queue_entry};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Stdio;

/// The git credential helper for the push to [`BOT_BRANCH`]: it answers `get` with the token
/// in [`WRITE_TOKEN`], which git inherits, and nothing else. It is configured on the push
/// command only, after the helpers from git's own configuration are cleared.
const PUSH_CREDENTIALS: &str = "!f() { test \"$1\" = get && printf \
                                'username=x-access-token\\npassword=%s\\n' \
                                \"$NAMI_PASSES_TOKEN\"; }; f";

/// The one branch whose PR may set `passes`.
pub const BOT_BRANCH: &str = "bot/passes";

/// The bot of the `nami-ci` GitHub App: the one author whose PR may set `passes`. Agents act
/// as `nami-agent[bot]`, so no agent can open that PR.
pub use crate::issue::trust::CI_LOGIN;

/// The branch that tasks merge into.
const BASE_BRANCH: &str = "main";

/// The line that marks a task as not passing yet, as the plans write it.
const NOT_PASSING: &str = "\"passes\": false";

/// The same line once the task passes.
const PASSING: &str = "\"passes\": true";

/// The one key of the file that the `acceptance` job writes.
const PASSING_KEY: &str = "passing";

/// A pull request's head branch and author, as GitHub or the event that started CI has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullHead {
    /// The head branch; `<owner>:<branch>` when it lives in another repository.
    pub branch: String,
    /// The login of whoever opened it.
    pub author: String,
}

impl PullHead {
    /// The head of a PR from `branch` in the repository `head_repository`, opened by `author`,
    /// against `repository`. A head elsewhere, or in a deleted repository (empty), is written
    /// `<owner>:<branch>`, so it is never this repository's branch.
    #[must_use]
    pub fn from_event(branch: &str, head_repository: &str, repository: &str, author: &str) -> Self {
        assert!(!repository.is_empty(), "the event names this repository");
        let branch = if head_repository == repository {
            branch.to_owned()
        } else {
            let owner = head_repository.split('/').next().filter(|owner| !owner.is_empty());
            format!("{}:{branch}", owner.unwrap_or("(unknown)"))
        };
        Self { branch, author: author.to_owned() }
    }
}

/// What the open-PR job reconciles on the bot's PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullState {
    /// Its head.
    pub head: PullHead,
    /// Its title, which becomes the squash commit's subject.
    pub title: String,
    /// Its description.
    pub body: String,
    /// True if auto-merge is on, so that it enters the merge queue once its checks pass.
    pub auto_merge: bool,
    /// True while it is open.
    pub open: bool,
    /// True once it has merged.
    pub merged: bool,
}

/// Where the checks that `main` requires stand on a commit, as GitHub shows them now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checks {
    /// Every one of them completed and passed, or none is required.
    Passed,
    /// None failed, and at least one has not completed, or has not reported on the commit yet.
    Pending,
    /// These completed without passing, each named with its conclusion, such as `gate
    /// (failure)`. Never empty.
    Failed(Vec<String>),
}

/// What setting `passes` needs from GitHub: its pull requests, the checks on their heads, and
/// `main`'s history. Every error says what failed.
pub trait Pulls {
    /// Pull request `number`: its head, title, description and auto-merge.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or does not have it.
    fn pull(&self, number: u32) -> Result<PullState, String>;

    /// True if a pull request from this repository's `branch` has merged into `main`.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked.
    fn has_merged(&self, branch: &str) -> Result<bool, String>;

    /// The open pull request from this repository's `branch`, if there is one.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or lists more than one.
    fn open_from(&self, branch: &str) -> Result<Option<u32>, String>;

    /// Opens a pull request from `branch` into `main`, and returns its number.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it or cannot be asked.
    fn open(&self, branch: &str, title: &str, body: &str) -> Result<u32, String>;

    /// Replaces the title and description of pull request `number`.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it or cannot be asked.
    fn edit(&self, number: u32, title: &str, body: &str) -> Result<(), String>;

    /// Closes pull request `number` without merging it.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it or cannot be asked.
    fn close(&self, number: u32) -> Result<(), String>;

    /// Turns on auto-merge for pull request `number`, so that it enters the merge queue once
    /// its checks pass.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it or cannot be asked.
    fn auto_merge(&self, number: u32) -> Result<(), String>;

    /// Where the checks that `main`'s rules require stand on `commit`.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or sends a check in a state this code does not know.
    fn required_checks(&self, commit: &str) -> Result<Checks, String>;

    /// True if `ancestor` is `commit` or one of its ancestors, in this repository on GitHub.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or does not know one of the two.
    fn is_ancestor(&self, ancestor: &str, commit: &str) -> Result<bool, String>;
}

/// What `passes` and `check-pr` need from outside the checkout.
#[derive(Clone, Copy)]
pub struct Services<'services> {
    /// GitHub's pull requests.
    pub pulls: &'services dyn Pulls,
    /// The test runner.
    pub tests: &'services dyn Acceptance,
    /// The environment's variables, such as [`WRITE_TOKEN`].
    pub variables: &'services dyn Fn(&str) -> Option<String>,
}

/// The change that `check-pr` checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change<'change> {
    /// The commit it is compared with.
    pub base: &'change str,
    /// Its branch: a PR's head branch, a merge-queue branch, or empty.
    pub branch: &'change str,
    /// Its pull request, if CI knows it; in the merge queue, the branch names it.
    pub pull: Option<u32>,
    /// The PR's head as the `pull_request` event has it, so that a change that sets no
    /// `passes` is checked without asking GitHub.
    pub head: Option<&'change PullHead>,
}

const USAGE: &str = "usage: cargo xtask task passes [--output <file> | --open-pr <file>] | \
                     check-pr <base> [<branch> [<pull request> [--author <login> --head-repo \
                     <owner/name> --repo <owner/name>]]]";

/// Runs `passes` or `check-pr` against the checkout at `root`, pushing to `remote`.
///
/// # Errors
///
/// Returns the reason the command failed, or why a change may not set `passes`.
pub fn execute(
    root: &Path,
    remote: &str,
    services: Services<'_>,
    arguments: &[&str],
) -> Result<String, String> {
    let change = |base, branch, pull| Change { base, branch, pull, head: None };
    match arguments {
        ["passes"] => found(root, services.tests, None),
        ["passes", "--output", file] => found(root, services.tests, Some(Path::new(file))),
        ["passes", "--open-pr", file] => {
            open_pr((root, remote), services.pulls, services.variables, Path::new(file))
        }
        ["check-pr", base] => super::check_pr(root, &change(base, "", None), services),
        ["check-pr", base, branch] => super::check_pr(root, &change(base, branch, None), services),
        ["check-pr", base, branch, pull] => {
            let pull = pull_number(pull)?;
            super::check_pr(root, &change(base, branch, pull), services)
        }
        [
            "check-pr",
            base,
            branch,
            pull,
            "--author",
            author,
            "--head-repo",
            head_repository,
            "--repo",
            repository,
        ] => {
            let pull = pull_number(pull)?.ok_or("the event's PR needs its number")?;
            if repository.is_empty() || author.is_empty() || branch.is_empty() {
                return Err("the event names no repository, author or branch".to_owned());
            }
            let head = PullHead::from_event(branch, head_repository, repository, author);
            let known = Change { head: Some(&head), ..change(base, branch, Some(pull)) };
            super::check_pr(root, &known, services)
        }
        _ => Err(USAGE.to_owned()),
    }
}

/// A pull request number from the command line, written as GitHub writes it; empty means
/// none, as CI passes it in the merge queue.
fn pull_number(text: &str) -> Result<Option<u32>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    let number = text.parse::<u32>().ok().filter(|number| number.to_string() == text);
    let number = number.filter(|number| *number > 0);
    number.map(Some).ok_or_else(|| format!("`{text}` is not a pull request number"))
}

/// The branch of task `id`.
fn task_branch(id: &str) -> String {
    format!("{}{id}", super::TASK_BRANCH_PREFIX)
}

/// The tasks whose tests are worth running: active, not passing, and every dependency
/// passing. Nothing here asks GitHub.
#[must_use]
pub fn unblocked(tasks: &[Task]) -> Vec<&Task> {
    let passing: BTreeSet<&str> =
        tasks.iter().filter(|task| task.passes).map(|task| task.id.as_str()).collect();
    let passing_count = tasks.iter().filter(|task| task.passes).count();
    assert_eq!(passing.len(), passing_count, "task IDs are unique: the plans were validated");
    tasks
        .iter()
        .filter(|task| task.active && !task.passes)
        .filter(|task| task.depends_on.iter().all(|id| passing.contains(id.as_str())))
        .collect()
}

/// The tasks that may be set now, before their tests run: [`unblocked`], and a merged PR from
/// `task/<id>`.
///
/// # Errors
///
/// When GitHub cannot be asked.
pub fn candidates<'tasks>(
    tasks: &'tasks [Task],
    pulls: &dyn Pulls,
) -> Result<Vec<&'tasks Task>, String> {
    let mut found = Vec::new();
    for task in unblocked(tasks) {
        let branch = task_branch(&task.id);
        assert!(plan::is_valid_id(&task.id), "a task ID cannot change the query: {branch}");
        if pulls.has_merged(&branch)? {
            found.push(task);
        }
    }
    Ok(found)
}

/// `passes [--output <file>]`.
fn found(root: &Path, tests: &dyn Acceptance, output: Option<&Path>) -> Result<String, String> {
    let tasks = super::valid_tasks(root)?;
    let unblocked = unblocked(&tasks);
    let verdicts = tests.check(&unblocked)?;
    assert_eq!(verdicts.len(), unblocked.len(), "one verdict per task");
    let mut lines = Vec::new();
    let mut passing = Vec::new();
    for (task, verdict) in unblocked.iter().zip(verdicts) {
        match verdict {
            Ok(()) => {
                lines.push(format!("passes\t{}\n", task.id));
                passing.push(task.id.as_str());
            }
            Err(reason) => lines.push(format!("fails\t{}\t{reason}\n", task.id)),
        }
    }
    if let Some(file) = output {
        std::fs::write(file, passing_json(&passing))
            .map_err(|error| format!("cannot write {}: {error}", file.display()))?;
    }
    if unblocked.is_empty() {
        lines.push("no task to run: none is active, unfinished and unblocked\n".to_owned());
    }
    Ok(lines.concat())
}

/// The file that the `acceptance` job hands to the `open passes PR` job.
#[must_use]
pub fn passing_json(ids: &[&str]) -> String {
    assert!(ids.iter().all(|id| plan::is_valid_id(id)), "only task IDs are written");
    let quoted: Vec<String> = ids.iter().map(|id| json::quote(id)).collect();
    format!("{{\"{PASSING_KEY}\": [{}]}}\n", quoted.join(", "))
}

/// The task IDs in a file that [`passing_json`] wrote. It is the only thing that crosses from
/// the job that runs tests to the job that holds the bot's key, so it is read as data and
/// nothing else is accepted.
///
/// # Errors
///
/// When the file is not exactly an object with one key, `passing`, holding distinct task IDs.
pub fn passing_from_json(text: &str) -> Result<BTreeSet<String>, String> {
    let shape = format!("the file must be {{\"{PASSING_KEY}\": [<task id>, ...]}}");
    let value = json::parse(text).map_err(|error| format!("{shape}: {}", error.message))?;
    let Some([(key, Value::Array(entries))]) = value.as_object() else {
        return Err(shape);
    };
    if key != PASSING_KEY {
        return Err(shape);
    }
    let mut ids = BTreeSet::new();
    for entry in entries {
        let id = entry.as_str().filter(|id| plan::is_valid_id(id));
        let id = id.ok_or_else(|| format!("{shape}; {entry:?} is not a task id"))?;
        if !ids.insert(id.to_owned()) {
            return Err(format!("{shape}; `{id}` is listed twice"));
        }
    }
    Ok(ids)
}

/// Sets `passes` on the tasks `ids` in their `tasks.json` files, and checks that nothing else
/// changed.
fn write_passes(root: &Path, tasks: &[Task], ids: &BTreeSet<String>) -> Result<(), String> {
    let mut by_file: BTreeMap<&Path, BTreeSet<String>> = BTreeMap::new();
    for task in tasks.iter().filter(|task| ids.contains(&task.id)) {
        let _ = by_file.entry(task.file.as_path()).or_default().insert(task.id.clone());
    }
    let total: usize = by_file.values().map(BTreeSet::len).sum();
    assert_eq!(total, ids.len(), "every task to set is a task of the plans");
    for (file, file_ids) in by_file {
        let path = root.join(file);
        let read = std::fs::read_to_string(&path);
        let text = read.map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        let flipped = set_passes(&text, &file_ids)?;
        only_passes_set(&file.to_string_lossy(), &text, &flipped)?;
        std::fs::write(&path, flipped)
            .map_err(|error| format!("cannot write {}: {error}", file.display()))?;
    }
    Ok(())
}

/// `text`, a `tasks.json`, with `"passes": false` changed to `true` on exactly the tasks
/// `ids`.
///
/// # Errors
///
/// When a task's `"id"` line or its `"passes": false` line is not found: the plans write one
/// field per line.
pub fn set_passes(text: &str, ids: &BTreeSet<String>) -> Result<String, String> {
    let mut current: Option<&str> = None;
    let mut flipped = BTreeSet::new();
    let mut result = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let field = line.trim().trim_end_matches(',');
        if let Some(rest) = field.strip_prefix("\"id\": \"") {
            current = rest.strip_suffix('"');
        }
        match current {
            Some(id) if field == NOT_PASSING && ids.contains(id) => {
                result.push_str(&line.replacen(NOT_PASSING, PASSING, 1));
                let _ = flipped.insert(id.to_owned());
            }
            _ => result.push_str(line),
        }
    }
    if flipped != *ids {
        let missing: Vec<&String> = ids.difference(&flipped).collect();
        return Err(format!(
            "cannot find the `{NOT_PASSING}` line of {missing:?}; the plans write one field \
             per line"
        ));
    }
    assert_eq!(result.len() + ids.len(), text.len(), "`true` is one byte shorter than `false`");
    Ok(result)
}

/// Checks that `head` differs from `base`, two versions of the file `path`, only in lines
/// `"passes": false` that became `"passes": true`.
///
/// # Errors
///
/// Names the first line that changes anything else.
pub fn only_passes_set(path: &str, base: &str, head: &str) -> Result<(), String> {
    let base_lines: Vec<&str> = base.split_inclusive('\n').collect();
    let head_lines: Vec<&str> = head.split_inclusive('\n').collect();
    if base_lines.len() != head_lines.len() {
        return Err(format!("`{path}` adds or removes lines; only `passes` values may change"));
    }
    for (number, (old, new)) in base_lines.iter().zip(&head_lines).enumerate() {
        let is_flip = old.trim().trim_end_matches(',') == NOT_PASSING
            && *new == old.replacen(NOT_PASSING, PASSING, 1);
        if old != new && !is_flip {
            return Err(format!(
                "`{path}` line {} changes more than a `passes` value from false to true",
                number + 1
            ));
        }
    }
    Ok(())
}

/// True for `docs/exec-plans/active/<plan>/tasks.json`.
#[must_use]
pub fn is_active_tasks_file(path: &str) -> bool {
    let prefix = format!("{}/active/", plan::PLANS_DIRECTORY);
    let plan = path.strip_prefix(&prefix).and_then(|rest| rest.strip_suffix("/tasks.json"));
    plan.is_some_and(|plan| !plan.is_empty() && !plan.contains('/'))
}

/// Why the change from `base` to the working tree's tracked files is more than `passes`
/// values set to true in active plans.
fn shape_problems(root: &Path, base: &str) -> Result<Vec<String>, String> {
    assert!(!base.is_empty() && !base.starts_with('-'), "a base is a revision: {base}");
    let arguments = ["diff", "--no-renames", "--name-status", base];
    let diff = git(root, &arguments, &[], None).map_err(|error| error.to_string())?;
    let mut problems = Vec::new();
    for line in diff.lines() {
        let (status, path) = line.split_once('\t').unwrap_or((line, ""));
        if status != "M" || !is_active_tasks_file(path) {
            problems.push(format!(
                "`{path}` ({status}) is not a change to an active plan's tasks.json; a change \
                 that sets `passes` changes nothing else"
            ));
            continue;
        }
        let shown = git(root, &["show", &format!("{base}:{path}")], &[], None);
        let old = shown.map_err(|error| error.to_string())?;
        let new = std::fs::read_to_string(root.join(path))
            .map_err(|error| format!("cannot read {path}: {error}"))?;
        if let Err(problem) = only_passes_set(path, &old, &new) {
            problems.push(problem);
        }
    }
    Ok(problems)
}

/// The commit that `check-pr` compares the change with. In the merge queue, `base` is the
/// group's base, and the branch also holds the PRs ahead of this one; then the change is this
/// PR's own squashed commit, `HEAD`, whose subject ends in `(#<pull>)`, and the base is its
/// parent. Everywhere else it is `base`.
///
/// # Errors
///
/// When git cannot read `HEAD` or its parent.
pub fn own_base(root: &Path, base: &str, branch: &str) -> Result<String, String> {
    assert!(!base.is_empty() && !base.starts_with('-'), "a base is a revision: {base}");
    let Some(entry) = queue_entry(branch) else {
        return Ok(base.to_owned());
    };
    let run = |arguments: &[&str]| {
        git(root, arguments, &[], None).map_err(|error| format!("cannot read HEAD: {error}"))
    };
    let subject = run(&["log", "-1", "--format=%s", "HEAD"])?;
    if pull_of_message(&subject) != Some(entry.pull) {
        return Ok(base.to_owned());
    }
    let parent = run(&["rev-parse", "--verify", "HEAD^1"])?.trim().to_owned();
    assert!(is_object_name(&parent), "rev-parse --verify names one commit: {parent}");
    Ok(parent)
}

/// Why `change`, which sets `passes` on the tasks `flipped`, may not do so. Empty means it
/// may: all of these hold.
///
/// 1. It is pull request `change.pull`, or the one that the merge-queue branch names. Its head
///    is [`BOT_BRANCH`] in this repository, and [`CI_LOGIN`] opened it.
/// 2. It changes nothing but `"passes": false` lines to `true`, in active plans' `tasks.json`.
/// 3. A PR from each flipped task's branch `task/<id>` has merged into `main`.
/// 4. Each flipped task's acceptance tests run and pass now, in this run. They run only when
///    1 to 3 hold, and a name that matches no test fails.
///
/// A check that cannot run is a problem too, so that it never passes.
#[must_use]
pub fn flip_problems(
    root: &Path,
    change: &Change<'_>,
    head_tasks: &[Task],
    flipped: &[String],
    services: Services<'_>,
) -> Vec<String> {
    assert!(!flipped.is_empty(), "only a change that sets passes is asked about");
    let mut problems = pull_problems(change, services.pulls);
    match shape_problems(root, change.base) {
        Ok(found) => problems.extend(found),
        Err(error) => problems.push(format!("cannot read the change: {error}")),
    }
    for id in flipped {
        match services.pulls.has_merged(&task_branch(id)) {
            Ok(true) => {}
            Ok(false) => problems.push(format!(
                "no PR from `{}` has merged into `{BASE_BRANCH}`, so `{id}` is not done",
                task_branch(id)
            )),
            Err(error) => problems.push(format!("cannot tell whether `{id}` merged: {error}")),
        }
    }
    if problems.is_empty() {
        let tasks: Vec<&Task> =
            head_tasks.iter().filter(|task| flipped.contains(&task.id)).collect();
        assert_eq!(tasks.len(), flipped.len(), "every flipped task is a task of the change");
        match services.tests.check(&tasks) {
            Ok(verdicts) => {
                // Before the zip: a short list would let the flips past its end through.
                assert_eq!(verdicts.len(), tasks.len(), "one verdict per flipped task");
                let failed = tasks.iter().zip(verdicts).filter_map(|(task, verdict)| {
                    verdict.err().map(|reason| format!("`{}` does not pass: {reason}", task.id))
                });
                problems.extend(failed);
            }
            Err(error) => problems.push(format!("cannot run the acceptance tests: {error}")),
        }
    }
    problems
}

/// Why `change` is not [`CI_LOGIN`]'s PR from [`BOT_BRANCH`].
fn pull_problems(change: &Change<'_>, pulls: &dyn Pulls) -> Vec<String> {
    assert!(change.pull.is_none_or(|number| number > 0), "pull requests are numbered from 1");
    let problems = bot_pull_problems(change, pulls);
    let named = |problem: &String| problem.contains('#') || problem.contains(change.branch);
    assert!(problems.iter().all(named), "each problem names the PR it is about: {problems:?}");
    problems
}

/// [`pull_problems`], without its checks.
fn bot_pull_problems(change: &Change<'_>, pulls: &dyn Pulls) -> Vec<String> {
    let queued = queue_entry(change.branch).map(|entry| entry.pull);
    let Some(number) = change.pull.or(queued) else {
        return vec![format!(
            "`{}` is no merge-queue branch and no pull request was given, so this is not the \
             bot's PR from `{BOT_BRANCH}`",
            change.branch
        )];
    };
    let head = match pulls.pull(number) {
        Ok(state) => state.head,
        Err(error) => return vec![format!("cannot read pull request #{number}: {error}")],
    };
    let mut problems = Vec::new();
    if head.branch != BOT_BRANCH {
        problems.push(format!(
            "#{number} comes from `{}`; only a PR from `{BOT_BRANCH}` may set `passes`",
            head.branch
        ));
    }
    if head.author != CI_LOGIN {
        problems.push(format!(
            "#{number} was opened by `{}`; only {CI_LOGIN} may set `passes`, and agents never \
             do (ADR-0011)",
            head.author
        ));
    }
    problems
}

/// Why `change`, which sets no `passes`, is still refused: [`BOT_BRANCH`] and [`CI_LOGIN`]'s
/// PRs exist only to set it, so anything else they carry is someone else's change. Empty
/// when it is neither. The head comes from the event when CI passes it, and from GitHub only
/// for a merge-queue entry or a PR given without it. A PR that cannot be looked up is a
/// problem, so the check never passes unseen.
#[must_use]
pub fn no_flip_problems(change: &Change<'_>, pulls: &dyn Pulls) -> Vec<String> {
    assert!(change.head.is_none() || change.pull.is_some(), "an event's head comes with its PR");
    let problems = flipless_problems(change, pulls);
    assert!(problems.len() <= 1, "one reason at most: {problems:?}");
    let refused = |problem: &String| {
        problem.contains("sets no `passes`") || problem.starts_with("cannot read pull request")
    };
    assert!(problems.iter().all(refused), "a flip-less PR is refused for one reason only");
    problems
}

/// [`no_flip_problems`], without its checks.
fn flipless_problems(change: &Change<'_>, pulls: &dyn Pulls) -> Vec<String> {
    let refusal = |head: &PullHead, what: &str| {
        format!(
            "{what} comes from `{}` by `{}`, and it sets no `passes`; that branch and that bot \
             only ever set it",
            head.branch, head.author
        )
    };
    if let Some(head) = change.head {
        let is_bot = head.branch == BOT_BRANCH || head.author == CI_LOGIN;
        return if is_bot { vec![refusal(head, "the PR")] } else { Vec::new() };
    }
    if change.branch == BOT_BRANCH {
        return vec![format!(
            "the change comes from `{BOT_BRANCH}`, and it sets no `passes`; that branch only \
             ever sets it"
        )];
    }
    let queued = queue_entry(change.branch).map(|entry| entry.pull);
    let Some(number) = change.pull.or(queued) else {
        return Vec::new();
    };
    match pulls.pull(number) {
        Ok(state) if state.head.branch == BOT_BRANCH || state.head.author == CI_LOGIN => {
            vec![refusal(&state.head, &format!("#{number}"))]
        }
        Ok(_) => Vec::new(),
        Err(error) => vec![format!("cannot read pull request #{number}: {error}")],
    }
}

/// The open PR from [`BOT_BRANCH`], if there is one, once it is known to be [`CI_LOGIN`]'s.
///
/// # Errors
///
/// When someone else opened it: pushing would change their PR, and closing it is a human's
/// call. Nothing is changed.
fn adoptable(pulls: &dyn Pulls) -> Result<Option<u32>, String> {
    let Some(number) = pulls.open_from(BOT_BRANCH)? else {
        return Ok(None);
    };
    let head = pulls.pull(number)?.head;
    if head.author != CI_LOGIN || head.branch != BOT_BRANCH {
        return Err(format!(
            "#{number}, the open PR from `{BOT_BRANCH}`, comes from `{}` by `{}`, not from \
             {CI_LOGIN}; nothing was changed. A human closes #{number}, and the next run opens \
             the bot's PR",
            head.branch, head.author
        ));
    }
    Ok(Some(number))
}

/// `passes --open-pr <file>`. It refuses to start without the token for writes in
/// [`WRITE_TOKEN`], as gh's writes do, so that nothing is half done for want of it.
fn open_pr(
    (root, remote): (&Path, &str),
    pulls: &dyn Pulls,
    variables: &dyn Fn(&str) -> Option<String>,
    file: &Path,
) -> Result<String, String> {
    let token = variables(WRITE_TOKEN).filter(|token| !token.trim().is_empty());
    let token = token.ok_or_else(|| {
        format!("`{WRITE_TOKEN}` holds no token, so `bot/passes` cannot be pushed")
    })?;
    let text = std::fs::read_to_string(file)
        .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    let reported = passing_from_json(&text)?;
    let tasks = super::valid_tasks(root)?;
    let allowed: BTreeSet<String> =
        candidates(&tasks, pulls)?.iter().map(|task| task.id.clone()).collect();
    let wanted: BTreeSet<String> = reported.intersection(&allowed).cloned().collect();
    let mut lines: Vec<String> = reported
        .difference(&allowed)
        .map(|id| {
            format!(
                "skipped\t{id}\tnot a task that may be set: unknown, done or blocked, or no PR \
                 from `{}` has merged\n",
                task_branch(id)
            )
        })
        .collect();
    let open = adoptable(pulls)?;
    if wanted.is_empty() {
        lines.push(match open {
            Some(number) => {
                pulls.close(number)?;
                format!("closed #{number}: no task is newly done\n")
            }
            None => "no task is newly done\n".to_owned(),
        });
        return Ok(lines.concat());
    }
    write_passes(root, &tasks, &wanted)?;
    let problems = shape_problems(root, "HEAD")?;
    if !problems.is_empty() {
        return Err(format!(
            "the working tree changes more than `passes`:\n{}",
            problems.join("\n")
        ));
    }
    let after = super::valid_tasks(root)?;
    let passing = |all: &[Task]| -> BTreeSet<String> {
        all.iter().filter(|task| task.passes).map(|task| task.id.clone()).collect()
    };
    let newly: BTreeSet<String> = passing(&after).difference(&passing(&tasks)).cloned().collect();
    assert_eq!(newly, wanted, "the working tree sets `passes` on exactly the wanted tasks");
    let flipped: Vec<String> =
        after.iter().filter(|task| wanted.contains(&task.id)).map(|task| task.id.clone()).collect();
    lines.push(publish((root, remote, &token), pulls, (&after, &flipped), open)?);
    Ok(lines.concat())
}

/// Builds the commit that [`BOT_BRANCH`] should hold, the flips in the working tree on top of
/// `HEAD`, and force-pushes it only when `remote`'s branch does not hold it ([`held_tip`]):
/// an unchanged set of flips that still applies cleanly leaves the PR's place in the merge
/// queue alone, however far `main` moved, and other flips or a conflict rebuild the branch.
/// A held tip is pushed again, as a new commit, when its required checks failed
/// ([`failed_checks`]), so that they run again. Then it opens the PR if `open` is none, and
/// reconciles it ([`reconcile`]).
fn publish(
    (root, remote, token): (&Path, &str, &str),
    pulls: &dyn Pulls,
    (tasks, flipped): (&[Task], &[String]),
    open: Option<u32>,
) -> Result<String, String> {
    assert!(!flipped.is_empty(), "a PR sets passes on at least one task");
    let head = git(root, &["rev-parse", "HEAD"], &[], None).map_err(|error| error.to_string())?;
    let (title, body) = pull_text(tasks, flipped, head.trim());
    let commit = commit_working_tree(root, &title, None)?;
    let mut done = Vec::new();
    match held_tip(root, remote, pulls, &commit)? {
        None => {
            push(root, remote, &commit, token)?;
            done.push("pushed".to_owned());
        }
        Some(tip) => {
            if let Some(failed) = failed_checks(pulls, open, &tip)? {
                let fresh = distinct_commit(root, &title, &commit, &tip)?;
                assert_ne!(fresh, tip, "only a new commit makes GitHub run the checks again");
                push(root, remote, &fresh, token)?;
                done.push(format!("pushed again, as {failed} did not pass"));
            }
        }
    }
    let number = if let Some(number) = open {
        number
    } else {
        done.push("opened".to_owned());
        pulls.open(BOT_BRANCH, &title, &body)?
    };
    done.extend(reconcile(pulls, number, &title, &body)?.into_iter().map(str::to_owned));
    let changes = if done.is_empty() { "nothing to change".to_owned() } else { done.join(", ") };
    Ok(format!("#{number} sets `passes` on {}: {changes}\n", flipped.join(", ")))
}

/// The required checks that did not pass on `tip`, the head of the open PR `open`, named for
/// the log; none while they pass or are pending, or when no PR is open, since opening one
/// starts its checks.
///
/// The checks that count are those that `main`'s rules require (`gate` and `lints (UI tests)`
/// today), not every check on the head: advisory checks, such as `mutants (diff, advisory)`,
/// may fail without keeping the PR out of the queue, and a push for each of them would take
/// it out of the queue on every run until it starves (#62). Nothing reruns a failed check by
/// itself: `ci.yml` starts on a push or on opening, and [`reconcile`]'s writes start nothing.
///
/// # Errors
///
/// When GitHub cannot be asked. The run then fails before it changes anything, as for every
/// other read, and the next run asks again.
fn failed_checks(
    pulls: &dyn Pulls,
    open: Option<u32>,
    tip: &str,
) -> Result<Option<String>, String> {
    assert!(is_object_name(tip), "a held tip is a commit: {tip}");
    if open.is_none() {
        return Ok(None);
    }
    match pulls.required_checks(tip)? {
        Checks::Passed | Checks::Pending => Ok(None),
        Checks::Failed(failed) => {
            assert!(!failed.is_empty(), "a failure names the checks that failed");
            let named: Vec<String> = failed.iter().map(|check| format!("`{check}`")).collect();
            Ok(Some(named.join(", ")))
        }
    }
}

/// `commit`, or, when it is `tip` itself, the same change committed one second after `tip`.
/// A push of the commit that the branch already holds changes nothing, so GitHub would run no
/// check again. The two are equal only when `main` has not moved and both were built in the
/// same second.
///
/// # Errors
///
/// When git cannot read `tip`'s time or build the commit.
fn distinct_commit(root: &Path, title: &str, commit: &str, tip: &str) -> Result<String, String> {
    assert!(is_object_name(commit) && is_object_name(tip), "two commits: {commit} {tip}");
    if commit != tip {
        return Ok(commit.to_owned());
    }
    let time = git(root, &["log", "-1", "--format=%ct", tip], &[], None);
    let time = time.map_err(|error| format!("cannot read the time of {tip}: {error}"))?;
    let seconds = time.trim().parse::<u64>();
    let seconds = seconds.map_err(|_| format!("git printed `{}` as {tip}'s time", time.trim()))?;
    let later = format!("@{} +0000", seconds + 1);
    let fresh = commit_working_tree(root, title, Some(&later))?;
    assert_ne!(fresh, tip, "a later committer time makes another commit");
    Ok(fresh)
}

/// Force-pushes `commit` as `remote`'s [`BOT_BRANCH`], as `token`. This is the one git command
/// that gets [`WRITE_TOKEN`], through a credential helper that only it is configured with.
fn push(root: &Path, remote: &str, commit: &str, token: &str) -> Result<(), String> {
    assert!(!token.trim().is_empty(), "open_pr refuses a run without the token for writes");
    assert!(PUSH_CREDENTIALS.contains(WRITE_TOKEN), "the helper answers with the write token");
    let credentials = [
        (WRITE_TOKEN, token),
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "credential.helper"),
        ("GIT_CONFIG_VALUE_0", ""),
        ("GIT_CONFIG_KEY_1", "credential.helper"),
        ("GIT_CONFIG_VALUE_1", PUSH_CREDENTIALS),
    ];
    let target = format!("{commit}:refs/heads/{BOT_BRANCH}");
    let _ = git(root, &["push", "--quiet", "--force", remote, &target], &credentials, None)
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The tip of `remote`'s [`BOT_BRANCH`] if it already does what `commit`, the wanted flips on
/// top of `HEAD`, would do: the tip has one parent, that parent is on `main` (GitHub says it
/// is an ancestor of `HEAD`), and the tip's own change, its diff against that parent, replays
/// cleanly onto `HEAD` and gives `commit`'s tree. A tip on an older `main` holds, so `main`
/// moving on pushes nothing: the job runs on every push to `main`, and a push for each would
/// take the PR out of the merge queue every time, until it starves (#62). Other flips, any
/// other change, a conflict with `HEAD`, a parent that is not on `main`, or a tip without
/// exactly one parent do not hold.
fn held_tip(
    root: &Path,
    remote: &str,
    pulls: &dyn Pulls,
    commit: &str,
) -> Result<Option<String>, String> {
    let run = |arguments: &[&str]| {
        git(root, arguments, &[], None)
            .map_err(|error| format!("cannot read `{BOT_BRANCH}`: {error}"))
    };
    let reference = format!("refs/heads/{BOT_BRANCH}");
    let listed = run(&["ls-remote", remote, &reference])?;
    let Some(tip) = listed.split_whitespace().next() else {
        return Ok(None);
    };
    assert!(is_object_name(tip), "ls-remote names the tip: {listed}");
    // Depth 2: the tip and its parent, whose trees the replay needs, and none of `main`'s
    // history.
    let local = format!("+{reference}:refs/nami/bot-passes");
    let _ = run(&["fetch", "--quiet", "--no-tags", "--depth=2", remote, &local])?;
    let ours = run(&["cat-file", "commit", commit])?;
    let (tree, base) = one_parent(&ours).expect("the bot's commit has a tree and one parent");
    let head = run(&["rev-parse", "HEAD"])?;
    assert_eq!(base, head.trim(), "the bot's commit sits on HEAD");
    let theirs = run(&["cat-file", "commit", tip])?;
    let Some((_, parent)) = one_parent(&theirs) else {
        return Ok(None);
    };
    let replayed = replayed_tree(root, (parent, base, tip))?;
    if replayed.as_deref() != Some(tree) {
        return Ok(None);
    }
    // Asked last, and only of a tip that holds otherwise: the checkout has none of `main`'s
    // history, and GitHub has all of it.
    let on_main = pulls.is_ancestor(parent, base)?;
    Ok(on_main.then(|| tip.to_owned()))
}

/// The tree that `tip`'s own change, its diff against `parent`, gives once replayed onto
/// `base`; none when it conflicts there.
///
/// This is `git merge-tree --write-tree --merge-base=<parent> <base> <tip>`: git answers
/// whether the change still applies, and its tree, compared with the freshly built one, says
/// whether the change is exactly the wanted flips. Comparing the two diffs instead would need
/// their blobs and context to match, which fails whenever `main` changed a `tasks.json`
/// elsewhere. The merge base is named, not searched for, so the replay needs only the tip and
/// its parent, not `main`'s history, which the job's depth-1 checkout lacks. While only this
/// job writes the branch, that parent is a commit of `main`, and so the base GitHub merges
/// from too. A tip on any other parent would carry that parent's changes into the PR, so
/// [`held_tip`] keeps a tip only when GitHub says its parent is on `main`.
///
/// # Errors
///
/// When git fails for any other reason than a conflict.
fn replayed_tree(
    root: &Path,
    (parent, base, tip): (&str, &str, &str),
) -> Result<Option<String>, String> {
    assert!([parent, base, tip].into_iter().all(is_object_name), "three commits: {tip}");
    let merge_base = format!("--merge-base={parent}");
    let arguments = ["merge-tree", "--write-tree", "--no-messages", &merge_base, base, tip];
    let output = git_command(root, &arguments, &[]).stdin(Stdio::null()).output();
    let output = output.map_err(|error| format!("cannot run git merge-tree: {error}"))?;
    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let tree = stdout.lines().next().unwrap_or_default().to_owned();
            assert!(is_object_name(&tree), "a clean merge-tree prints its tree: {stdout}");
            Ok(Some(tree))
        }
        // git's exit status for a merge with conflicts.
        Some(1) => Ok(None),
        _ => Err(format!(
            "cannot replay `{BOT_BRANCH}` onto {base}: git merge-tree: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// True for a full hexadecimal object name, as git prints it.
fn is_object_name(name: &str) -> bool {
    name.len() >= 40 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The tree and the parent of a raw commit object that has exactly one parent; none for a
/// root or a merge.
fn one_parent(commit: &str) -> Option<(&str, &str)> {
    let shape = tree_and_parents(commit);
    let starts = shape.first().is_none_or(|line| line.starts_with("tree "));
    assert!(starts, "a commit object starts with its tree: {shape:?}");
    let [tree, parent] = shape.as_slice() else {
        return None;
    };
    Some((tree.strip_prefix("tree ")?, parent.strip_prefix("parent ")?))
}

/// The `tree` and `parent` lines of a raw commit object, as `git cat-file commit` prints it:
/// what the commit holds and where it sits, without its author, time or message.
#[must_use]
pub fn tree_and_parents(commit: &str) -> Vec<&str> {
    let header = commit.lines().take_while(|line| !line.is_empty());
    header.filter(|line| line.starts_with("tree ") || line.starts_with("parent ")).collect()
}

/// Brings the bot's PR `number` to `title` and `body`, with auto-merge on, as GitHub shows it
/// now. Returns what it changed. Neither write takes the PR out of the merge queue.
fn reconcile(
    pulls: &dyn Pulls,
    number: u32,
    title: &str,
    body: &str,
) -> Result<Vec<&'static str>, String> {
    let state = pulls.pull(number)?;
    if state.head.branch != BOT_BRANCH || state.head.author != CI_LOGIN {
        return Err(format!(
            "#{number} comes from `{}` by `{}`, not from {CI_LOGIN}'s `{BOT_BRANCH}`; a human \
             looks at it",
            state.head.branch, state.head.author
        ));
    }
    if !state.open {
        // It merged, or a human closed it, while this run worked: nothing is left to fix, and
        // the next run starts from the new `main`.
        let how = if state.merged { "merged" } else { "closed" };
        return Ok(vec![how, "nothing to reconcile"]);
    }
    let mut done = Vec::new();
    if state.title != title || state.body != body {
        pulls.edit(number, title, body)?;
        done.push("edited");
    }
    if !state.auto_merge {
        pulls.auto_merge(number)?;
        done.push("auto-merge on");
    }
    Ok(done)
}

/// Commits the working tree's changes to the plans on top of `HEAD`, through a private index,
/// so that neither the branch nor the index of the checkout moves, and at the committer time
/// `committed`, in git's date format, if one is given. Returns the commit.
fn commit_working_tree(
    root: &Path,
    title: &str,
    committed: Option<&str>,
) -> Result<String, String> {
    assert!(committed.is_none_or(|time| time.starts_with('@')), "a time in seconds: {committed:?}");
    let git_directory = git(root, &["rev-parse", "--absolute-git-dir"], &[], None);
    let git_directory = git_directory.map_err(|error| error.to_string())?;
    let index =
        Path::new(git_directory.trim()).join(format!("nami-passes-{}.index", std::process::id()));
    let index_text = index.to_string_lossy().into_owned();
    let mut environment = vec![("GIT_INDEX_FILE", index_text.as_str())];
    environment.extend(committed.map(|time| ("GIT_COMMITTER_DATE", time)));
    let build = || -> Result<String, String> {
        let run = |arguments: &[&str]| {
            git(root, arguments, &environment, None).map_err(|error| error.to_string())
        };
        let _ = run(&["read-tree", "HEAD"])?;
        let _ = run(&["add", "--update", "--", plan::PLANS_DIRECTORY])?;
        let tree = run(&["write-tree"])?;
        let message = format!("{title}\n\nSet by `cargo xtask task passes` (ADR-0011).\n");
        let commit = run(&["commit-tree", tree.trim(), "-p", "HEAD", "-m", &message])?;
        Ok(commit.trim().to_owned())
    };
    let result = build();
    // The private index is scratch: a failure to delete it changes nothing that is pushed.
    if let Err(error) = std::fs::remove_file(&index) {
        eprintln!("task: cannot delete {}: {error}", index.display());
    }
    let commit = result?;
    assert!(!commit.is_empty(), "commit-tree names the commit");
    Ok(commit)
}

/// The title and description of the PR that sets `passes` on `flipped`, found done at `head`.
#[must_use]
pub fn pull_text(tasks: &[Task], flipped: &[String], head: &str) -> (String, String) {
    assert!(!flipped.is_empty(), "the PR sets passes on at least one task");
    let title = format!("Set passes: {}", flipped.join(", "));
    let items: Vec<String> = tasks
        .iter()
        .filter(|task| flipped.contains(&task.id))
        .map(|task| {
            let tests: Vec<String> =
                task.acceptance.iter().map(|name| format!("`{name}`")).collect();
            format!("- `{}`: {}\n", task.id, tests.join(", "))
        })
        .collect();
    let list = items.concat();
    let body = format!(
        "## Summary\n\nThe post-merge workflow `passes` found these tasks done on \
         `{BASE_BRANCH}` at {head}: each one's PR from `task/<id>` has merged, its dependencies \
         pass, and its acceptance tests passed (ADR-0011, ADR-0017).\n\n{list}\nThis PR changes \
         only those `passes` fields. `cargo xtask task check-pr` runs their acceptance tests \
         again in this PR's own CI, and in the merge queue.\n\n## Rule changes\n\nNone\n\n## \
         Protected files\n\nNone\n\n## Follow-up issues\n\nNone\n"
    );
    let listed = |id: &String| list.contains(&format!("- `{id}`: "));
    assert!(flipped.iter().all(listed), "the description lists every flipped task: {flipped:?}");
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::{
        is_active_tasks_file, one_parent, only_passes_set, passing_from_json, passing_json,
        pull_number, pull_text, set_passes, unblocked,
    };
    use crate::task::plan::Task;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    const TASKS: &str = concat!(
        "{\n  \"tasks\": [\n",
        "    {\n      \"id\": \"a\",\n      \"passes\": false\n    },\n",
        "    {\n      \"id\": \"b\",\n      \"passes\": false\n    }\n",
        "  ]\n}\n"
    );

    fn ids(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    // Goal: exactly the named tasks' `passes` lines change, and a task that is not in the file
    // is an error, not a silent no-op.
    #[test]
    fn set_passes_changes_only_the_named_tasks() {
        let flipped = set_passes(TASKS, &ids(&["b"])).expect("b is in the file");
        assert_eq!(flipped, TASKS.replacen("false\n    }\n  ]", "true\n    }\n  ]", 1));
        assert!(flipped.contains("\"a\",\n      \"passes\": false"), "a is untouched");
        let error = set_passes(TASKS, &ids(&["b", "zz"])).expect_err("zz is not in the file");
        assert!(error.contains("zz"), "{error}");
        assert_eq!(set_passes(TASKS, &ids(&[])).as_deref(), Ok(TASKS), "nothing asked");
    }

    // Goal: only `"passes": false` lines that became `true` pass the shape check; any other
    // change, an added line, or a flip back to false is named.
    #[test]
    fn only_passes_changes_are_accepted() {
        let both = set_passes(TASKS, &ids(&["a", "b"])).expect("both are in the file");
        assert_eq!(only_passes_set("t.json", TASKS, &both), Ok(()));
        assert_eq!(only_passes_set("t.json", TASKS, TASKS), Ok(()), "no change at all");
        let renamed = both.replace("\"id\": \"a\"", "\"id\": \"c\"");
        let error = only_passes_set("t.json", TASKS, &renamed).expect_err("an id changed");
        assert!(error.contains("t.json") && error.contains("line 4"), "{error}");
        let back = only_passes_set("t.json", &both, TASKS).expect_err("true to false");
        assert!(back.contains("line 5"), "{back}");
        let longer = format!("{both}\n");
        assert!(only_passes_set("t.json", TASKS, &longer).expect_err("a line").contains("adds"));
        let spaced = TASKS.replacen("\"passes\": false", "\"passes\":  true", 1);
        assert!(only_passes_set("t.json", TASKS, &spaced).is_err(), "the line is exactly true");
    }

    // Goal: only an active plan's own tasks.json counts as a file that may set passes.
    #[test]
    fn active_tasks_files_are_recognised() {
        assert!(is_active_tasks_file("docs/exec-plans/active/guardrails/tasks.json"));
        for other in [
            "docs/exec-plans/completed/guardrails/tasks.json",
            "docs/exec-plans/active/tasks.json",
            "docs/exec-plans/active/a/b/tasks.json",
            "docs/exec-plans/active/guardrails/plan.md",
            "xtask/src/task.rs",
        ] {
            assert!(!is_active_tasks_file(other), "{other}");
        }
    }

    fn task(id: &str, depends_on: &[&str], passes: bool, active: bool) -> Task {
        Task {
            id: id.to_owned(),
            title: id.to_owned(),
            depends_on: depends_on.iter().map(|&dependency| dependency.to_owned()).collect(),
            owner_paths: Vec::new(),
            acceptance: vec![format!("x::{id}")],
            passes,
            file: PathBuf::from("docs/exec-plans/active/p/tasks.json"),
            active,
        }
    }

    // Goal: only active, unfinished tasks whose dependencies all pass are worth running.
    #[test]
    fn unblocked_tasks_are_unfinished_active_and_ready() {
        let tasks = [
            task("done", &[], true, true),
            task("ready", &["done"], false, true),
            task("blocked", &["ready"], false, true),
            task("old", &[], false, false),
            task("free", &[], false, true),
        ];
        let ids: Vec<&str> = unblocked(&tasks).iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, ["ready", "free"]);
    }

    // Goal: the file between the two jobs round-trips, and anything but distinct task IDs
    // under the one key `passing` is refused.
    #[test]
    fn the_passing_file_is_plain_data() {
        let text = passing_json(&["a", "b-2"]);
        assert_eq!(text, "{\"passing\": [\"a\", \"b-2\"]}\n");
        assert_eq!(passing_from_json(&text), Ok(ids(&["a", "b-2"])));
        assert_eq!(passing_from_json(&passing_json(&[])), Ok(ids(&[])));
        for bad in [
            "",
            "[]",
            "{\"passes\": []}",
            "{\"passing\": [], \"more\": []}",
            "{\"passing\": \"a\"}",
            "{\"passing\": [1]}",
            "{\"passing\": [\"A b\"]}",
            "{\"passing\": [\"a\", \"a\"]}",
        ] {
            let error = passing_from_json(bad).expect_err("refused");
            assert!(error.starts_with("the file must be {\"passing\""), "{bad}: {error}");
        }
    }

    // Goal: only a commit with exactly one parent has a change of its own to replay; a root
    // commit and a merge have none, so `bot/passes` is rebuilt from them.
    #[test]
    fn only_a_one_parent_commit_has_its_own_change() {
        let commit = "tree t1\nparent p1\nauthor a\n\nparent p9 in the message\n";
        assert_eq!(one_parent(commit), Some(("t1", "p1")));
        assert_eq!(one_parent("tree t1\nauthor a\n\nroot\n"), None, "a root commit");
        assert_eq!(one_parent("tree t1\nparent p1\nparent p2\n\nmerge\n"), None, "a merge");
    }

    // Goal: a pull request number is positive, and empty means none.
    #[test]
    fn pull_numbers_are_positive() {
        assert_eq!(pull_number(""), Ok(None));
        assert_eq!(pull_number("12"), Ok(Some(12)));
        for bad in ["0", "-1", "x", "1.5"] {
            assert!(pull_number(bad).is_err(), "{bad}");
        }
    }

    // Goal: the PR's title names the tasks, and its description lists each one's tests under
    // the template's headings, so that `cargo xtask pr-body` accepts it.
    #[test]
    fn the_pull_text_names_tasks_and_tests() {
        let tasks = [task("a", &[], false, true), task("b", &[], false, true)];
        let (title, body) = pull_text(&tasks, &["b".to_owned()], "abc123");
        assert_eq!(title, "Set passes: b");
        assert!(body.contains("at abc123") && body.contains("- `b`: `x::b`\n"), "{body}");
        assert!(!body.contains("`a`"), "{body}");
        for heading in ["## Summary", "## Rule changes", "## Protected files", "## Follow-up"] {
            assert!(body.contains(heading), "{heading}");
        }
    }
}
