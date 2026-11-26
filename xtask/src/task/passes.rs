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
//!   from `task/<id>`. It sets `passes` on exactly those, commits the change on top of `HEAD`,
//!   force-pushes the commit as [`BOT_BRANCH`], opens or updates that branch's PR and turns on
//!   auto-merge. When no task is left, it closes the open PR, if there is one. It runs no
//!   tests, so the bot's token never meets a test binary.
//! - `check-pr` lets a change set `passes` only when [`flip_problems`] finds nothing.
//!
//! The merged PR matters because acceptance tests can pass before the work is done: two tasks
//! may share a test, and a later task's test may already exist.

use super::acceptance::Acceptance;
use super::claims::git;
use super::plan::{self, Task};
use crate::issue::github::{Gh, string_at};
use crate::json::{self, Value};
use crate::queue::failure::{pull_of_message, queue_entry};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// The one branch whose PR may set `passes`.
pub const BOT_BRANCH: &str = "bot/passes";

/// The bot of the `nami-ci` GitHub App: the one author whose PR may set `passes`. Agents act
/// as `nami-agent[bot]`, so no agent can open that PR.
pub const CI_LOGIN: &str = "nami-ci[bot]";

/// The branch that tasks merge into.
const BASE_BRANCH: &str = "main";

/// The line that marks a task as not passing yet, as the plans write it.
const NOT_PASSING: &str = "\"passes\": false";

/// The same line once the task passes.
const PASSING: &str = "\"passes\": true";

/// The one key of the file that the `acceptance` job writes.
const PASSING_KEY: &str = "passing";

/// A pull request's head branch and author, as GitHub has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullHead {
    /// The head branch; `<owner>:<branch>` when it lives in another repository.
    pub branch: String,
    /// The login of whoever opened it.
    pub author: String,
}

/// What setting `passes` needs from GitHub's pull requests. Every error says what failed.
pub trait Pulls {
    /// The head branch and author of pull request `number`.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or does not have it.
    fn head(&self, number: u32) -> Result<PullHead, String>;

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
}

/// What `passes` and `check-pr` need from outside the checkout.
#[derive(Clone, Copy)]
pub struct Services<'services> {
    /// GitHub's pull requests.
    pub pulls: &'services dyn Pulls,
    /// The test runner.
    pub tests: &'services dyn Acceptance,
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
}

const USAGE: &str = "usage: cargo xtask task passes [--output <file> | --open-pr <file>] | \
                     check-pr <base> [<branch> [<pull request>]]";

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
    let change = |base, branch, pull| Change { base, branch, pull };
    match arguments {
        ["passes"] => found(root, services.tests, None),
        ["passes", "--output", file] => found(root, services.tests, Some(Path::new(file))),
        ["passes", "--open-pr", file] => open_pr(root, remote, services.pulls, Path::new(file)),
        ["check-pr", base] => super::check_pr(root, &change(base, "", None), services),
        ["check-pr", base, branch] => super::check_pr(root, &change(base, branch, None), services),
        ["check-pr", base, branch, pull] => {
            let pull = pull_number(pull)?;
            super::check_pr(root, &change(base, branch, pull), services)
        }
        _ => Err(USAGE.to_owned()),
    }
}

/// A pull request number from the command line; empty means none, as CI passes it in the
/// merge queue.
fn pull_number(text: &str) -> Result<Option<u32>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    let number = text.parse::<u32>().ok().filter(|number| *number > 0);
    let number = number.ok_or_else(|| format!("`{text}` is not a pull request number"))?;
    assert!(number > 0, "pull requests are numbered from 1");
    Ok(Some(number))
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
    let found: Vec<&Task> = tasks
        .iter()
        .filter(|task| task.active && !task.passes)
        .filter(|task| task.depends_on.iter().all(|id| passing.contains(id.as_str())))
        .collect();
    assert!(found.iter().all(|task| !task.passes), "only unfinished tasks are unblocked");
    found
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
        if pulls.has_merged(&task_branch(&task.id))? {
            found.push(task);
        }
    }
    assert!(found.iter().all(|task| task.active), "only active tasks are candidates");
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
    assert_eq!(ids.len(), entries.len(), "every entry is one distinct ID");
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
    assert!(problems.len() <= diff.lines().count(), "at most one problem per changed file");
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
    assert!(!parent.is_empty(), "rev-parse names the parent");
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
    let queued = queue_entry(change.branch).map(|entry| entry.pull);
    let Some(number) = change.pull.or(queued) else {
        return vec![format!(
            "`{}` is no merge-queue branch and no pull request was given, so this is not the \
             bot's PR from `{BOT_BRANCH}`",
            change.branch
        )];
    };
    let head = match pulls.head(number) {
        Ok(head) => head,
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

/// `passes --open-pr <file>`.
fn open_pr(root: &Path, remote: &str, pulls: &dyn Pulls, file: &Path) -> Result<String, String> {
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
    let open = pulls.open_from(BOT_BRANCH)?;
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
    let flipped: Vec<String> =
        after.iter().filter(|task| wanted.contains(&task.id)).map(|task| task.id.clone()).collect();
    assert_eq!(flipped.len(), wanted.len(), "every task to set is still in the plans");
    lines.push(publish(root, remote, pulls, (&after, &flipped), open)?);
    Ok(lines.concat())
}

/// Commits the working tree's change to `passes` as [`BOT_BRANCH`], pushes it, and opens or
/// updates the PR, `open` if there is one, that sets `passes` on `flipped`.
fn publish(
    root: &Path,
    remote: &str,
    pulls: &dyn Pulls,
    (tasks, flipped): (&[Task], &[String]),
    open: Option<u32>,
) -> Result<String, String> {
    assert!(!flipped.is_empty(), "a PR sets passes on at least one task");
    let head = git(root, &["rev-parse", "HEAD"], &[], None).map_err(|error| error.to_string())?;
    let (title, body) = pull_text(tasks, flipped, head.trim());
    let commit = commit_working_tree(root, &title)?;
    let target = format!("{commit}:refs/heads/{BOT_BRANCH}");
    let _ = git(root, &["push", "--quiet", "--force", remote, &target], &[], None)
        .map_err(|error| error.to_string())?;
    let number = match open {
        Some(number) => {
            pulls.edit(number, &title, &body)?;
            number
        }
        None => pulls.open(BOT_BRANCH, &title, &body)?,
    };
    pulls.auto_merge(number)?;
    assert!(number > 0, "pull requests are numbered from 1");
    Ok(format!("#{number} sets `passes` on {}\n", flipped.join(", ")))
}

/// Commits the working tree's changes to the plans on top of `HEAD`, through a private index,
/// so that neither the branch nor the index of the checkout moves. Returns the commit.
fn commit_working_tree(root: &Path, title: &str) -> Result<String, String> {
    let git_directory = git(root, &["rev-parse", "--absolute-git-dir"], &[], None);
    let git_directory = git_directory.map_err(|error| error.to_string())?;
    let index =
        Path::new(git_directory.trim()).join(format!("nami-passes-{}.index", std::process::id()));
    let index_text = index.to_string_lossy().into_owned();
    let environment = [("GIT_INDEX_FILE", index_text.as_str())];
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
    assert!(title.len() < body.len(), "the description says more than the title");
    (title, body)
}

impl Pulls for Gh {
    fn head(&self, number: u32) -> Result<PullHead, String> {
        let pull = self.one(&[&format!("repos/{{owner}}/{{repo}}/pulls/{number}")], None)?;
        head_from_json(&pull)
    }

    fn has_merged(&self, branch: &str) -> Result<bool, String> {
        let path = format!(
            "repos/{{owner}}/{{repo}}/pulls?state=closed&base={BASE_BRANCH}&head={{owner}}:\
             {branch}&per_page=100"
        );
        let pulls = self.list(&path)?;
        Ok(pulls.iter().any(|pull| pull.get("merged_at").is_some_and(|at| *at != Value::Null)))
    }

    fn open_from(&self, branch: &str) -> Result<Option<u32>, String> {
        let path = format!(
            "repos/{{owner}}/{{repo}}/pulls?state=open&head={{owner}}:{branch}&per_page=100"
        );
        let numbers: Result<Vec<u32>, String> = self.list(&path)?.iter().map(number_of).collect();
        match numbers?.as_slice() {
            [] => Ok(None),
            [number] => Ok(Some(*number)),
            many => Err(format!("GitHub lists {} open PRs from `{branch}`", many.len())),
        }
    }

    fn open(&self, branch: &str, title: &str, body: &str) -> Result<u32, String> {
        let request = format!(
            "{{\"title\": {}, \"head\": {}, \"base\": {}, \"body\": {}}}",
            json::quote(title),
            json::quote(branch),
            json::quote(BASE_BRANCH),
            json::quote(body)
        );
        let arguments = ["--method", "POST", "repos/{owner}/{repo}/pulls", "--input", "-"];
        number_of(&self.one(&arguments, Some(&request))?)
    }

    fn edit(&self, number: u32, title: &str, body: &str) -> Result<(), String> {
        let request =
            format!("{{\"title\": {}, \"body\": {}}}", json::quote(title), json::quote(body));
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}");
        let _ = self.api(&["--method", "PATCH", &path, "--input", "-"], Some(&request))?;
        Ok(())
    }

    fn close(&self, number: u32) -> Result<(), String> {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}");
        let request = "{\"state\": \"closed\"}";
        let _ = self.api(&["--method", "PATCH", &path, "--input", "-"], Some(request))?;
        Ok(())
    }

    fn auto_merge(&self, number: u32) -> Result<(), String> {
        let output = Command::new("gh")
            .args(["pr", "merge", &number.to_string(), "--auto", "--squash"])
            .current_dir(self.root())
            .output()
            .map_err(|error| format!("cannot run gh: {error}; install the GitHub CLI"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "`gh pr merge {number} --auto --squash` failed: {}; add #{number} to the merge queue \
             by hand",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Reads a pull request's head branch and author from GitHub's pulls API. A head in another
/// repository, or in one that was deleted, is written `<owner>:<branch>`.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn head_from_json(pull: &Value) -> Result<PullHead, String> {
    let branch = string_at(pull, &["head", "ref"])?;
    let base = string_at(pull, &["base", "repo", "full_name"])?;
    let head = string_at(pull, &["head", "repo", "full_name"]).ok();
    let branch = if head.as_deref() == Some(base.as_str()) {
        branch
    } else {
        string_at(pull, &["head", "label"]).unwrap_or_else(|_| format!("(unknown):{branch}"))
    };
    Ok(PullHead { branch, author: string_at(pull, &["user", "login"])? })
}

/// The `number` of a pull request in GitHub's pulls API.
fn number_of(pull: &Value) -> Result<u32, String> {
    let Some(Value::Number(digits)) = pull.get("number") else {
        return Err("a pull request has no `number`".to_owned());
    };
    let number = digits.parse::<u32>().ok().filter(|number| *number > 0);
    number.ok_or_else(|| format!("a pull request has the number `{digits}`"))
}

#[cfg(test)]
mod tests {
    use super::{
        PullHead, head_from_json, is_active_tasks_file, only_passes_set, passing_from_json,
        passing_json, pull_number, pull_text, set_passes, unblocked,
    };
    use crate::json;
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

    // Goal: a head in this repository gives its branch name, and a fork's gives
    // `<owner>:<branch>`, so a fork's `bot/passes` is never the bot's.
    #[test]
    fn heads_name_their_repository() {
        let pull = |head: &str| {
            json::parse(&format!(
                r#"{{"user": {{"login": "nami-agent[bot]"}}, "base": {{"repo": {{"full_name":
                "o/n"}}}}, "head": {{"ref": "bot/passes", "label": "x:bot/passes", "repo":
                {head}}}}}"#
            ))
            .expect("the fixture is JSON")
        };
        let own = head_from_json(&pull(r#"{"full_name": "o/n"}"#)).expect("valid");
        let expected =
            PullHead { branch: "bot/passes".to_owned(), author: "nami-agent[bot]".to_owned() };
        assert_eq!(own, expected);
        let fork = head_from_json(&pull(r#"{"full_name": "x/n"}"#)).expect("valid");
        assert_eq!(fork.branch, "x:bot/passes");
        assert_eq!(head_from_json(&pull("null")).expect("valid").branch, "x:bot/passes");
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
