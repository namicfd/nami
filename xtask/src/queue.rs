//! `cargo xtask queue`: the merge queue's two blind spots (issue #51).
//!
//! - `clashes --pr <n> [--comment]` lists the ADR numbers that PR `<n>` adds while `main` or
//!   another open PR by a trusted author also uses them. With `--comment` it posts one comment
//!   on the PR, or updates the one it posted before, saying who holds each number and that
//!   whoever lands second renumbers (ADR-0000). Advisory: it never fails on a clash.
//! - `failed --run <id>` files an issue of kind `bug` for the PR whose merge-queue run `<id>`
//!   failed, through the path of `cargo xtask issue file`, so that `cargo xtask task ready`
//!   lists it and the next session rebases or renumbers the PR. It files nothing when a trusted
//!   open issue has the same title already, so a re-run files no duplicate.
//!
//! Both act on the checkout's GitHub repository as whoever `GH_TOKEN` names, and read the code
//! owners from `.github/CODEOWNERS` on `<remote>/main` (`$NAMI_TASK_REMOTE`, or `origin`). The
//! workflow `.github/workflows/queue.yml` runs them.

pub mod clashes;
pub mod failure;
pub mod forge;

use crate::issue::github::{Gh, GitHub};
use crate::issue::trust::{self, BOT_LOGIN, Trust};
use crate::issue::{self, Draft, FileOptions};
use clashes::{Clash, MARKER};
use failure::{FailedJob, Failure};
use forge::Forge;
use std::collections::BTreeSet;
use std::process::ExitCode;

const USAGE: &str =
    "usage: cargo xtask queue clashes --pr <number> [--comment] | failed --run <id>";

/// The logins whose comment with the [`MARKER`] this check updates: the workflow's own token,
/// and the bot. A marker in anyone else's comment is ignored.
const COMMENT_AUTHORS: [&str; 2] = ["github-actions[bot]", BOT_LOGIN];

/// The branch whose ADRs every PR is compared with.
const MAIN: &str = "main";

/// Runs one `cargo xtask queue` command in this checkout: prints its output, or its error.
pub fn run(arguments: &[&str]) -> ExitCode {
    let root = crate::workspace_root();
    let remote = std::env::var("NAMI_TASK_REMOTE").unwrap_or_else(|_| "origin".to_owned());
    let gh = Gh::new(&root);
    let result = trust::load(&root, &format!("{remote}/{MAIN}"))
        .and_then(|trust| execute(&gh, &gh, &trust, arguments));
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("queue: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one command against `forge` and `github`, trusting the authors in `trust`. Returns
/// what the command prints, or the error to report.
///
/// # Errors
///
/// Bad options, or a failure to reach GitHub.
pub fn execute(
    forge: &dyn Forge,
    github: &dyn GitHub,
    trust: &Trust,
    arguments: &[&str],
) -> Result<String, String> {
    match arguments {
        ["clashes", "--pr", number] => check_clashes(forge, trust, parse_pull(number)?, false),
        ["clashes", "--pr", number, "--comment"] | ["clashes", "--comment", "--pr", number] => {
            check_clashes(forge, trust, parse_pull(number)?, true)
        }
        ["failed", "--run", id] => file_failure(forge, github, trust, parse_run(id)?),
        _ => Err(USAGE.to_owned()),
    }
}

fn parse_pull(text: &str) -> Result<u32, String> {
    let number = issue::canonical_number(text.strip_prefix('#').unwrap_or(text));
    number.ok_or_else(|| format!("`{text}` is not a pull request number; {USAGE}"))
}

fn parse_run(text: &str) -> Result<u64, String> {
    let id = text.parse::<u64>().ok().filter(|id| *id > 0 && !text.starts_with(['0', '+']));
    id.ok_or_else(|| format!("`{text}` is not a workflow run ID; {USAGE}"))
}

/// The ADR clashes of PR `pull`, printed, and with `comment` also posted on the PR.
fn check_clashes(
    forge: &dyn Forge,
    trust: &Trust,
    pull: u32,
    comment: bool,
) -> Result<String, String> {
    let files = forge.pull_files(pull)?;
    let own = clashes::added_adrs(&files);
    let main = forge.adr_names_on(MAIN)?;
    let mut others = Vec::new();
    if !own.is_empty() {
        for other in forge.open_pulls()? {
            if other.number != pull && trust.is_trusted(&other.author) {
                others.push((other.number, clashes::added_adrs(&forge.pull_files(other.number)?)));
            }
        }
    }
    let found = clashes::clashes(&own, &main, &clashes::removed_adrs(&files), &others);
    let next = clashes::next_free(
        main.iter().chain(own.iter()).chain(others.iter().flat_map(|(_, names)| names.iter())),
    );
    let mut output = summary(pull, &found, next);
    if comment {
        output.push_str(&publish(forge, pull, &found, next)?);
    }
    assert!(found.is_empty() || output.contains("ADR-"), "every clash is reported");
    Ok(output)
}

/// What `clashes` prints: one line per clash, or that there is none.
fn summary(pull: u32, found: &BTreeSet<Clash>, next: u32) -> String {
    if found.is_empty() {
        return format!("#{pull} adds no ADR number that is in use elsewhere\n");
    }
    let lines: Vec<String> = found
        .iter()
        .map(|clash| match clash.holder {
            clashes::Holder::Main => format!("warning: ADR-{:04} is already on main", clash.number),
            clashes::Holder::Pull(other) => {
                format!("warning: ADR-{:04} is also added by #{other}", clash.number)
            }
        })
        .collect();
    format!("{}\nthe next free number is ADR-{next:04}\n", lines.join("\n"))
}

/// Posts the clash comment on PR `pull`, or updates the one posted before. With no clash,
/// it updates an earlier comment to say so, and posts nothing new.
fn publish(
    forge: &dyn Forge,
    pull: u32,
    found: &BTreeSet<Clash>,
    next: u32,
) -> Result<String, String> {
    let comments = forge.pull_comments(pull)?;
    let earlier = comments.iter().find(|comment| {
        COMMENT_AUTHORS.contains(&comment.author.as_str()) && comment.body.starts_with(MARKER)
    });
    let body = if found.is_empty() { clashes::resolved() } else { clashes::render(found, next) };
    assert!(body.starts_with(MARKER), "the comment can be found again");
    match earlier {
        Some(earlier) if earlier.body == body => Ok("the comment is up to date\n".to_owned()),
        Some(earlier) => {
            forge.edit_comment(earlier.id, &body)?;
            Ok(format!("updated comment {}\n", earlier.id))
        }
        None if found.is_empty() => Ok(String::new()),
        None => {
            forge.post_comment(pull, &body)?;
            Ok(format!("commented on #{pull}\n"))
        }
    }
}

/// Files the issue for failed merge-queue run `run`, unless it is not one, or a trusted open
/// issue already records the PR's failure.
fn file_failure(
    forge: &dyn Forge,
    github: &dyn GitHub,
    trust: &Trust,
    run: u64,
) -> Result<String, String> {
    let details = forge.run(run)?;
    if details.event != "merge_group" || details.conclusion.as_deref() != Some("failure") {
        return Ok(format!("run {run} is not a failed merge-queue run; nothing to file\n"));
    }
    let (base, pull) = failure::queue_entry(&details.head_branch).ok_or_else(|| {
        format!("run {run} ran on `{}`, which is not a merge-queue branch", details.head_branch)
    })?;
    let title = failure::title(pull);
    let open = github.open_issues()?;
    let recorded = open.iter().find(|issue| {
        issue.is_open && issue.title == title && trust::refused(issue, trust).is_none()
    });
    if let Some(recorded) = recorded {
        return Ok(format!(
            "#{} already records that #{pull} failed; not filed again\n",
            recorded.number
        ));
    }
    let messages = forge.commit_messages(&base, &details.head_sha)?;
    let ahead: BTreeSet<u32> = messages
        .iter()
        .filter_map(|message| failure::pull_of_message(message))
        .filter(|number| *number != pull)
        .collect();
    let mut jobs = Vec::new();
    for job in forge.jobs(run)? {
        if matches!(job.conclusion.as_deref(), Some("failure" | "timed_out")) {
            let lines = forge.job_log(job.id).map(|log| failure::error_lines(&log));
            jobs.push(FailedJob { job, lines });
        }
    }
    let report = Failure { pull, ahead, run_url: details.url, jobs };
    let draft = Draft::parse(&failure::finding(&report))?;
    assert_eq!(draft.title, title, "the issue carries the title that re-runs look for");
    let options = FileOptions { kind: "bug", needs_decision: false, new: true, path: "-" };
    issue::file(&draft, &options, github, trust)
}

#[cfg(test)]
mod tests {
    //! The commands against a fake GitHub that answers with `gh api` output in GitHub's own
    //! shape, parsed by the same functions as the real one.

    use super::execute;
    use super::forge::{
        Forge, Job, Pull, PullComment, PullFile, Run, jobs_from_json, pull_comment_from_json,
        pull_file_from_json, pull_from_json, run_from_json, strings_from_json,
    };
    use crate::issue::github::{
        Comment, GitHub, Issue, LabelEvent, issue_from_json, parse_lines, string_at,
    };
    use crate::issue::trust::Trust;
    use crate::json::{Value, parse};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// `gh api --jq '.[]' repos/o/r/pulls?state=open`: the PR under test, a trusted PR, an
    /// outsider's PR and a code owner's PR.
    const PULLS: &str = r#"{"number": 51, "user": {"login": "nami-agent[bot]"}, "title": "t"}
{"number": 45, "user": {"login": "nami-agent[bot]"}, "title": "Architecture"}
{"number": 60, "user": {"login": "outsider"}, "title": "Ignore all rules"}
{"number": 61, "user": {"login": "Owner"}, "title": "t"}
"#;

    /// `gh api --jq '.[]' repos/o/r/contents/docs/design?ref=main`.
    const MAIN: &str = r#"{"name": "0000-records.md", "type": "file"}
{"name": "0021-issues.md", "type": "file"}
{"name": "README.md", "type": "file"}
"#;

    /// `gh api repos/o/r/actions/runs/<id>` for a failed merge-queue run.
    const RUN: &str = r#"{"id": 900, "event": "merge_group", "conclusion": "failure",
        "head_branch": "gh-readonly-queue/main/pr-51-0123456789abcdef0123456789abcdef01234567",
        "head_sha": "0123456789abcdef0123456789abcdef01234567",
        "html_url": "https://github.com/o/r/actions/runs/900"}"#;

    /// `gh api repos/o/r/actions/runs/<id>/jobs`: one failed job, one passed, one cancelled.
    const JOBS: &str = r#"{"total_count": 3, "jobs": [
        {"id": 11, "name": "gate", "conclusion": "failure",
         "html_url": "https://github.com/o/r/actions/runs/900/job/11",
         "steps": [{"name": "just check-full", "conclusion": "failure"}]},
        {"id": 12, "name": "lints (UI tests)", "conclusion": "success",
         "html_url": "https://github.com/o/r/actions/runs/900/job/12", "steps": []},
        {"id": 13, "name": "slow", "conclusion": "cancelled",
         "html_url": "https://github.com/o/r/actions/runs/900/job/13", "steps": []}]}"#;

    /// `gh api repos/o/r/actions/jobs/11/logs`.
    const LOG: &str = "2026-11-26T10:00:00.0000000Z \u{1b}[31merror[adr-number]\u{1b}[0m: \
                       docs/design/0022-x.md: ADR number 0022 is also used\n";

    /// One line of `gh api --jq '.[]' repos/o/r/issues?state=open`.
    fn issue_line(number: u32, title: &str, author: &str, state: &str) -> String {
        format!(
            "{{\"number\": {number}, \"title\": \"{title}\", \"user\": {{\"login\": \
             \"{author}\"}}, \"state\": \"{state}\", \"labels\": [{{\"name\": \"bug\"}}], \
             \"body\": \"\"}}\n"
        )
    }

    /// The open issues: an outsider's issue with the title of #51's failure, and the bot's
    /// issue for another PR.
    fn issues() -> String {
        issue_line(3, "PR #51 failed in the merge queue", "outsider", "open")
            + &issue_line(4, "PR #52 failed in the merge queue", "nami-agent[bot]", "open")
    }

    /// GitHub as fixed `gh api` output, recording what is written.
    #[derive(Default)]
    struct Fake {
        pulls: &'static str,
        main: &'static str,
        files: BTreeMap<u32, String>,
        comments: String,
        run: String,
        messages: &'static str,
        issues: String,
        posted: RefCell<Vec<(u32, String)>>,
        edited: RefCell<Vec<(u64, String)>>,
        filed: RefCell<Vec<(String, String, Vec<String>)>>,
    }

    fn lines<T>(text: &str, read: fn(&Value) -> Result<T, String>) -> Vec<T> {
        parse_lines(text).unwrap().iter().map(|value| read(value).unwrap()).collect()
    }

    fn name_of(entry: &Value) -> Result<String, String> {
        string_at(entry, &["name"])
    }

    impl Forge for Fake {
        fn adr_names_on(&self, branch: &str) -> Result<Vec<String>, String> {
            assert_eq!(branch, "main");
            Ok(lines(self.main, name_of))
        }
        fn open_pulls(&self) -> Result<Vec<Pull>, String> {
            Ok(lines(self.pulls, pull_from_json))
        }
        fn pull_files(&self, number: u32) -> Result<Vec<PullFile>, String> {
            Ok(lines(self.files.get(&number).map_or("", String::as_str), pull_file_from_json))
        }
        fn pull_comments(&self, _number: u32) -> Result<Vec<PullComment>, String> {
            Ok(lines(&self.comments, pull_comment_from_json))
        }
        fn post_comment(&self, number: u32, body: &str) -> Result<(), String> {
            self.posted.borrow_mut().push((number, body.to_owned()));
            Ok(())
        }
        fn edit_comment(&self, id: u64, body: &str) -> Result<(), String> {
            self.edited.borrow_mut().push((id, body.to_owned()));
            Ok(())
        }
        fn run(&self, id: u64) -> Result<Run, String> {
            assert_eq!(id, 900);
            run_from_json(&parse(&self.run).unwrap())
        }
        fn jobs(&self, _id: u64) -> Result<Vec<Job>, String> {
            jobs_from_json(&parse(JOBS).unwrap())
        }
        fn job_log(&self, id: u64) -> Result<String, String> {
            if id == 11 { Ok(LOG.to_owned()) } else { Err(format!("no log for {id}")) }
        }
        fn commit_messages(&self, base: &str, head: &str) -> Result<Vec<String>, String> {
            assert_eq!((base, head), ("main", SHA));
            strings_from_json(&parse(self.messages).unwrap())
        }
    }

    impl GitHub for Fake {
        fn issue(&self, number: u32) -> Result<Issue, String> {
            Err(format!("not asked for #{number}"))
        }
        fn open_issues(&self) -> Result<Vec<Issue>, String> {
            Ok(lines(&self.issues, issue_from_json))
        }
        fn comments(&self, number: u32) -> Result<Vec<Comment>, String> {
            Err(format!("not asked for #{number}"))
        }
        fn label_events(&self, number: u32) -> Result<Vec<LabelEvent>, String> {
            Err(format!("not asked for #{number}"))
        }
        fn create(&self, title: &str, body: &str, labels: &[&str]) -> Result<u32, String> {
            let labels = labels.iter().map(|label| (*label).to_owned()).collect();
            self.filed.borrow_mut().push((title.to_owned(), body.to_owned(), labels));
            Ok(70)
        }
    }

    fn trust() -> Trust {
        Trust::from_codeowners("/docs/design/ @owner\n")
    }

    /// `gh api --jq '.[]' repos/o/r/pulls/<n>/files` for ADR files with these statuses.
    fn files(entries: &[(&str, &str)]) -> String {
        entries
            .iter()
            .map(|(name, status)| {
                format!("{{\"filename\": \"docs/design/{name}\", \"status\": \"{status}\"}}\n")
            })
            .collect::<Vec<String>>()
            .concat()
    }

    /// A comment by `author` with `body`, as one line of `gh api --jq '.[]'` output.
    fn comment(id: u64, author: &str, body: &str) -> String {
        format!(
            "{{\"id\": {id}, \"user\": {{\"login\": \"{author}\"}}, \"body\": {}}}\n",
            crate::json::quote(body)
        )
    }

    fn clash_fake() -> Fake {
        let files = BTreeMap::from([
            (51, files(&[("0022-ratchet.md", "added"), ("0021-issues.md", "modified")])),
            (45, files(&[("0022-architecture.md", "added")])),
            (60, files(&[("0022-outsider.md", "added")])),
            (61, files(&[("0023-other.md", "added")])),
        ]);
        Fake { pulls: PULLS, main: MAIN, files, ..Fake::default() }
    }

    fn without_adrs(fake: Fake) -> Fake {
        Fake { files: BTreeMap::from([(51, files(&[]))]), ..fake }
    }

    fn run(fake: &Fake, arguments: &[&str]) -> Result<String, String> {
        execute(fake, fake, &trust(), arguments)
    }

    // Goal: a number that a trusted open PR also adds is reported with that PR and the next
    // free number; an outsider's PR is not read, and without `--comment` nothing is posted.
    #[test]
    fn clashes_with_open_pull_requests_are_reported() {
        let fake = clash_fake();
        let output = run(&fake, &["clashes", "--pr", "51"]).expect("advisory, never an error");
        assert_eq!(
            output,
            "warning: ADR-0022 is also added by #45\nthe next free number is ADR-0024\n"
        );
        assert!(fake.posted.borrow().is_empty(), "no comment without --comment");
        let on_main = Fake { main: "{\"name\": \"0022-main.md\"}\n", ..clash_fake() };
        let main_output = run(&on_main, &["clashes", "--pr", "51"]).unwrap();
        assert!(main_output.starts_with("warning: ADR-0022 is already on main\n"), "{main_output}");
        let quiet = without_adrs(clash_fake());
        let quiet_output = run(&quiet, &["clashes", "--pr", "#51", "--comment"]).unwrap();
        assert_eq!(quiet_output, "#51 adds no ADR number that is in use elsewhere\n");
        assert!(quiet.posted.borrow().is_empty() && quiet.edited.borrow().is_empty());
    }

    // Goal: `--comment` posts one comment, then updates that one; a marker in someone else's
    // comment is not ours, and a fixed clash is marked so.
    #[test]
    fn the_clash_comment_is_posted_once_and_then_updated() {
        let marker = super::MARKER;
        let fake = Fake { comments: comment(5, "outsider", marker), ..clash_fake() };
        let output = run(&fake, &["clashes", "--comment", "--pr", "51"]).unwrap();
        assert!(output.ends_with("commented on #51\n"), "{output}");
        let posted = fake.posted.borrow();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].0, 51);
        assert!(posted[0].1.contains("ADR-0022 is also added by #45"), "{}", posted[0].1);
        assert!(!posted[0].1.contains("outsider"), "an outsider's file name is never quoted");
        assert!(fake.edited.borrow().is_empty());

        let old = format!("{marker}\nold");
        let earlier = Fake { comments: comment(8, "github-actions[bot]", &old), ..clash_fake() };
        let updated = run(&earlier, &["clashes", "--pr", "51", "--comment"]).unwrap();
        assert!(updated.ends_with("updated comment 8\n"), "{updated}");
        assert_eq!(earlier.edited.borrow()[0].0, 8);
        assert!(earlier.edited.borrow()[0].1.contains("#45"));
        assert!(earlier.posted.borrow().is_empty(), "one comment per PR");

        let fixed =
            without_adrs(Fake { comments: comment(8, "nami-agent[bot]", &old), ..clash_fake() });
        let marked = run(&fixed, &["clashes", "--pr", "51", "--comment"]).unwrap();
        assert!(marked.ends_with("updated comment 8\n"), "{marked}");
        assert_eq!(fixed.edited.borrow()[0].1, super::clashes::resolved());
    }

    // Goal: a comment that already says what the check would write is not edited again.
    #[test]
    fn an_unchanged_comment_is_left_alone() {
        let resolved = super::clashes::resolved();
        let fake = without_adrs(Fake {
            comments: comment(8, "nami-agent[bot]", &resolved),
            ..clash_fake()
        });
        let output = run(&fake, &["clashes", "--pr", "51", "--comment"]).unwrap();
        assert!(output.ends_with("the comment is up to date\n"), "{output}");
        assert!(fake.edited.borrow().is_empty() && fake.posted.borrow().is_empty());
    }

    fn failure_fake() -> Fake {
        Fake {
            run: RUN.to_owned(),
            messages: r#"["Architecture ADRs (#45)\n\nbody", "Ratchet (#51)"]"#,
            issues: issues(),
            ..Fake::default()
        }
    }

    // Goal: a failed merge-queue run files one `bug` issue for its PR, which names the PR
    // ahead, the failed job and step and the first error line; an outsider's issue with the
    // same title does not stop it, and cancelled or passed jobs are not reported.
    #[test]
    fn a_failed_queue_run_files_an_issue_for_its_pull_request() {
        let fake = failure_fake();
        let output = run(&fake, &["failed", "--run", "900"]).expect("filed");
        assert!(output.starts_with("filed #70: PR #51 failed in the merge queue\n"), "{output}");
        let filed = fake.filed.borrow();
        assert_eq!(filed.len(), 1);
        let (title, body, labels) = &filed[0];
        assert_eq!(title, "PR #51 failed in the merge queue");
        assert_eq!(labels, &["bug"]);
        for part in [
            "- Found in: merge-queue run https://github.com/o/r/actions/runs/900",
            "also held #45, ahead",
            "Job `gate` failed, step `just check-full`",
            "    error[adr-number]: docs/design/0022-x.md: ADR number 0022 is also used\n",
            "Rebase or renumber, then re-queue.",
        ] {
            assert!(body.contains(part), "{part}\n---\n{body}");
        }
        assert!(!body.contains("lints (UI tests)") && !body.contains("slow"), "{body}");
        assert!(!body.contains("Architecture"), "commit subjects are never quoted");
        assert!(!body.contains("#45, #51"), "the PR itself is not one of those ahead");
    }

    // Goal: a re-run finds the bot's open issue by its title and files nothing; runs that are
    // not failed merge-queue runs file nothing; a run on another branch is an error.
    #[test]
    fn re_runs_and_other_runs_file_nothing() {
        let title = "PR #51 failed in the merge queue";
        let recorded = issues() + &issue_line(9, title, "nami-agent[bot]", "open");
        let again = Fake { issues: recorded, ..failure_fake() };
        let skipped = run(&again, &["failed", "--run", "900"]).unwrap();
        assert_eq!(skipped, "#9 already records that #51 failed; not filed again\n");
        assert!(again.filed.borrow().is_empty());
        let closed = issues() + &issue_line(9, title, "nami-agent[bot]", "closed");
        let reopened = Fake { issues: closed, ..failure_fake() };
        assert!(run(&reopened, &["failed", "--run", "900"]).unwrap().starts_with("filed #70"));

        for other in
            [RUN.replace("merge_group", "pull_request"), RUN.replace("\"failure\"", "null")]
        {
            let other = Fake { run: other, ..failure_fake() };
            let ignored = run(&other, &["failed", "--run", "900"]).unwrap();
            assert_eq!(ignored, "run 900 is not a failed merge-queue run; nothing to file\n");
            assert!(other.filed.borrow().is_empty());
        }
        let odd =
            Fake { run: RUN.replace("gh-readonly-queue/main/pr-51-", "task/x-"), ..failure_fake() };
        let error = run(&odd, &["failed", "--run", "900"]).expect_err("not a queue branch");
        assert!(error.contains("not a merge-queue branch"), "{error}");
    }

    // Goal: malformed commands and numbers are errors that show the usage.
    #[test]
    fn bad_arguments_show_the_usage() {
        let fake = Fake::default();
        for arguments in [
            &["clashes"][..],
            &["clashes", "--pr"],
            &["clashes", "--pr", "0"],
            &["clashes", "--pr", "x"],
            &["clashes", "--pr", "1", "--post"],
            &["failed", "--run", "0"],
            &["failed", "--run", "012"],
            &["failed", "--run", "+12"],
            &["failed", "--run", "-1"],
            &["failed"],
            &[],
        ] {
            let error = run(&fake, arguments).expect_err("bad arguments");
            assert!(error.contains("usage: cargo xtask queue"), "{arguments:?}: {error}");
        }
    }
}
