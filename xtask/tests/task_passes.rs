//! Setting `passes` end to end (ADR-0011, ADR-0017): the post-merge jobs and `check-pr`,
//! against real git checkouts, with GitHub's pull requests and the test runner from fakes.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::task::acceptance::{Acceptance, Nextest};
use xtask::task::passes::{self, PullHead, Pulls, Services};
use xtask::task::plan::Task;

/// GitHub's pull requests: fixed heads and merged branches, and a log of what was asked.
#[derive(Default)]
struct FakePulls {
    heads: BTreeMap<u32, PullHead>,
    merged: Vec<String>,
    open: Option<u32>,
    log: RefCell<Vec<String>>,
}

impl FakePulls {
    fn record(&self, entry: String) {
        self.log.borrow_mut().push(entry);
    }
}

impl Pulls for FakePulls {
    fn head(&self, number: u32) -> Result<PullHead, String> {
        self.heads.get(&number).cloned().ok_or_else(|| format!("no pull request #{number}"))
    }
    fn has_merged(&self, branch: &str) -> Result<bool, String> {
        Ok(self.merged.iter().any(|merged| merged == branch))
    }
    fn open_from(&self, branch: &str) -> Result<Option<u32>, String> {
        Ok(self.open.filter(|_| branch == passes::BOT_BRANCH))
    }
    fn open(&self, branch: &str, title: &str, body: &str) -> Result<u32, String> {
        self.record(format!("open {branch} {title}\n{body}"));
        Ok(70)
    }
    fn edit(&self, number: u32, title: &str, _body: &str) -> Result<(), String> {
        self.record(format!("edit #{number} {title}"));
        Ok(())
    }
    fn close(&self, number: u32) -> Result<(), String> {
        self.record(format!("close #{number}"));
        Ok(())
    }
    fn auto_merge(&self, number: u32) -> Result<(), String> {
        self.record(format!("auto-merge #{number}"));
        Ok(())
    }
}

/// A test runner with a fixed verdict per task, which logs the tasks it ran.
#[derive(Default)]
struct FakeTests {
    failing: Vec<String>,
    ran: RefCell<Vec<String>>,
}

impl Acceptance for FakeTests {
    fn check(&self, tasks: &[&Task]) -> Result<Vec<Result<(), String>>, String> {
        let verdicts = tasks.iter().map(|task| {
            self.ran.borrow_mut().push(task.id.clone());
            if self.failing.contains(&task.id) { Err("a test failed".to_owned()) } else { Ok(()) }
        });
        Ok(verdicts.collect())
    }
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git").args(arguments).current_dir(directory).output();
    let output = output.expect("git runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "git {arguments:?} failed: {stderr}");
    String::from_utf8(output.stdout).expect("git prints UTF-8").trim().to_owned()
}

const TASKS: &str = "docs/exec-plans/active/p/tasks.json";

/// The plan, one field per line as the plans write it: `a`, and `b`, which depends on `a`.
fn tasks_json(a_passes: bool) -> String {
    let task = |id: &str, depends: &str, passes: bool| {
        format!(
            "    {{\n      \"id\": \"{id}\",\n      \"title\": \"Task {id}\",\n      \
             \"depends_on\": [{depends}],\n      \"owner_paths\": [\"src/{id}\"],\n      \
             \"acceptance\": [\"{id}::works\"],\n      \"passes\": {passes}\n    }}"
        )
    };
    format!(
        "{{\n  \"tasks\": [\n{},\n{}\n  ]\n}}\n",
        task("a", "", a_passes),
        task("b", "\"a\"", false)
    )
}

/// A bare remote and one clone holding the committed plan, pushed as `main`.
fn setup(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("nami-passes-{name}-{}", std::process::id()));
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
    std::fs::create_dir_all(clone.join("docs/exec-plans/active/p")).expect("writable");
    let plan = "docs/exec-plans/active/p/plan.md";
    std::fs::write(clone.join(plan), "# Plan\n### a\n### b\n").expect("plan.md is written");
    std::fs::write(clone.join(TASKS), tasks_json(false)).expect("tasks.json is written");
    let _ = commit(&clone, "plan");
    let _ = git(&clone, &["push", "--quiet", "origin", "HEAD:refs/heads/main"]);
    (root, clone)
}

fn commit(clone: &Path, message: &str) -> String {
    let _ = git(clone, &["add", "--all"]);
    let _ = git(clone, &["commit", "--quiet", "--allow-empty", "-m", message]);
    git(clone, &["rev-parse", "HEAD"])
}

fn bot_head(branch: &str, author: &str) -> PullHead {
    PullHead { branch: branch.to_owned(), author: author.to_owned() }
}

/// GitHub as it is for the bot's PR #7: from `bot/passes`, by `nami-ci[bot]`, with `a`'s PR
/// from `task/a` merged.
fn bots_pull() -> FakePulls {
    FakePulls {
        heads: [(7, bot_head(passes::BOT_BRANCH, passes::CI_LOGIN))].into(),
        merged: vec!["task/a".to_owned()],
        ..FakePulls::default()
    }
}

fn check(clone: &Path, pulls: &FakePulls, tests: &FakeTests, arguments: &[&str]) -> String {
    let services = Services { pulls, tests };
    match passes::execute(clone, "origin", services, arguments) {
        Ok(output) => format!("ok: {output}"),
        Err(error) => format!("error: {error}"),
    }
}

// Goal: `check-pr` lets a change set `passes` only when all four conditions hold, and each
// condition alone, broken, rejects it: the head branch, the author, the shape of the diff,
// and the acceptance tests run in this run. A missing merged PR also rejects it, and no test
// runs unless the rest holds.
#[test]
fn check_pr_lets_only_the_bots_flip_through() {
    let (root, clone) = setup("check-pr");
    let base = git(&clone, &["rev-parse", "HEAD"]);
    std::fs::write(clone.join(TASKS), tasks_json(true)).expect("tasks.json is written");
    let _ = commit(&clone, "Set passes: a");
    let arguments = ["check-pr", base.as_str(), "bot/passes", "7"];
    let tests = FakeTests::default();
    let accepted = check(&clone, &bots_pull(), &tests, &arguments);
    assert!(accepted.starts_with("ok: ") && accepted.contains("on a: their"), "{accepted}");
    assert_eq!(*tests.ran.borrow(), ["a"], "the flipped task's tests ran in this run");

    let rejected = |github: &FakePulls, runner: &FakeTests, given: &[&str], reason: &str| {
        let output = check(&clone, github, runner, given);
        assert!(output.starts_with("error: ") && output.contains(reason), "{reason}: {output}");
        assert!(output.contains("agents never do"), "{output}");
    };
    let mut pulls = bots_pull();
    let _ = pulls.heads.insert(7, bot_head("task/a", passes::CI_LOGIN));
    rejected(&pulls, &tests, &arguments, "#7 comes from `task/a`");
    let _ = pulls.heads.insert(7, bot_head("fork:bot/passes", passes::CI_LOGIN));
    rejected(&pulls, &tests, &arguments, "#7 comes from `fork:bot/passes`");
    let _ = pulls.heads.insert(7, bot_head(passes::BOT_BRANCH, "nami-agent[bot]"));
    rejected(&pulls, &tests, &arguments, "opened by `nami-agent[bot]`");
    rejected(&bots_pull(), &tests, &["check-pr", &base, "bot/passes"], "no pull request was");
    rejected(&bots_pull(), &tests, &["check-pr", &base, "bot/passes", "8"], "no pull request #8");
    let unmerged = FakePulls { merged: Vec::new(), ..bots_pull() };
    rejected(&unmerged, &tests, &arguments, "no PR from `task/a` has merged");
    assert_eq!(tests.ran.borrow().len(), 1, "no test runs while another condition fails");
    let failing = FakeTests { failing: vec!["a".to_owned()], ..FakeTests::default() };
    rejected(&bots_pull(), &failing, &arguments, "`a` does not pass: a test failed");

    let flipped = tasks_json(true);
    std::fs::write(clone.join(TASKS), flipped.replace("Task b", "Task B")).expect("written");
    rejected(&bots_pull(), &tests, &arguments, "line 13 changes more than a `passes` value");
    std::fs::write(clone.join(TASKS), &flipped).expect("written");
    std::fs::write(clone.join("docs/exec-plans/active/p/plan.md"), "# Plan\n### a\n### b\nx")
        .expect("written");
    rejected(&bots_pull(), &tests, &arguments, "`docs/exec-plans/active/p/plan.md` (M) is not");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: in the merge queue, the PR comes from the queue branch's name, and each entry is
// judged on its own squashed commit: the bot's flip passes behind another PR, and a PR behind
// the bot's flip is not taken for one that sets `passes`.
#[test]
fn check_pr_judges_each_queue_entry_on_its_own_commit() {
    let (root, clone) = setup("queue");
    let group_base = git(&clone, &["rev-parse", "HEAD"]);
    std::fs::write(clone.join("other.txt"), "x").expect("written");
    let _ = commit(&clone, "Another PR (#6)");
    std::fs::write(clone.join(TASKS), tasks_json(true)).expect("tasks.json is written");
    let _ = commit(&clone, "Set passes: a (#7)");
    let tests = FakeTests::default();
    let queue = |pull: u32| format!("gh-readonly-queue/main/pr-{pull}-{group_base}");
    let bot = check(&clone, &bots_pull(), &tests, &["check-pr", &group_base, &queue(7), ""]);
    assert!(bot.starts_with("ok: ") && bot.contains("on a: their"), "{bot}");
    let wrong = check(&clone, &bots_pull(), &tests, &["check-pr", &group_base, &queue(6), ""]);
    assert!(wrong.contains("`other.txt` (A) is not"), "no own commit, so the group: {wrong}");

    std::fs::write(clone.join("later.txt"), "y").expect("written");
    let _ = commit(&clone, "A later PR (#8)");
    let later = check(&clone, &bots_pull(), &tests, &["check-pr", &group_base, &queue(8), ""]);
    assert_eq!(later, "ok: ", "the flip ahead of #8 is not #8's");
    let error = check(&clone, &bots_pull(), &tests, &["check-pr", &group_base, "bot/passes", "x"]);
    assert!(error.contains("`x` is not a pull request number"), "{error}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: the `acceptance` job runs only unfinished, unblocked tasks, and its file lists exactly
// those that passed; a failing task is reported with its reason and left out.
#[test]
fn passes_reports_the_unblocked_tasks_that_pass() {
    let (root, clone) = setup("found");
    let file = root.join("passing.json");
    let file_text = file.to_string_lossy().into_owned();
    let tests = FakeTests::default();
    let pulls = FakePulls::default();
    let output = check(&clone, &pulls, &tests, &["passes", "--output", &file_text]);
    assert_eq!(output, "ok: passes\ta\n", "b is blocked on a");
    assert_eq!(std::fs::read_to_string(&file).expect("written"), "{\"passing\": [\"a\"]}\n");
    let failing = FakeTests { failing: vec!["a".to_owned()], ..FakeTests::default() };
    let failed = check(&clone, &pulls, &failing, &["passes", "--output", &file_text]);
    assert_eq!(failed, "ok: fails\ta\ta test failed\n");
    assert_eq!(std::fs::read_to_string(&file).expect("written"), "{\"passing\": []}\n");

    std::fs::write(clone.join(TASKS), tasks_json(true)).expect("tasks.json is written");
    let next = check(&clone, &pulls, &tests, &["passes"]);
    assert_eq!(next, "ok: passes\tb\n", "a passes, so b runs");
    assert!(pulls.log.borrow().is_empty(), "the acceptance job asks GitHub nothing");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: the `open passes PR` job sets `passes` only on reported tasks that may be set, pushes
// exactly that change as `bot/passes`, opens one PR or updates the open one, and closes it
// when nothing is left; and the pushed commit passes `check-pr`.
#[test]
fn open_pr_sets_only_candidates_and_keeps_one_pr() {
    let (root, clone) = setup("open-pr");
    let main = git(&clone, &["rev-parse", "HEAD"]);
    let file = root.join("passing.json");
    let file_text = file.to_string_lossy().into_owned();
    std::fs::write(&file, "{\"passing\": [\"a\", \"b\", \"zz\"]}").expect("written");
    let pulls = bots_pull();
    let tests = FakeTests::default();
    let output = check(&clone, &pulls, &tests, &["passes", "--open-pr", &file_text]);
    assert!(output.contains("skipped\tb\t") && output.contains("skipped\tzz\t"), "{output}");
    assert!(output.ends_with("#70 sets `passes` on a\n"), "{output}");
    let log = pulls.log.borrow().clone();
    assert!(log[0].starts_with("open bot/passes Set passes: a\n## Summary"), "{log:?}");
    assert_eq!(log[1], "auto-merge #70");
    assert!(tests.ran.borrow().is_empty(), "the job with the token runs no tests");
    let pushed = git(&clone, &["ls-remote", "origin", "refs/heads/bot/passes"]);
    let pushed = pushed.split('\t').next().expect("a commit").to_owned();
    assert_eq!(git(&clone, &["rev-parse", &format!("{pushed}^")]), main, "on top of main");
    let diff = git(&clone, &["diff", "--numstat", &main, &pushed]);
    assert_eq!(diff, format!("1\t1\t{TASKS}"), "one line of one file");
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), main, "the checkout's branch stays");

    let _ = git(&clone, &["checkout", "--quiet", "--force", &pushed]);
    let checked = check(&clone, &pulls, &tests, &["check-pr", &main, "bot/passes", "7"]);
    assert!(checked.starts_with("ok: "), "the bot's commit passes check-pr: {checked}");
    let _ = git(&clone, &["checkout", "--quiet", "--force", &main]);

    let updating = FakePulls { open: Some(70), ..bots_pull() };
    let updated = check(&clone, &updating, &tests, &["passes", "--open-pr", &file_text]);
    assert!(updated.ends_with("#70 sets `passes` on a\n"), "{updated}");
    assert_eq!(*updating.log.borrow(), ["edit #70 Set passes: a", "auto-merge #70"]);
    let _ = git(&clone, &["checkout", "--quiet", "--force", &main]);
    std::fs::write(&file, "{\"passing\": [\"b\"]}").expect("written");
    let closing = FakePulls { open: Some(70), ..bots_pull() };
    let closed = check(&clone, &closing, &tests, &["passes", "--open-pr", &file_text]);
    assert!(closed.ends_with("closed #70: no task is newly done\n"), "{closed}");
    assert_eq!(*closing.log.borrow(), ["close #70"]);
    std::fs::write(&file, "{\"passing\": [\"a\"], \"x\": 1}").expect("written");
    let refused = check(&clone, &closing, &tests, &["passes", "--open-pr", &file_text]);
    assert!(refused.starts_with("error: the file must be {\"passing\""), "{refused}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

/// A workspace with one crate, `fixture`: a library with a passing, a failing and an ignored
/// test, and an integration test target `outer`.
fn fixture_workspace(root: &Path) -> PathBuf {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(workspace.join("src")).expect("writable");
    std::fs::create_dir_all(workspace.join("tests")).expect("writable");
    let toolchain = xtask::workspace_root().join("rust-toolchain.toml");
    let _ = std::fs::copy(toolchain, workspace.join("rust-toolchain.toml")).expect("copied");
    let files = [
        (
            "Cargo.toml",
            "[workspace]\n\n[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \
             \"2024\"\npublish = false\n",
        ),
        (
            "Cargo.lock",
            "# This file is automatically @generated by Cargo.\n# It is not intended for manual \
             editing.\nversion = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn good() {}\n    #[test]\n    fn bad() \
             {\n        panic!(\"bad\");\n    }\n    #[test]\n    #[ignore = \"slow\"]\n    fn \
             skipped() {}\n}\n",
        ),
        ("tests/outer.rs", "#[test]\nfn outer_good() {}\n"),
    ];
    for (file, text) in files {
        std::fs::write(workspace.join(file), text).expect("written");
    }
    workspace
}

fn task(id: &str, acceptance: &[&str]) -> Task {
    Task {
        id: id.to_owned(),
        title: id.to_owned(),
        depends_on: Vec::new(),
        owner_paths: Vec::new(),
        acceptance: acceptance.iter().map(|&name| name.to_owned()).collect(),
        passes: false,
        file: PathBuf::from(TASKS),
        active: true,
    }
}

// Goal: the real runner passes a task only when every name it lists names a test that ran and
// passed, in a library or an integration test; a failing, missing, ignored or partial name
// fails. Method: `cargo nextest` on a one-crate workspace.
#[test]
fn nextest_passes_only_names_that_ran_and_passed() {
    let root = std::env::temp_dir().join(format!("nami-passes-nextest-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).expect("a stale temporary directory is removable");
    }
    let workspace = fixture_workspace(&root);
    let tasks = [
        task("both", &["fixture::tests::good", "fixture::outer::outer_good"]),
        task("failing", &["fixture::tests::good", "fixture::tests::bad"]),
        task("missing", &["fixture::tests::gone"]),
        task("ignored", &["fixture::tests::skipped"]),
        task("partial", &["fixture::tests"]),
    ];
    let chosen: Vec<&Task> = tasks.iter().collect();
    let verdicts = Nextest::new(&workspace).check(&chosen).expect("nextest lists the tests");
    assert_eq!(verdicts.len(), tasks.len(), "one verdict per task");
    assert_eq!(verdicts[0], Ok(()), "both tests passed");
    let failed = verdicts[1].as_ref().expect_err("bad fails");
    assert!(failed.contains("acceptance tests of `failing` failed"), "{failed}");
    for verdict in &verdicts[2..] {
        let error = verdict.as_ref().expect_err("no such test runs");
        assert!(error.contains("names no test that runs"), "{error}");
    }
    assert_eq!(Nextest::new(&workspace).check(&[]), Ok(Vec::new()), "nothing to run");
    std::fs::remove_dir_all(&root).expect("cleanup");
}
