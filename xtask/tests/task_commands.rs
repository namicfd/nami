//! `cargo xtask task` commands end to end, against a real git checkout (ADR-0011), with
//! GitHub issues from a fake (ADR-0021).

use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::issue::github::{Comment, GitHub, Issue, LabelEvent};
use xtask::task::execute;

/// GitHub with fixed issues and label events, or unreachable.
#[derive(Default)]
struct Fake {
    issues: Vec<Issue>,
    events: Vec<(u32, LabelEvent)>,
    is_down: bool,
}

impl GitHub for Fake {
    fn issue(&self, number: u32) -> Result<Issue, String> {
        let found = self.issues.iter().find(|issue| issue.number == number).cloned();
        found.filter(|_| !self.is_down).ok_or_else(|| format!("no issue #{number}"))
    }
    fn open_issues(&self) -> Result<Vec<Issue>, String> {
        let open = self.issues.iter().filter(|issue| issue.is_open).cloned().collect();
        if self.is_down { Err("GitHub is down".to_owned()) } else { Ok(open) }
    }
    fn comments(&self, _number: u32) -> Result<Vec<Comment>, String> {
        Ok(Vec::new())
    }
    fn label_events(&self, number: u32) -> Result<Vec<LabelEvent>, String> {
        let events = self.events.iter().filter(|(of, _)| *of == number);
        Ok(events.map(|(_, event)| event.clone()).collect())
    }
    fn create(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<u32, String> {
        Err("the task commands never file issues".to_owned())
    }
}

fn issue(number: u32, author: &str, labels: &[&str]) -> Issue {
    Issue {
        number,
        title: format!("Issue {number}"),
        author: author.to_owned(),
        is_open: true,
        is_pull_request: false,
        labels: labels.iter().map(|label| (*label).to_owned()).collect(),
        body: String::new(),
    }
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git").args(arguments).current_dir(directory).output();
    let output = output.expect("git runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "git {arguments:?} failed: {stderr}");
    String::from_utf8(output.stdout).expect("git prints UTF-8")
}

fn tasks_json(a_passes: bool) -> String {
    format!(
        r#"{{"tasks": [
  {{"id": "a", "title": "Task a", "depends_on": [], "owner_paths": ["src/a"],
    "acceptance": ["a::works"], "passes": {a_passes}}},
  {{"id": "b", "title": "Task b", "depends_on": ["a"], "owner_paths": ["src/b"],
    "acceptance": ["b::works"], "passes": false}}
]}}"#
    )
}

/// A bare remote and one clone holding a committed plan with tasks `a` and `b` (b needs a).
fn setup(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("nami-task-{name}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).expect("a stale temporary directory is removable");
    }
    std::fs::create_dir_all(&root).expect("temporary directory is writable");
    let _ = git(&root, &["init", "--quiet", "--bare", "remote.git"]);
    let _ = git(&root, &["clone", "--quiet", "remote.git", "clone"]);
    let clone = root.join("clone");
    for (key, value) in [("user.name", "tester"), ("user.email", "t@example.invalid")] {
        let _ = git(&clone, &["config", key, value]);
    }
    let _ = git(&clone, &["config", "commit.gpgsign", "false"]);
    let plan = clone.join("docs/exec-plans/active/p");
    std::fs::create_dir_all(&plan).expect("plan directory is writable");
    std::fs::write(plan.join("plan.md"), "# Plan\n### a\n### b\n").expect("plan.md is written");
    std::fs::write(plan.join("tasks.json"), tasks_json(false)).expect("tasks.json is written");
    let _ = git(&clone, &["add", "."]);
    let _ = git(&clone, &["commit", "--quiet", "-m", "plan"]);
    assert!(clone.join(".git").exists(), "the clone is a git checkout");
    (root, clone)
}

fn run(clone: &Path, arguments: &[&str]) -> Result<String, String> {
    execute(clone, "origin", &Fake::default(), arguments)
}

/// Commits `.github/CODEOWNERS` naming `@owner` and pushes it as the remote's `main`.
fn push_code_owners(clone: &Path) {
    std::fs::create_dir_all(clone.join(".github")).expect("directory is writable");
    std::fs::write(clone.join(".github/CODEOWNERS"), "/x/ @owner\n").expect("file is written");
    let _ = git(clone, &["add", "."]);
    let _ = git(clone, &["commit", "--quiet", "-m", "code owners"]);
    let _ = git(clone, &["push", "--quiet", "origin", "HEAD:refs/heads/main"]);
}

// Goal: `issue show` and `issue file` take the code owners from the remote's `main`, not from
// the working tree, and report bad input. Method: a checkout whose staged CODEOWNERS names
// someone else than `main` does.
#[test]
fn issue_commands_trust_the_code_owners_on_main() {
    let (root, clone) = setup("issue-commands");
    let github = Fake { issues: vec![issue(3, "owner", &["bug"])], ..Fake::default() };
    let issue_command =
        |arguments: &[&str]| xtask::issue::execute(&clone, "origin", &github, arguments);
    let unknown = issue_command(&["show", "3"]).expect_err("no main yet");
    assert!(unknown.contains("on origin/main"), "{unknown}");
    push_code_owners(&clone);
    std::fs::write(clone.join(".github/CODEOWNERS"), "/x/ @other\n").expect("file is written");
    let _ = git(&clone, &["add", "."]);

    let shown = issue_command(&["show", "#3"]).expect("owner is a code owner on main");
    assert!(shown.starts_with("#3 Issue 3\nstate: open\nlabels: bug\nauthor: owner\n"), "{shown}");
    assert!(issue_command(&["show", "x"]).expect_err("bad").contains("not an issue number"));
    assert!(issue_command(&["show", "4"]).expect_err("missing").contains("no issue #4"));

    let draft = root.join("finding.md");
    let finding =
        "# A finding\n\n- Found in: task a\n\n## What happened\n\nX.\n\n## Proposed fix\n\nY.\n";
    std::fs::write(&draft, finding).expect("draft is written");
    let path = draft.to_string_lossy().into_owned();
    let filed = issue_command(&["file", "--kind", "bug", &path]).expect_err("the fake refuses");
    assert!(filed.contains("never file issues"), "the draft reached GitHub: {filed}");
    std::fs::write(&draft, "# A finding\n").expect("draft is written");
    let broken = issue_command(&["file", "--kind", "bug", &path]).expect_err("incomplete");
    assert!(broken.contains("the finding needs"), "{broken}");
    let missing = issue_command(&["file", "--kind", "bug", "/no/such/file.md"]);
    assert!(missing.expect_err("missing").contains("cannot read `/no/such/file.md`"));
    assert!(
        issue_command(&["file", "--kind", "bug"]).expect_err("no file").contains("exactly one")
    );
    assert!(issue_command(&["list"]).expect_err("usage").starts_with("usage:"));
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: an issue is listed, claimed, shown and released like a task, but only if agents may
// work on it: an open issue by the bot or a code owner, with one kind label and no pending
// decision. When GitHub is down, `ready` fails and still names the plan tasks.
#[test]
fn issues_are_claimed_like_tasks() {
    let (root, clone) = setup("issues");
    push_code_owners(&clone);
    let pull = Issue { is_pull_request: true, ..issue(6, "nami-agent[bot]", &["bug"]) };
    let removal = |actor: &str| LabelEvent {
        label: "needs-decision".to_owned(),
        actor: actor.to_owned(),
        added: false,
    };
    let mut github = Fake {
        issues: vec![
            issue(3, "nami-agent[bot]", &["bug"]),
            issue(4, "outsider", &["bug"]),
            issue(5, "nami-agent[bot]", &["docs", "needs-decision"]),
            pull,
            issue(7, "owner", &["docs"]),
            issue(8, "owner", &["guardrail-gap"]),
        ],
        events: vec![(7, removal("nami-agent[bot]")), (8, removal("owner"))],
        is_down: false,
    };
    let task = |fake: &Fake, arguments: &[&str]| execute(&clone, "origin", fake, arguments);
    let both = "a\tTask a\nissue-3\tIssue 3\nissue-8\tIssue 8\n";
    assert_eq!(task(&github, &["ready"]), Ok(both.to_owned()), "plan tasks, then issues");

    for (id, reason) in [
        ("issue-4", "neither the bot nor a code owner"),
        ("issue-5", "waits for the maintainer's decision"),
        ("issue-6", "is a pull request"),
        ("issue-7", "removed by `nami-agent[bot]`"),
        ("issue-9", "no issue #9"),
        ("issue-x", "is not an issue's ID"),
    ] {
        let error = task(&github, &["claim", id]).expect_err("not claimable");
        assert!(error.contains(reason), "{id}: {error}");
    }
    let claimed = task(&github, &["claim", "issue-3"]).expect("a workable issue");
    assert!(claimed.starts_with("claimed issue-3\n") && claimed.contains("Fixes #3"), "{claimed}");
    let again = task(&github, &["claim", "issue-3"]).expect_err("held");
    assert!(again.contains("is claimed by agent: "), "{again}");
    let held = "a\tTask a\nissue-8\tIssue 8\n";
    assert_eq!(task(&github, &["ready"]), Ok(held.to_owned()), "a held issue is not ready");
    let holding = task(&github, &["status"]).expect("status works");
    assert!(holding.lines().any(|line| line.starts_with("issue-3 ") && line.contains(" claimed ")));

    github.issues[0].is_open = false;
    let closed = task(&github, &["status"]).expect("status works");
    assert!(closed.contains("done: the issue is closed"), "{closed}");
    github.is_down = true;
    let unknown = task(&github, &["status"]).expect("status works without GitHub");
    assert!(unknown.contains("its state is unknown: no issue #3"), "{unknown}");
    let partial = task(&github, &["ready"]).expect_err("an incomplete list fails");
    assert!(partial.contains("GitHub is down") && partial.ends_with("\na\tTask a\n"), "{partial}");
    assert_eq!(task(&github, &["release", "issue-3"]), Ok("released issue-3\n".to_owned()));

    let check = task(&github, &["check-pr", "HEAD", "task/issue-3"]);
    assert_eq!(check, Ok(String::new()), "an issue has no owner paths to warn about");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: validate, ready, status, claim and release agree with each other and with the
// dependency graph. Method: walk one task through its life in a fixture checkout.
#[test]
fn commands_follow_a_task_through_its_life() {
    let (root, clone) = setup("life");
    assert_eq!(run(&clone, &["validate"]), Ok("tasks are valid\n".to_owned()));
    assert_eq!(run(&clone, &["ready"]), Ok("a\tTask a\n".to_owned()), "b waits for a");
    let status = run(&clone, &["status"]).expect("status works");
    assert!(status.contains("ready") && status.contains("blocked on a"), "{status}");

    let blocked = run(&clone, &["claim", "b"]).expect_err("a blocked task cannot be claimed");
    assert!(blocked.contains("blocked on a"), "{blocked}");
    assert!(run(&clone, &["claim", "zz"]).expect_err("unknown").contains("no active task"));
    assert_eq!(run(&clone, &["claim", "a"]), Ok("claimed a\n".to_owned()));
    let again = run(&clone, &["claim", "a"]).expect_err("a held task cannot be claimed");
    assert!(again.contains("claimed by agent: "), "{again}");
    assert_eq!(run(&clone, &["ready"]), Ok(String::new()), "nothing is ready while a is held");

    let holder = std::env::var("NAMI_AGENT_ID").unwrap_or_else(|_| "tester".to_owned());
    let held = run(&clone, &["status"]).expect("status works");
    assert!(held.contains(&format!("by agent: {holder}, session: ")), "{held}");
    assert_eq!(run(&clone, &["release", "a"]), Ok("released a\n".to_owned()));
    assert!(run(&clone, &["release", "a"]).expect_err("twice").contains("not claimed"));
    assert_eq!(run(&clone, &["ready"]), Ok("a\tTask a\n".to_owned()), "a is free again");
    assert!(run(&clone, &["bogus"]).expect_err("usage").starts_with("usage:"));
    for (command, bad) in [("release", ""), ("release", "a/b"), ("claim", "-a"), ("claim", "A")] {
        let error = run(&clone, &[command, bad]).expect_err("bad ids are input errors");
        assert!(error.contains("is not a task id"), "{command} {bad:?}: {error}");
    }
    // `ls-remote` matches by suffix: a branch named `task/claims` must not confuse the lookup.
    let _ = git(&clone, &["push", "--quiet", "origin", "HEAD:refs/heads/task/claims"]);
    let _ = git(&clone, &["push", "--quiet", "origin", "--delete", "claims"]);
    assert_eq!(run(&clone, &["ready"]), Ok("a\tTask a\n".to_owned()), "no claims branch again");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: check-pr fails exactly when a change sets `passes`, and warns about files outside the
// task's owner paths on `task/<id>` branches. Method: commits on top of the fixture plan.
#[test]
fn check_pr_rejects_passes_set_outside_ci() {
    let (root, clone) = setup("check-pr");
    let base = git(&clone, &["rev-parse", "HEAD"]).trim().to_owned();
    let tasks = clone.join("docs/exec-plans/active/p/tasks.json");
    std::fs::write(&tasks, tasks_json(true)).expect("tasks.json is written");
    let error = run(&clone, &["check-pr", &base]).expect_err("setting passes is rejected");
    assert!(error.contains("sets `passes` on `a`") && error.contains("1 task(s)"), "{error}");

    let _ = git(&clone, &["commit", "--quiet", "-am", "CI marks a as passing"]);
    let marked = git(&clone, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(run(&clone, &["check-pr", &marked]), Ok(String::new()), "unchanged passes are fine");

    std::fs::create_dir_all(clone.join("src/a")).expect("source directory is writable");
    std::fs::write(clone.join("src/a/lib.rs"), "").expect("owned file is written");
    std::fs::write(clone.join("outside.rs"), "").expect("outside file is written");
    // Widening its own owner paths does not help a task: the base definition counts, and the
    // edit to tasks.json is itself reported.
    std::fs::write(&tasks, tasks_json(true).replace("\"src/a\"", "\".\"")).expect("written");
    let _ = git(&clone, &["add", "."]);
    let _ = git(&clone, &["commit", "--quiet", "-m", "work on a"]);
    let output = run(&clone, &["check-pr", &marked, "task/a"]).expect("warnings do not fail");
    let expected = [
        concat!(
            "warning: `docs/exec-plans/active/p/tasks.json` ",
            "is outside the owner_paths of `a` (ADR-0011)"
        ),
        "warning: `outside.rs` is outside the owner_paths of `a` (ADR-0011)",
    ];
    assert_eq!(output.lines().collect::<Vec<_>>(), expected);
    assert_eq!(run(&clone, &["check-pr", &marked, "feature/x"]), Ok(String::new()), "not a task");

    std::fs::write(&tasks, "{").expect("tasks.json is written");
    let invalid = run(&clone, &["ready"]).expect_err("invalid plans block the commands");
    assert!(invalid.contains("invalid JSON") && invalid.contains("1 problem(s)"), "{invalid}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: a dependency on a task in a completed plan counts as passing. Method: a completed plan
// holds the passing task `c`, and the active task `d` depends on it.
#[test]
fn completed_plans_unblock_their_dependants() {
    let (root, clone) = setup("completed");
    let done = clone.join("docs/exec-plans/completed/done");
    std::fs::create_dir_all(&done).expect("plan directory is writable");
    std::fs::write(done.join("plan.md"), "### c\n").expect("plan.md is written");
    let task = |id: &str, depends: &str, passes: bool| {
        format!(
            r#"{{"id": "{id}", "title": "Task {id}", "depends_on": [{depends}],
                "owner_paths": ["src/{id}"], "acceptance": ["{id}::works"], "passes": {passes}}}"#
        )
    };
    let completed = format!("{{\"tasks\": [{}]}}", task("c", "", true));
    std::fs::write(done.join("tasks.json"), completed).expect("tasks.json is written");
    let active = clone.join("docs/exec-plans/active/p");
    std::fs::write(active.join("plan.md"), "### d\n").expect("plan.md is written");
    let pending = format!("{{\"tasks\": [{}]}}", task("d", "\"c\"", false));
    std::fs::write(active.join("tasks.json"), pending).expect("tasks.json is written");
    assert_eq!(run(&clone, &["ready"]), Ok("d\tTask d\n".to_owned()), "c passes, so d is ready");
    let status = run(&clone, &["status"]).expect("status works");
    assert!(!status.contains("Task c"), "completed plans are not listed: {status}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}
