//! `cargo xtask task` commands end to end, against a real git checkout (ADR-0011), with
//! GitHub issues from a fake (ADR-0021).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::issue::github::{Comment, GitHub, Issue, LabelEvent, Timestamp};
use xtask::task::execute;

/// GitHub with fixed issues, label events, comments and merge-queue entries, or unreachable.
/// It records whose comments were asked for, the comments posted and the labels removed. The
/// repository has the kind labels and `needs-decision`, unless `needs-decision` is gone.
#[derive(Default)]
struct Fake {
    issues: Vec<Issue>,
    events: Vec<(u32, LabelEvent)>,
    comments: Vec<(u32, Comment)>,
    queued: Vec<u32>,
    is_down: bool,
    is_label_gone: bool,
    asked: RefCell<Vec<u32>>,
    posted: RefCell<Vec<(u32, String)>>,
    removed: RefCell<Vec<(u32, String)>>,
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
    fn comments(&self, number: u32) -> Result<Vec<Comment>, String> {
        self.asked.borrow_mut().push(number);
        let comments = self.comments.iter().filter(|(of, _)| *of == number);
        Ok(comments.map(|(_, comment)| comment.clone()).collect())
    }
    fn label_events(&self, number: u32) -> Result<Vec<LabelEvent>, String> {
        let events = self.events.iter().filter(|(of, _)| *of == number);
        Ok(events.map(|(_, event)| event.clone()).collect())
    }
    fn create(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<u32, String> {
        Err("the task commands never file issues".to_owned())
    }
    fn comment(&self, number: u32, body: &str) -> Result<Comment, String> {
        self.posted.borrow_mut().push((number, body.to_owned()));
        let at = Timestamp::parse("2026-11-26T09:00:00Z")?;
        let author = "nami-agent[bot]".to_owned();
        Ok(Comment { author, body: body.to_owned(), created_at: at.clone(), updated_at: at })
    }
    fn remove_label(&self, number: u32, label: &str) -> Result<(), String> {
        self.removed.borrow_mut().push((number, label.to_owned()));
        Ok(())
    }
    fn repository_labels(&self) -> Result<Vec<String>, String> {
        let names = ["bug", "docs", "guardrail-gap", "needs-decision"];
        let kept = names.iter().filter(|name| !self.is_label_gone || **name != "needs-decision");
        Ok(kept.map(|name| (*name).to_owned()).collect())
    }
    fn is_queued(&self, number: u32) -> Result<bool, String> {
        if self.is_down {
            Err("GitHub is down".to_owned())
        } else {
            Ok(self.queued.contains(&number))
        }
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
    let issues =
        vec![issue(3, "owner", &["bug"]), issue(5, "nami-agent[bot]", &["bug", "needs-decision"])];
    let added = LabelEvent {
        label: "needs-decision".to_owned(),
        actor: "nami-agent[bot]".to_owned(),
        added: true,
        created_at: Timestamp::parse("2026-11-26T08:00:00Z").expect("a time"),
    };
    let github = Fake { issues, events: vec![(5, added)], ..Fake::default() };
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

    let decision = root.join("decision.md");
    std::fs::write(&decision, "Agents may\nremove it.\n").expect("decision is written");
    let decision = decision.to_string_lossy().into_owned();
    let decide = |number: &str, by: &str| {
        let options = ["--by", by, "--date", "2026-11-26", "--source", "lead session"];
        issue_command(&[&["decide", number][..], &options, &[decision.as_str()]].concat())
    };
    let other = decide("5", "other").expect_err("`other` is a code owner only in the work tree");
    assert!(other.contains("`other` is not a code owner"), "{other}");
    let owners = decide("3", "owner").expect_err("an owner's issue");
    assert!(owners.contains("#3 was opened by `owner`, not by the bot"), "{owners}");
    assert!(github.posted.borrow().is_empty(), "a refusal posts nothing");
    assert_eq!(
        decide("#5", "@owner"),
        Ok("decided #5: posted the decision record and removed \
         `needs-decision`\n"
            .to_owned())
    );
    let record = "<!-- nami:decision -->\nDecision: Agents may remove it.\nDecided by: @owner, \
                  2026-11-26\nSource: lead session\n";
    assert_eq!(*github.posted.borrow(), [(5, record.to_owned())]);
    assert_eq!(*github.removed.borrow(), [(5, "needs-decision".to_owned())]);
    std::fs::remove_dir_all(&root).expect("cleanup");
}

/// The label history of issue `number` in which an owner removes the bot's `needs-decision`
/// and asks again, and the bot then removes the label, adds it and removes it at 08:10.
fn reask(number: u32) -> Vec<(u32, LabelEvent)> {
    let steps = [
        ("nami-agent[bot]", "2026-11-26T07:00:00Z"),
        ("owner", "2026-11-26T07:10:00Z"),
        ("owner", "2026-11-26T07:20:00Z"),
        ("nami-agent[bot]", "2026-11-26T07:30:00Z"),
        ("nami-agent[bot]", "2026-11-26T07:40:00Z"),
        ("nami-agent[bot]", "2026-11-26T08:10:00Z"),
    ];
    let event = |step: usize, (actor, time): (&str, &str)| LabelEvent {
        label: "needs-decision".to_owned(),
        actor: actor.to_owned(),
        added: step.is_multiple_of(2),
        created_at: Timestamp::parse(time).expect("a time"),
    };
    steps.into_iter().enumerate().map(|(step, entry)| (number, event(step, entry))).collect()
}

/// Issues for `issues_are_claimed_like_tasks`. #3 is workable; #4 is an outsider's; #5 waits for
/// a decision; #6 is a pull request; #8 and #10 are decided, by an owner and by the bot with a
/// record; the bot removed the label of #7 before its record, of #11 with a non-owner's, and
/// with a valid one that of #12, which an owner opened, and of #13, which an owner labelled.
/// #14's record is from the second of the addition. Each label was added at 08:00 and removed
/// at 08:10. #15 has the reviewer's history: an owner removed the bot's label and asked again,
/// and the bot then removed it, added it, recorded a decision and removed it.
fn claimable_issues() -> Fake {
    let pull = Issue { is_pull_request: true, ..issue(6, "nami-agent[bot]", &["bug"]) };
    let at = |time: &str| Timestamp::parse(time).expect("a time");
    let event = |actor: &str, added: bool, time: &str| LabelEvent {
        label: "needs-decision".to_owned(),
        actor: actor.to_owned(),
        added,
        created_at: at(time),
    };
    let round = |number: u32, adder: &str, remover: &str| {
        [
            (number, event(adder, true, "2026-11-26T08:00:00Z")),
            (number, event(remover, false, "2026-11-26T08:10:00Z")),
        ]
    };
    let bot = "nami-agent[bot]";
    Fake {
        issues: vec![
            issue(3, "nami-agent[bot]", &["bug"]),
            issue(4, "outsider", &["bug"]),
            issue(5, "nami-agent[bot]", &["docs", "needs-decision"]),
            pull,
            issue(7, "nami-agent[bot]", &["docs"]),
            issue(8, "owner", &["guardrail-gap"]),
            issue(10, "nami-agent[bot]", &["bug"]),
            issue(11, "nami-agent[bot]", &["bug"]),
            issue(12, "owner", &["bug"]),
            issue(13, bot, &["bug"]),
            issue(14, bot, &["bug"]),
            issue(15, bot, &["bug"]),
        ],
        events: [
            &round(7, bot, bot)[..],
            &round(8, "owner", "owner"),
            &round(10, bot, bot),
            &round(11, bot, bot),
            &round(12, bot, bot),
            &round(13, "owner", bot),
            &round(14, bot, bot),
            &reask(15),
        ]
        .concat(),
        comments: claimable_records(),
        ..Fake::default()
    }
}

/// The bot's decision records on the issues of [`claimable_issues`].
fn claimable_records() -> Vec<(u32, Comment)> {
    let at = |time: &str| Timestamp::parse(time).expect("a time");
    let record = |decider: &str, time: &str| Comment {
        author: "nami-agent[bot]".to_owned(),
        body: format!(
            "<!-- nami:decision -->\nDecision: Go.\nDecided by: @{decider}, 2026-11-26\n\
             Source: lead session\n"
        ),
        created_at: at(time),
        updated_at: at(time),
    };
    // #7's record does not count: it was posted after the removal.
    let late = "2026-11-26T08:11:00Z";
    let time = "2026-11-26T08:10:00Z";
    vec![
        (7, record("owner", late)),
        (10, record("owner", time)),
        (11, record("outsider", time)),
        (12, record("owner", time)),
        (13, record("owner", time)),
        (14, record("owner", "2026-11-26T08:00:00Z")),
        (15, record("owner", time)),
    ]
}

// Goal: an issue is listed, claimed, shown and released like a task, but only if agents may
// work on it: an open issue by the bot or a code owner, with one kind label and no pending
// decision. When GitHub is down, `ready` fails and still names the plan tasks.
#[test]
fn issues_are_claimed_like_tasks() {
    let (root, clone) = setup("issues");
    push_code_owners(&clone);
    let mut github = claimable_issues();
    let task = |fake: &Fake, arguments: &[&str]| execute(&clone, "origin", fake, arguments);
    let both = "a\tTask a\nissue-3\tIssue 3\nissue-8\tIssue 8\nissue-10\tIssue 10\n";
    assert_eq!(task(&github, &["ready"]), Ok(both.to_owned()), "plan tasks, then issues");
    let asked = [7, 10, 11, 14];
    assert_eq!(*github.asked.borrow(), asked, "comments matter only for the bot's own labels");

    for (id, reason) in [
        ("issue-4", "neither the bot nor a code owner"),
        ("issue-5", "waits for the maintainer's decision"),
        ("issue-6", "is a pull request"),
        ("issue-7", "removed by `nami-agent[bot]` without a decision record"),
        ("issue-11", "`@outsider`, who is not a code owner"),
        ("issue-12", "`owner` opened the issue, so only a code owner lifts `needs-decision`"),
        ("issue-13", "`owner` added `needs-decision` once, so only a code owner lifts it"),
        ("issue-14", "the bot's latest comment before it is not newer than the label"),
        ("issue-15", "`owner` removed `needs-decision` once, so only a code owner lifts it"),
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
    let held = "a\tTask a\nissue-8\tIssue 8\nissue-10\tIssue 10\n";
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

/// The bot's issue about failed run `run` of PR #45.
fn queue_failure(number: u32, run: u64) -> Issue {
    Issue {
        title: format!("PR #45 failed in the merge queue (run {run})"),
        ..issue(number, "nami-agent[bot]", &["bug"])
    }
}

/// PR #45, open, with the description `body`.
fn pull_45(body: &str) -> Issue {
    Issue { is_pull_request: true, body: body.to_owned(), ..issue(45, "nami-agent[bot]", &[]) }
}

// Goal: the bot's queue-failure issue waits while the PR it names is open and its description
// closes the issue: `ready` leaves it out, `claim` refuses and says why, and `status` shows a
// held claim as waiting (ADR-0021, point 6). A closing word in a comment does not count. A
// closed PR, or a number that is no PR, makes the issue a human's to close; a title by
// someone else than the bot is an ordinary issue.
#[test]
fn queue_failures_wait_for_their_fix() {
    let (root, clone) = setup("queue-failures");
    push_code_owners(&clone);
    let failure = queue_failure(3, 900);
    let listed = format!("a\tTask a\nissue-3\t{}\n", failure.title);
    let mut github =
        Fake { issues: vec![failure, pull_45("Summary.\n\nFixes #3\n")], ..Fake::default() };
    let task = |fake: &Fake, arguments: &[&str]| execute(&clone, "origin", fake, arguments);
    let left_out = "a\tTask a\n".to_owned();
    assert_eq!(task(&github, &["ready"]), Ok(left_out.clone()), "the fix is in flight");
    let refused = task(&github, &["claim", "issue-3"]).expect_err("it waits");
    assert!(refused.contains("#45 says `Fixes #3`: the fix is in flight"), "{refused}");

    github.issues[1].body = "Summary.\n<!-- Fixes #3 -->\n".to_owned();
    assert_eq!(task(&github, &["ready"]), Ok(listed.clone()), "a comment closes nothing");
    github.queued = vec![45];
    assert_eq!(task(&github, &["ready"]), Ok(left_out.clone()), "never push to a queued PR");
    let queued = task(&github, &["claim", "issue-3"]).expect_err("#45 is queued");
    assert!(queued.contains("#45 is in the merge queue, and agents never push"), "{queued}");
    github.queued.clear();
    let claimed = task(&github, &["claim", "issue-3"]).expect("work for an agent");
    assert!(claimed.contains("fix it on #45's own branch") && claimed.contains("keep the claim"));
    github.issues[1].body = "**Fixes #3**".to_owned();
    let status = task(&github, &["status"]).expect("status works");
    assert!(status.contains("; #45 says `Fixes #3`: the fix is in flight"), "{status}");
    let popped = github.issues.pop().expect("the fix");
    let unknown = task(&github, &["status"]).expect("status works");
    assert!(unknown.contains("its fix's state is unknown: no issue #45"), "{unknown}");
    assert_eq!(task(&github, &["release", "issue-3"]), Ok("released issue-3\n".to_owned()));
    github.issues.push(popped);
    assert_eq!(task(&github, &["ready"]), Ok(left_out.clone()), "released, and still waiting");

    github.issues[1].is_open = false;
    assert_eq!(task(&github, &["ready"]), Ok(left_out.clone()), "a closed PR takes no fix");
    let closed = task(&github, &["claim", "issue-3"]).expect_err("a human's");
    assert!(closed.contains("#45 is closed or merged, but issue #3 is still open"), "{closed}");
    github.issues[1] = Issue { is_pull_request: false, is_open: true, ..github.issues[1].clone() };
    assert_eq!(task(&github, &["ready"]), Ok(left_out), "#45 is an issue, not a PR");
    let no_pull = task(&github, &["claim", "issue-3"]).expect_err("a human's");
    assert!(no_pull.contains("#45, which issue #3 names, is not a pull request"), "{no_pull}");
    github.issues[1].is_pull_request = true;
    github.issues[0].author = "owner".to_owned();
    assert_eq!(task(&github, &["ready"]), Ok(listed), "only the bot files queue failures");
    let plain = task(&github, &["claim", "issue-3"]).expect("an ordinary issue");
    assert!(plain.contains("work on the branch `task/issue-3`"), "{plain}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: without the repository's `needs-decision` label, renamed or deleted, nobody can tell
// which issues wait for a decision: `ready` lists no issue and says why, still naming the plan
// tasks, and `claim` refuses every issue.
#[test]
fn a_missing_decision_label_holds_every_issue() {
    let (root, clone) = setup("label-gone");
    push_code_owners(&clone);
    let github = Fake {
        issues: vec![issue(3, "nami-agent[bot]", &["bug"])],
        is_label_gone: true,
        ..Fake::default()
    };
    let task = |arguments: &[&str]| execute(&clone, "origin", &github, arguments);
    let ready = task(&["ready"]).expect_err("an incomplete list");
    assert!(ready.contains("has no `needs-decision` label: it was renamed or deleted"), "{ready}");
    assert!(ready.ends_with("\na\tTask a\n"), "the plan tasks are still named: {ready}");
    let claim = task(&["claim", "issue-3"]).expect_err("held");
    assert!(claim.contains("cannot claim `issue-3`: the repository has no"), "{claim}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: a queue failure of a PR that the bot did not open is held whatever its labels say, so
// lifting `needs-decision` (point 7) never sends agents to push to a human's PR (point 6).
#[test]
fn a_humans_failed_pull_request_is_held_without_the_label() {
    let (root, clone) = setup("queue-failures-human");
    push_code_owners(&clone);
    let human = Issue { author: "owner".to_owned(), ..pull_45("Summary.\n") };
    let github = Fake { issues: vec![queue_failure(3, 900), human], ..Fake::default() };
    let task = |arguments: &[&str]| execute(&clone, "origin", &github, arguments);
    assert_eq!(task(&["ready"]), Ok("a\tTask a\n".to_owned()), "the issue is held");
    let refused = task(&["claim", "issue-3"]).expect_err("a human's PR");
    assert!(refused.contains("#45 was not opened by the bot, so agents never push"), "{refused}");
    assert!(refused.contains("a human rebases it (ADR-0021, point 6)"), "{refused}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: a queue-failure issue that the CI app filed waits for its fix like the bot's, and is
// claimed for the PR's own branch; any other issue by the CI app is neither listed nor
// claimable (ADR-0023).
#[test]
fn the_ci_apps_queue_failures_wait_like_the_bots() {
    let (root, clone) = setup("queue-failures-ci");
    push_code_owners(&clone);
    let failure = Issue { author: "nami-ci[bot]".to_owned(), ..queue_failure(3, 900) };
    let listed = format!("a\tTask a\nissue-3\t{}\n", failure.title);
    let other = issue(5, "nami-ci[bot]", &["bug"]);
    let mut github = Fake { issues: vec![failure, other, pull_45("Fixes #3")], ..Fake::default() };
    let task = |fake: &Fake, arguments: &[&str]| execute(&clone, "origin", fake, arguments);
    assert_eq!(task(&github, &["ready"]), Ok("a\tTask a\n".to_owned()), "#3 waits, #5 is out");
    let refused = task(&github, &["claim", "issue-3"]).expect_err("it waits");
    assert!(refused.contains("#45 says `Fixes #3`: the fix is in flight"), "{refused}");
    let untrusted = task(&github, &["claim", "issue-5"]).expect_err("not a queue failure");
    assert!(untrusted.contains("`nami-ci[bot]`, whom agents trust only"), "{untrusted}");
    github.issues[2].body = "Summary.".to_owned();
    assert_eq!(task(&github, &["ready"]), Ok(listed), "no fix in flight: work for an agent");
    let claimed = task(&github, &["claim", "issue-3"]).expect("work for an agent");
    assert!(claimed.contains("fix it on #45's own branch"), "{claimed}");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: a second failed run of the same PR is new work. Its issue is ready while the PR says
// `Fixes` only for the first one, and waits too once the PR closes both (ADR-0021, point 6).
#[test]
fn a_second_queue_failure_is_ready_while_the_first_waits() {
    let (root, clone) = setup("queue-failures-chained");
    push_code_owners(&clone);
    let (first, second) = (queue_failure(3, 900), queue_failure(4, 950));
    let second_line = format!("a\tTask a\nissue-4\t{}\n", second.title);
    let mut github = Fake { issues: vec![first, second, pull_45("Fixes #3")], ..Fake::default() };
    let task = |fake: &Fake, arguments: &[&str]| execute(&clone, "origin", fake, arguments);
    assert_eq!(task(&github, &["ready"]), Ok(second_line), "the second failure is new work");
    assert!(task(&github, &["claim", "issue-3"]).expect_err("waits").contains("in flight"));
    let claimed = task(&github, &["claim", "issue-4"]).expect("the new failure");
    assert!(claimed.contains("add `Fixes #4` to #45's description"), "{claimed}");
    assert_eq!(task(&github, &["release", "issue-4"]), Ok("released issue-4\n".to_owned()));
    github.issues[2].body = "Fixes #3\nFixes #4".to_owned();
    assert_eq!(task(&github, &["ready"]), Ok("a\tTask a\n".to_owned()), "both wait now");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: a lookup that fails for one issue leaves out only that issue: `ready` fails, says how
// many it left out, and still lists the rest.
#[test]
fn a_failed_lookup_leaves_out_only_its_issue() {
    let (root, clone) = setup("ready-left-out");
    push_code_owners(&clone);
    // Issue #3 names PR #45, which GitHub does not have.
    let github = Fake {
        issues: vec![queue_failure(3, 900), issue(5, "nami-agent[bot]", &["bug"])],
        ..Fake::default()
    };
    let ready = execute(&clone, "origin", &github, &["ready"]).expect_err("incomplete");
    assert_eq!(
        ready,
        "1 issue(s) left out because their state is unknown, so this list is incomplete; the \
         rest of the list:\na\tTask a\nissue-5\tIssue 5\n"
    );
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
