//! `cargo xtask task`: plans, ready tasks and claims (ADR-0011), and issues claimed like tasks
//! (ADR-0021).
//!
//! - `status`: every active task with its state, then every claimed issue.
//! - `ready`: what can start now and nobody holds, one `id<TAB>title` per line: the plan tasks
//!   in plan order, then the issues that agents may work on, oldest first, as `issue-<number>`.
//! - `claim <id>` and `release <id>`: take or give back a task or an issue through the `claims`
//!   branch.
//! - `validate`: the plan checks that `cargo xtask tidy` also runs.
//! - `check-pr <base> [<branch>]`: CI's check that a change never sets `passes`, plus an
//!   advisory list of files outside the task's `owner_paths` when the branch is `task/<id>`.
//!
//! The remote is `origin`, or `$NAMI_TASK_REMOTE`. A claim records `$NAMI_AGENT_ID` (default:
//! git's `user.name`) and `$NAMI_SESSION_ID`. Issues come from GitHub through `gh`
//! (`crate::issue`).

pub mod claims;
pub mod plan;

use crate::issue::github::{Gh, GitHub};
use crate::issue::{self, trust};
use claims::Claim;
use plan::Task;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask task status | ready | claim <id> | release <id> | \
                     validate | check-pr <base> [<branch>]";

/// Branches named `task/<id>` belong to that task.
const TASK_BRANCH_PREFIX: &str = "task/";

/// Runs one `cargo xtask task` command in this checkout: prints its output, or its error.
pub fn run(arguments: &[&str]) -> ExitCode {
    let root = crate::workspace_root();
    let remote = std::env::var("NAMI_TASK_REMOTE").unwrap_or_else(|_| "origin".to_owned());
    match execute(&root, &remote, &Gh::new(&root), arguments) {
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
/// `github`. Returns what the command prints on success, or the error to report.
///
/// # Errors
///
/// Returns the reason the command failed: invalid plans, a task or issue that cannot be
/// claimed, a git or GitHub failure, or a change that sets `passes`.
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
        ["release", id] => claims::release(root, remote, id)
            .map(|()| format!("released {id}\n"))
            .map_err(|error| format!("cannot release `{id}`: {error}")),
        ["validate"] => valid_tasks(root).map(|_| "tasks are valid\n".to_owned()),
        ["check-pr", base] => check_pr(root, base, ""),
        ["check-pr", base, branch] => check_pr(root, base, branch),
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
    for claim in &current {
        let Ok(Some(number)) = issue::number_of_id(&claim.task) else {
            continue;
        };
        // Titles are not printed: whether an issue is trusted is `issue show`'s business.
        let description = match github.issue(number) {
            Ok(found) if !found.is_open => "done: the issue is closed".to_owned(),
            Ok(_) => claimed(claim),
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
        Ok(issues) => Ok(ready + &issues),
        Err(error) => Err(format!(
            "cannot list the issues, so this list is incomplete: {error}\nthe plan tasks that \
             are ready:\n{ready}"
        )),
    }
}

/// The issues that agents may work on and nobody holds, oldest first, as `ready` lines.
fn ready_issues(
    root: &Path,
    remote: &str,
    github: &dyn GitHub,
    current: &[Claim],
) -> Result<String, String> {
    let open = github.open_issues()?;
    let candidates: Vec<_> = open.iter().filter(|found| !found.is_pull_request).collect();
    if candidates.is_empty() {
        return Ok(String::new());
    }
    let trust = trust::load(root, &format!("{remote}/main"))?;
    let held: BTreeSet<&str> = current.iter().map(|claim| claim.task.as_str()).collect();
    let mut lines = Vec::new();
    for found in candidates {
        let id = issue::id_of(found.number);
        // The cheap checks first: label events cost one request per issue.
        if held.contains(id.as_str()) || trust::unworkable(found, &[], &trust).is_some() {
            continue;
        }
        let events = github.label_events(found.number)?;
        if trust::unworkable(found, &events, &trust).is_none() {
            lines.push(format!("{id}\t{}\n", found.title));
        }
    }
    assert!(lines.len() <= open.len(), "at most every open issue is ready");
    Ok(lines.concat())
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
    claims::claim(root, remote, id, &holder)
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
    let events = github.label_events(number)?;
    if let Some(reason) = trust::unworkable(&found, &events, &trust) {
        return Err(format!("cannot claim `{id}`: {reason}"));
    }
    let holder = holder(root);
    claims::claim(root, remote, id, &holder)
        .map_err(|error| format!("cannot claim `{id}`: {error}"))?;
    Ok(format!(
        "claimed {id}\nread it with `cargo xtask issue show {number}`, work on the branch \
         `task/{id}`, and write `Fixes #{number}` in the PR description\n"
    ))
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

/// CI: fail if the change sets `passes` on any task, and list the files outside the task's
/// `owner_paths` when the branch is `task/<id>` (advisory). The task's definition comes from
/// `base`, so a branch cannot widen its own `owner_paths`.
fn check_pr(root: &Path, base: &str, branch: &str) -> Result<String, String> {
    let (head_tasks, _) =
        plan::load(root).map_err(|error| format!("cannot read the plans: {error}"))?;
    let base_tasks = tasks_at(root, base)?;
    let base_passing: BTreeSet<String> =
        base_tasks.iter().filter(|task| task.passes).map(|task| task.id.clone()).collect();
    let flipped = passes_flips(&head_tasks, &base_passing);
    let mut output = String::new();
    // An issue has no owner paths: its PR is one small change (ADR-0021).
    let task_id = branch.strip_prefix(TASK_BRANCH_PREFIX);
    if let Some(id) = task_id.filter(|id| !id.starts_with(issue::ID_PREFIX)) {
        let changed = claims::git(root, &["diff", "--name-only", base, "HEAD"], &[], None)
            .map_err(|error| error.to_string())?;
        let changed: Vec<&str> = changed.lines().collect();
        let warnings = outside_owner_paths(&base_tasks, id, &changed);
        let lines: Vec<String> =
            warnings.iter().map(|warning| format!("warning: {warning}\n")).collect();
        output = lines.concat();
    }
    if flipped.is_empty() {
        return Ok(output);
    }
    let errors: Vec<String> = flipped
        .iter()
        .map(|id| {
            format!("error: this change sets `passes` on `{id}`; agents never set it (ADR-0011)")
        })
        .collect();
    Err(format!("{output}{}\n{} task(s) had `passes` set", errors.join("\n"), flipped.len()))
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
