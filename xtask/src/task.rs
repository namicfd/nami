//! `cargo xtask task`: plans, ready tasks and claims (ADR-0011), and issues claimed like tasks
//! (ADR-0021).
//!
//! - `status`: every active task with its state, then every claimed issue.
//! - `ready`: what can start now and nobody holds, one `id<TAB>title` per line: the plan tasks
//!   in plan order, then the issues that agents may work on, oldest first, as `issue-<number>`.
//! - `claim <id>` and `release <id>`: take or give back a task or an issue through the `claims`
//!   branch.
//! - `validate`: the plan checks that `cargo xtask tidy` also runs.
//! - `passes [--output <file> | --open-pr <file>]`: the post-merge workflow that sets `passes`
//!   through a PR from `bot/passes` (`passes`, ADR-0017).
//! - `check-pr <base> [<branch> [<pull request>]]`: CI's check that a change sets `passes`
//!   only as the `nami-ci` bot's PR from `bot/passes`, plus an advisory list of files outside
//!   the task's `owner_paths` when the branch is `task/<id>`.
//!
//! The remote is `origin`, or `$NAMI_TASK_REMOTE`. A claim records `$NAMI_AGENT_ID` (default:
//! git's `user.name`) and `$NAMI_SESSION_ID`. Issues come from GitHub through `gh`
//! (`crate::issue`).

pub mod acceptance;
pub mod claims;
pub mod passes;
pub mod plan;
pub mod pulls;

use crate::issue::github::{Gh, GitHub, Issue};
use crate::issue::{self, trust};
use crate::queue;
use acceptance::Nextest;
use claims::Claim;
use passes::{Change, Services};
use plan::Task;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask task status | ready | claim <id> | release <id> | \
                     validate | passes [--output <file> | --open-pr <file>] | check-pr <base> \
                     [<branch> [<pull request>]]";

/// Branches named `task/<id>` belong to that task.
const TASK_BRANCH_PREFIX: &str = "task/";

/// Runs one `cargo xtask task` command in this checkout: prints its output, or its error.
pub fn run(arguments: &[&str]) -> ExitCode {
    let root = crate::workspace_root();
    let remote = std::env::var("NAMI_TASK_REMOTE").unwrap_or_else(|_| "origin".to_owned());
    let github = Gh::new(&root);
    let result = match arguments.first() {
        Some(&("passes" | "check-pr")) => {
            let process = pulls::GhProcess::new(&root);
            let pulls = pulls::GitHubPulls::new(&process);
            let services = Services { pulls: &pulls, tests: &Nextest::new(&root) };
            passes::execute(&root, &remote, services, arguments)
        }
        _ => execute(&root, &remote, &github, arguments),
    };
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("task: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one command against the checkout at `root`, the claims on `remote` and the issues on
/// `github`. Returns what the command prints on success, or the error to report. `passes`
/// and `check-pr` are [`passes::execute`]'s.
///
/// # Errors
///
/// Returns the reason the command failed: invalid plans, a task or issue that cannot be
/// claimed, or a git or GitHub failure.
pub fn execute(
    root: &Path,
    remote: &str,
    github: &dyn GitHub,
    arguments: &[&str],
) -> Result<String, String> {
    if let ["claim" | "release", id] = arguments {
        check_task_id(id)?;
    }
    match arguments {
        ["status"] => status(root, remote, github),
        ["ready"] => ready(root, remote, github),
        ["claim", id] => claim(root, remote, github, id),
        ["release", id] => claims::release(root, remote, &[], id)
            .map(|()| format!("released {id}\n"))
            .map_err(|error| format!("cannot release `{id}`: {error}")),
        ["validate"] => valid_tasks(root).map(|_| "tasks are valid\n".to_owned()),
        _ => Err(USAGE.to_owned()),
    }
}

/// A task ID given on the command line is user input: reject what cannot be an ID.
fn check_task_id(id: &str) -> Result<(), String> {
    if !plan::is_valid_id(id) {
        return Err(format!("`{id}` is not a task id (lowercase letters, digits and dashes)"));
    }
    issue::number_of_id(id).map(|_| ())
}

/// Where a task stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// CI has marked it as passing.
    Done,
    /// Someone holds it.
    Claimed(Claim),
    /// Its dependencies pass and nobody holds it.
    Ready,
    /// Waiting for these dependencies.
    Blocked(Vec<String>),
}

/// The state of every task, in order.
#[must_use]
pub fn states(tasks: &[Task], claims: &[Claim]) -> Vec<State> {
    let passing: BTreeSet<&str> =
        tasks.iter().filter(|task| task.passes).map(|task| task.id.as_str()).collect();
    let claimed: BTreeMap<&str, &Claim> =
        claims.iter().map(|claim| (claim.task.as_str(), claim)).collect();
    let result: Vec<State> = tasks
        .iter()
        .map(|task| {
            if task.passes {
                return State::Done;
            }
            if let Some(&claim) = claimed.get(task.id.as_str()) {
                return State::Claimed(claim.clone());
            }
            let waiting: Vec<String> = task
                .depends_on
                .iter()
                .filter(|dependency| !passing.contains(dependency.as_str()))
                .cloned()
                .collect();
            if waiting.is_empty() { State::Ready } else { State::Blocked(waiting) }
        })
        .collect();
    assert_eq!(result.len(), tasks.len(), "one state per task");
    result
}

/// Loads the tasks and fails if any plan is invalid.
fn valid_tasks(root: &Path) -> Result<Vec<Task>, String> {
    let (tasks, violations) =
        plan::load(root).map_err(|error| format!("cannot read the plans: {error}"))?;
    if violations.is_empty() {
        return Ok(tasks);
    }
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    Err(format!("{}\n{} problem(s) in the plans; fix them first", report.join("\n"), report.len()))
}

/// Every active task with its state, and every claim on the claims branch.
type Snapshot = (Vec<(Task, State)>, Vec<Claim>);

/// The current [`Snapshot`]. States are computed over all plans, so a dependency on a task in
/// a completed plan counts as passing.
fn snapshot(root: &Path, remote: &str) -> Result<Snapshot, String> {
    let tasks = valid_tasks(root)?;
    let tip = claims::fetch(root, remote).map_err(|error| error.to_string())?;
    let current = claims::list(root, tip.as_deref()).map_err(|error| error.to_string())?;
    let all_states = states(&tasks, &current);
    let active: Vec<(Task, State)> =
        tasks.into_iter().zip(all_states).filter(|(task, _)| task.active).collect();
    Ok((active, current))
}

fn status(root: &Path, remote: &str, github: &dyn GitHub) -> Result<String, String> {
    let (tasks, current) = snapshot(root, remote)?;
    let mut lines = Vec::with_capacity(tasks.len());
    for (task, state) in tasks {
        let description = match state {
            State::Done => "done".to_owned(),
            State::Claimed(claim) => claimed(&claim),
            State::Ready => "ready".to_owned(),
            State::Blocked(waiting) => format!("blocked on {}", waiting.join(", ")),
        };
        lines.push(format!("{:<32} {:<10} {}\n", task.id, description, task.title));
    }
    let repository = issue::github::repository_of(root, remote);
    for claim in &current {
        let Ok(Some(number)) = issue::number_of_id(&claim.task) else {
            continue;
        };
        // Titles are not printed: whether an issue is trusted is `issue show`'s business.
        let description = match github.issue(number) {
            Ok(found) if !found.is_open => "done: the issue is closed".to_owned(),
            Ok(found) => match queue_failure_hold(github, &found, repository.as_deref()) {
                Ok(Some(reason)) => format!("{}; {reason}", claimed(claim)),
                Ok(None) => claimed(claim),
                Err(error) => format!("{}; its fix's state is unknown: {error}", claimed(claim)),
            },
            Err(error) => format!("{}; its state is unknown: {error}", claimed(claim)),
        };
        lines.push(format!("{:<32} {description}\n", claim.task));
    }
    Ok(lines.concat())
}

fn claimed(claim: &Claim) -> String {
    let holder = claim.holder.replace('\n', ", ");
    format!("claimed {} ({}) by {holder}", claim.age, claim.claimed_at)
}

fn ready(root: &Path, remote: &str, github: &dyn GitHub) -> Result<String, String> {
    let (tasks, current) = snapshot(root, remote)?;
    let ready: Vec<String> = tasks
        .iter()
        .filter(|(_, state)| *state == State::Ready)
        .map(|(task, _)| format!("{}\t{}\n", task.id, task.title))
        .collect();
    let ready = ready.concat();
    match ready_issues(root, remote, github, &current) {
        Ok((issues, left_out)) if left_out.is_empty() => Ok(ready + &issues),
        Ok((issues, left_out)) => Err(format!(
            "{} issue(s) left out because their state is unknown or cannot be checked, so this \
             list is incomplete; the rest of the list:\n{ready}{issues}",
            left_out.len()
        )),
        Err(error) => Err(format!(
            "cannot list the issues, so this list is incomplete: {error}\nthe plan tasks that \
             are ready:\n{ready}"
        )),
    }
}

/// The issues that agents may work on and nobody holds, oldest first, as `ready` lines, and
/// the numbers of the issues left out because their state is unknown (a lookup failed) or
/// cannot be checked (a CI app's issue without the scope to check it). Each of those is named
/// on stderr; one failed lookup leaves out only its own issue.
fn ready_issues(
    root: &Path,
    remote: &str,
    github: &dyn GitHub,
    current: &[Claim],
) -> Result<(String, Vec<u32>), String> {
    let open = github.open_issues()?;
    let candidates: Vec<_> = open.iter().filter(|found| !found.is_pull_request).collect();
    if candidates.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let trust = trust::load(root, &format!("{remote}/main"))?;
    trust::decision_label_exists(&github.repository_labels()?)?;
    let repository = issue::github::repository_of(root, remote);
    let held: BTreeSet<&str> = current.iter().map(|claim| claim.task.as_str()).collect();
    let mut lines = Vec::new();
    let mut left_out = Vec::new();
    for found in candidates {
        let id = issue::id_of(found.number);
        // An issue by the CI app that cannot be checked here is left out loudly, not silently:
        // it may be work, and the reason says what to fix.
        let unchecked = (found.author == trust::CI_LOGIN).then(|| trust.ci_unavailable());
        if let Some(Some(reason)) = unchecked.filter(|_| !held.contains(id.as_str())) {
            eprintln!(
                "task: {id} is left out of the list: the CI app's issues cannot be checked \
                 here: {reason}"
            );
            left_out.push(found.number);
            continue;
        }
        // The cheap checks first: label events cost one request per issue, and comments one
        // more when the bot removed `needs-decision` last.
        if held.contains(id.as_str()) || trust::unworkable(found, &[], &[], &trust).is_some() {
            continue;
        }
        match is_workable(github, found, &trust, repository.as_deref()) {
            Ok(true) => lines.push(format!("{id}\t{}\n", found.title)),
            Ok(false) => {}
            Err(error) => {
                eprintln!("task: {id} is left out of the list: {error}");
                left_out.push(found.number);
            }
        }
    }
    assert!(lines.len() + left_out.len() <= open.len(), "each open issue is listed at most once");
    Ok((lines.concat(), left_out))
}

/// True if agents may work on `found` now: its label history allows it, and it is no
/// queue-failure issue that waits (ADR-0021, point 6).
fn is_workable(
    github: &dyn GitHub,
    found: &Issue,
    trust: &trust::Trust,
    repository: Option<&str>,
) -> Result<bool, String> {
    let (events, comments) = issue::history(github, found)?;
    if trust::unworkable(found, &events, &comments, trust).is_some() {
        return Ok(false);
    }
    Ok(queue_failure_hold(github, found, repository)?.is_none())
}

fn claim(root: &Path, remote: &str, github: &dyn GitHub, id: &str) -> Result<String, String> {
    if let Some(number) = issue::number_of_id(id)? {
        return claim_issue(root, remote, github, id, number);
    }
    let (tasks, _) = snapshot(root, remote)?;
    let (_, state) = tasks
        .into_iter()
        .find(|(task, _)| task.id == id)
        .ok_or_else(|| format!("no active task `{id}`"))?;
    match state {
        State::Ready => {}
        State::Done => return Err(format!("`{id}` is already done")),
        State::Claimed(claim) => return Err(format!("`{id}` is claimed by {}", claim.holder)),
        State::Blocked(waiting) => {
            return Err(format!("`{id}` is blocked on {}", waiting.join(", ")));
        }
    }
    let holder = holder(root);
    claims::claim(root, remote, &[], id, &holder)
        .map_err(|error| format!("cannot claim `{id}`: {error}"))?;
    Ok(format!("claimed {id}\n"))
}

/// Claims issue `number`, whose claim ID is `id`, if agents may work on it (ADR-0021).
fn claim_issue(
    root: &Path,
    remote: &str,
    github: &dyn GitHub,
    id: &str,
    number: u32,
) -> Result<String, String> {
    assert_eq!(issue::id_of(number), id, "the ID names the issue");
    let (_, current) = snapshot(root, remote)?;
    if let Some(claim) = current.iter().find(|claim| claim.task == id) {
        return Err(format!("`{id}` is claimed by {}", claim.holder));
    }
    let found = github.issue(number)?;
    let trust = trust::load(root, &format!("{remote}/main"))?;
    trust::decision_label_exists(&github.repository_labels()?)
        .map_err(|why| format!("cannot claim `{id}`: {why}"))?;
    let (events, comments) = issue::history(github, &found)?;
    if let Some(reason) = trust::unworkable(&found, &events, &comments, &trust) {
        return Err(format!("cannot claim `{id}`: {reason}"));
    }
    let repository = issue::github::repository_of(root, remote);
    if let Some(reason) = queue_failure_hold(github, &found, repository.as_deref())? {
        return Err(format!("cannot claim `{id}`: {reason}"));
    }
    let holder = holder(root);
    claims::claim(root, remote, &[], id, &holder)
        .map_err(|error| format!("cannot claim `{id}`: {error}"))?;
    let work = match queue_failure_pull(&found) {
        Some(pull) => format!(
            "fix it on #{pull}'s own branch, add `Fixes #{number}` to #{pull}'s description, \
             re-queue #{pull}, and keep the claim: #{pull}'s merge closes the issue (ADR-0021, \
             point 6)"
        ),
        None => format!(
            "work on the branch `task/{id}`, and write `Fixes #{number}` in the PR \
                         description"
        ),
    };
    Ok(format!("claimed {id}\nread it with `cargo xtask issue show {number}`, {work}\n"))
}

/// The PR that `found` reports as failed in the merge queue, if the bot or the CI app filed it
/// with the title that `cargo xtask queue failed` gives (ADR-0021, point 6; ADR-0023).
fn queue_failure_pull(found: &Issue) -> Option<u32> {
    let entry = queue::failure::entry_of_title(&found.title);
    let by_filer = [trust::BOT_LOGIN, trust::CI_LOGIN].contains(&found.author.as_str());
    entry.map(|(pull, _)| pull).filter(|_| by_filer)
}

/// Why agents may not work on the queue-failure issue `found` now, or `None` if they may or it
/// is no queue-failure issue (ADR-0021, point 6):
///
/// - the bot did not open the PR: agents never push to a human's PR, so a human rebases it.
///   This holds whatever the labels say, so lifting `needs-decision` never releases it;
/// - the PR it names is open and its description closes the issue: the fix is in flight, and
///   the PR's merge closes the issue;
/// - the PR is closed or merged while the issue is open, or the number is no PR: nobody can
///   push a fix there, and a human closes the issue;
/// - the PR's branch is not in this repository, or cannot be looked up: agents check out and
///   push only the bot's own branches here, whatever the issue's labels say (ADR-0023), and a
///   human handles it;
/// - the PR is in the merge queue: a push would take it out again. If it fails there, the
///   new run gets its own issue.
///
/// `repository` is this repository's `owner/name`, for `Fixes <owner>/<name>#<n>`. Only whether
/// the description closes the issue is read, never its text.
fn queue_failure_hold(
    github: &dyn GitHub,
    found: &Issue,
    repository: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(pull) = queue_failure_pull(found) else {
        return Ok(None);
    };
    let number = found.number;
    let fix = github.issue(pull)?;
    let reason = if !fix.is_pull_request {
        format!("#{pull}, which issue #{number} names, is not a pull request; a human closes it")
    } else if fix.author != trust::BOT_LOGIN {
        // Held whatever the labels say: `needs-decision` is not what keeps agents off it.
        format!(
            "#{pull} was not opened by the bot, so agents never push to it; a human rebases or \
             renumbers it, re-queues it and closes issue #{number}"
        )
    } else if !fix.is_open {
        format!("#{pull} is closed or merged, but issue #{number} is still open; a human closes it")
    } else if !github.head_is_here(pull)? {
        format!(
            "#{pull}'s branch is not in this repository (a fork's, or deleted), so agents do not \
             check it out or push to it; a human handles #{pull} and closes issue #{number}"
        )
    } else if crate::pr::closes(&fix.body, number, repository) {
        format!(
            "#{pull} says `Fixes #{number}`: the fix is in flight and #{pull}'s merge closes the \
             issue, so take other work"
        )
    } else if github.is_queued(pull)? {
        format!(
            "#{pull} is in the merge queue, and agents never push to a queued PR; take other \
             work, and if #{pull} fails again, CI files a new issue"
        )
    } else {
        return Ok(None);
    };
    Ok(Some(format!("{reason} (ADR-0021, point 6)")))
}

/// The text of a claim file: who holds the task and from which session.
fn holder(root: &Path) -> String {
    let agent = std::env::var("NAMI_AGENT_ID").ok().filter(|value| !value.is_empty());
    let agent = agent.or_else(|| {
        let name = claims::git(root, &["config", "user.name"], &[], None).ok()?;
        Some(name.trim().to_owned()).filter(|name| !name.is_empty())
    });
    let session = std::env::var("NAMI_SESSION_ID").unwrap_or_else(|_| "-".to_owned());
    format!("agent: {}\nsession: {session}", agent.unwrap_or_else(|| "unknown".to_owned()))
}

/// CI: fail if the change sets `passes` on any task, unless it is the `nami-ci` bot's PR that
/// [`passes::flip_problems`] accepts, and list the files outside the task's `owner_paths` when
/// the branch is `task/<id>` (advisory). The task's definition comes from the base, so a
/// branch cannot widen its own `owner_paths`. In the merge queue, the base is the parent of
/// the PR's own commit ([`passes::own_base`]).
fn check_pr(root: &Path, change: &Change<'_>, services: Services<'_>) -> Result<String, String> {
    let (head_tasks, _) =
        plan::load(root).map_err(|error| format!("cannot read the plans: {error}"))?;
    let base = passes::own_base(root, change.base, change.branch)?;
    let base_tasks = tasks_at(root, &base)?;
    let base_passing: BTreeSet<String> =
        base_tasks.iter().filter(|task| task.passes).map(|task| task.id.clone()).collect();
    let flipped = passes_flips(&head_tasks, &base_passing);
    let mut output = String::new();
    // An issue has no owner paths: its PR is one small change (ADR-0021).
    let task_id = change.branch.strip_prefix(TASK_BRANCH_PREFIX);
    if let Some(id) = task_id.filter(|id| !id.starts_with(issue::ID_PREFIX)) {
        let changed = claims::git(root, &["diff", "--name-only", &base, "HEAD"], &[], None)
            .map_err(|error| error.to_string())?;
        let changed: Vec<&str> = changed.lines().collect();
        let warnings = outside_owner_paths(&base_tasks, id, &changed);
        let lines: Vec<String> =
            warnings.iter().map(|warning| format!("warning: {warning}\n")).collect();
        output = lines.concat();
    }
    if flipped.is_empty() {
        let problems = passes::no_flip_problems(change, services.pulls);
        if problems.is_empty() {
            return Ok(output);
        }
        let errors: Vec<String> =
            problems.iter().map(|problem| format!("error: {problem}")).collect();
        return Err(format!("{output}{}", errors.join("\n")));
    }
    let own = Change { base: &base, ..*change };
    let problems = passes::flip_problems(root, &own, &head_tasks, &flipped, services);
    if problems.is_empty() {
        return Ok(format!(
            "{output}the bot's PR from `{}` sets `passes` on {}: their acceptance tests passed \
             in this run\n",
            passes::BOT_BRANCH,
            flipped.join(", ")
        ));
    }
    let errors: Vec<String> = problems.iter().map(|problem| format!("error: {problem}")).collect();
    Err(format!(
        "{output}{}\nthis change sets `passes` on {}; only the bot's PR from `{}` may, and agents \
         never do (ADR-0011, ADR-0017)",
        errors.join("\n"),
        flipped.join(", "),
        passes::BOT_BRANCH
    ))
}

/// Every task in the plans at commit `base`.
fn tasks_at(root: &Path, base: &str) -> Result<Vec<Task>, String> {
    let files = claims::git(
        root,
        &["ls-tree", "-r", "--name-only", base, "--", plan::PLANS_DIRECTORY],
        &[],
        None,
    )
    .map_err(|error| error.to_string())?;
    let mut tasks = Vec::new();
    for file in files.lines().filter(|file| file.ends_with("/tasks.json")) {
        let text = claims::git(root, &["show", &format!("{base}:{file}")], &[], None)
            .map_err(|error| error.to_string())?;
        tasks.extend(plan::parse_tasks(Path::new(file), &text, true).0);
    }
    Ok(tasks)
}

/// Tasks that pass now but did not pass at the base.
#[must_use]
pub fn passes_flips(head: &[Task], base_passing: &BTreeSet<String>) -> Vec<String> {
    let flipped: Vec<String> = head
        .iter()
        .filter(|task| task.passes && !base_passing.contains(&task.id))
        .map(|task| task.id.clone())
        .collect();
    assert!(flipped.len() <= head.len(), "at most every task flips");
    flipped
}

/// Advisory: changed files that lie outside task `id`'s owner paths, as defined in `tasks`.
/// The task's `plan.md` is exempt, so that a task may record notes there; its `tasks.json` is
/// not, so that a change to the task's own definition is always reported.
#[must_use]
pub fn outside_owner_paths(tasks: &[Task], id: &str, changed: &[&str]) -> Vec<String> {
    let Some(task) = tasks.iter().find(|task| task.id == id) else {
        return vec![format!("the branch names task `{id}`, which does not exist at the base")];
    };
    let plan_file = task.file.with_file_name("plan.md").to_string_lossy().into_owned();
    let outside: Vec<String> = changed
        .iter()
        .filter(|file| !task.owner_paths.iter().any(|owned| plan::overlaps(file, owned)))
        .filter(|file| **file != plan_file)
        .map(|file| format!("`{file}` is outside the owner_paths of `{id}` (ADR-0011)"))
        .collect();
    assert!(outside.len() <= changed.len(), "at most every changed file is outside");
    outside
}

#[cfg(test)]
mod tests {
    use super::{State, outside_owner_paths, passes_flips, states};
    use crate::task::claims::Claim;
    use crate::task::plan::Task;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn task(id: &str, depends_on: &[&str], passes: bool) -> Task {
        Task {
            id: id.to_owned(),
            title: id.to_uppercase(),
            depends_on: depends_on.iter().map(|&d| d.to_owned()).collect(),
            owner_paths: vec![format!("crates/{id}")],
            acceptance: vec![format!("{id}::works")],
            passes,
            file: PathBuf::from("docs/exec-plans/active/p/tasks.json"),
            active: true,
        }
    }

    // Goal: done, claimed, ready and blocked are told apart; a claim outranks readiness and
    // only passing dependencies unblock a task.
    #[test]
    fn states_follow_dependencies_and_claims() {
        let tasks = [
            task("a", &[], true),
            task("b", &["a"], false),
            task("c", &["a", "b"], false),
            task("d", &[], false),
        ];
        let claim = Claim {
            task: "d".to_owned(),
            holder: "agent: x".to_owned(),
            claimed_at: "2026-11-25T00:00:00Z".to_owned(),
            age: "now".to_owned(),
        };
        let result = states(&tasks, std::slice::from_ref(&claim));
        assert_eq!(
            result,
            [
                State::Done,
                State::Ready,
                State::Blocked(vec!["b".to_owned()]),
                State::Claimed(claim)
            ]
        );
    }

    // Goal: only tasks that pass now and did not pass at the base count as flipped.
    #[test]
    fn passes_flips_are_detected() {
        let head = [task("a", &[], true), task("b", &[], true), task("c", &[], false)];
        let base: BTreeSet<String> = ["a".to_owned()].into();
        assert_eq!(passes_flips(&head, &base), ["b"]);
        assert_eq!(
            passes_flips(&head, &["a".to_owned(), "b".to_owned()].into()),
            Vec::<String>::new()
        );
    }

    // Goal: files outside the task's owner paths are listed, except the plan's own file; an
    // unknown task is reported.
    #[test]
    fn owner_path_warnings() {
        let tasks = [task("a", &[], false)];
        let changed = [
            "crates/a/src/lib.rs",
            "crates/b/src/lib.rs",
            "docs/exec-plans/active/p/plan.md",
            "docs/exec-plans/active/p/tasks.json",
        ];
        let warnings = outside_owner_paths(&tasks, "a", &changed);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("crates/b/src/lib.rs"), "the outside file is named");
        assert!(warnings[1].contains("tasks.json"), "editing the task list is always reported");
        assert!(
            outside_owner_paths(&tasks, "zz", &changed)[0].contains("does not exist"),
            "unknown task"
        );
    }
}
