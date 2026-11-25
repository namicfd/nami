//! `cargo xtask task` commands end to end, against a real git checkout (ADR-0011).

use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::task::execute;

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
    let _ = std::fs::remove_dir_all(&root);
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
    execute(clone, "origin", arguments)
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
    let _ = git(&clone, &["add", "."]);
    let _ = git(&clone, &["commit", "--quiet", "-m", "work on a"]);
    let output = run(&clone, &["check-pr", &marked, "task/a"]).expect("warnings do not fail");
    assert_eq!(output, "warning: `outside.rs` is outside the owner_paths of `a` (ADR-0011)\n");
    assert_eq!(run(&clone, &["check-pr", &marked, "feature/x"]), Ok(String::new()), "not a task");

    std::fs::write(&tasks, "{").expect("tasks.json is written");
    let invalid = run(&clone, &["ready"]).expect_err("invalid plans block the commands");
    assert!(invalid.contains("invalid JSON") && invalid.contains("1 problem(s)"), "{invalid}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}
