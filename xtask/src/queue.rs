//! `cargo xtask queue`: the merge queue's two blind spots (issue #51).
//!
//! - `clashes --pr <n> [--comment]` lists the ADR numbers that PR `<n>` adds while `main` or
//!   another open PR by a trusted author also uses them. With `--comment` it posts one comment
//!   on the PR, or updates the one it posted before, saying who holds each number and that
//!   whoever lands second renumbers (ADR-0000). Advisory: it never fails on a clash.
//! - `failed --run <id>` files an issue of kind `bug` for the PR whose merge-queue run `<id>`
//!   failed, through the path of `cargo xtask issue file`, so that `cargo xtask task ready`
//!   lists it and the next session rebases or renumbers the PR on its own branch (ADR-0021,
//!   point 6). It files nothing unless the PR really left the queue: it is open, not merged,
//!   not queued, and no newer merge-queue run tested it. A trusted open issue with the same
//!   title stops a re-run from filing twice. A PR that the bot did not open gets
//!   `needs-decision`, because agents do not push to it.
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
use failure::{FailedJob, Failure, QueueEntry, Vocabulary};
use forge::{Forge, PullState, Run};
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
    let main = format!("{remote}/{MAIN}");
    let gh = Gh::new(&root);
    let vocabulary = || Vocabulary::load(&root, &main);
    let result = trust::load(&root, &main)
        .and_then(|trust| execute(&gh, &gh, &trust, &vocabulary, arguments));
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

/// Runs one command against `forge` and `github`, trusting the authors in `trust`, and quoting
/// only what `vocabulary` returns. Returns what the command prints, or the error to report.
///
/// # Errors
///
/// Bad options, or a failure to reach GitHub or to load the vocabulary.
pub fn execute(
    forge: &dyn Forge,
    github: &dyn GitHub,
    trust: &Trust,
    vocabulary: &dyn Fn() -> Result<Vocabulary, String>,
    arguments: &[&str],
) -> Result<String, String> {
    match arguments {
        ["clashes", "--pr", number] => check_clashes(forge, trust, parse_pull(number)?, false),
        ["clashes", "--pr", number, "--comment"] | ["clashes", "--comment", "--pr", number] => {
            check_clashes(forge, trust, parse_pull(number)?, true)
        }
        ["failed", "--run", id] => file_failure(forge, github, trust, vocabulary, parse_run(id)?),
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
    let own = clashes::added_adrs(&files)?;
    let main = forge.adr_names_on(MAIN)?;
    let mut others = Vec::new();
    if !own.is_empty() {
        for other in forge.open_pulls()? {
            if other.number != pull && trust.is_trusted(&other.author) {
                others.push((other.number, clashes::added_adrs(&forge.pull_files(other.number)?)?));
            }
        }
    }
    let found = clashes::clashes(&own, &main, &clashes::removed_adrs(&files)?, &others);
    let next = clashes::next_free(
        main.iter().chain(own.iter()).chain(others.iter().flat_map(|(_, names)| names.iter())),
    );
    let mut output = summary(pull, &found, next);
    if comment {
        output.push_str(&publish(forge, pull, &found, next)?);
    }
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

/// Files the issue for failed merge-queue run `run`, unless it is not one, the PR is still in
/// play, or a trusted open issue already records the PR's failure.
fn file_failure(
    forge: &dyn Forge,
    github: &dyn GitHub,
    trust: &Trust,
    vocabulary: &dyn Fn() -> Result<Vocabulary, String>,
    run: u64,
) -> Result<String, String> {
    let details = forge.run(run)?;
    if details.id != run {
        return Err(format!("GitHub answered with run {} when asked for run {run}", details.id));
    }
    if details.path != failure::CI_WORKFLOW {
        return Ok(format!(
            "run {run} is a run of `{}`, not of {}; nothing to file\n",
            details.path,
            failure::CI_WORKFLOW
        ));
    }
    if details.event != "merge_group" || details.conclusion.as_deref() != Some("failure") {
        return Ok(format!("run {run} is not a failed merge-queue run; nothing to file\n"));
    }
    let entry = failure::queue_entry(&details.head_branch).ok_or_else(|| {
        format!("run {run} ran on `{}`, which is not a merge-queue branch", details.head_branch)
    })?;
    let pull = entry.pull;
    let state = forge.pull_state(pull)?;
    if let Some(reason) = still_in_play(forge, &details, &entry, &state)? {
        return Ok(format!("{reason}; nothing to file\n"));
    }
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
    let vocabulary = vocabulary()?;
    let report = Failure {
        pull,
        group: group_of(forge, &entry, &details)?,
        repository: forge.repository()?,
        run,
        jobs: failed_jobs(forge, run, &vocabulary)?,
        author: state.author,
    };
    let draft = Draft::parse(&failure::finding(&report, &vocabulary))?;
    assert_eq!(draft.title, title, "the issue carries the title that re-runs look for");
    let by_bot = report.author == BOT_LOGIN;
    let options = FileOptions { kind: "bug", needs_decision: !by_bot, new: true, path: "-" };
    let number = issue::file_number(&draft, &options, github, trust)?;
    Ok(format!("filed #{number}: {title}\n"))
}

/// Why PR `entry.pull` has not left the queue for good after run `details`, or `None` if it
/// has: it merged or closed, it is queued again, or a newer merge-queue run of the same
/// workflow tested it. A run of a group that GitHub rebuilt fails without removing its PR.
fn still_in_play(
    forge: &dyn Forge,
    details: &Run,
    entry: &QueueEntry,
    state: &PullState,
) -> Result<Option<String>, String> {
    let pull = entry.pull;
    if state.is_merged {
        return Ok(Some(format!("#{pull} has merged")));
    }
    if !state.is_open {
        return Ok(Some(format!("#{pull} is closed")));
    }
    if state.is_queued {
        return Ok(Some(format!("#{pull} is in the merge queue")));
    }
    let runs = forge.queue_runs_since(&details.created_at)?;
    let newer = runs.iter().find(|other| {
        other.workflow_id == details.workflow_id
            && (&other.created_at, other.id) > (&details.created_at, details.id)
            && failure::queue_entry(&other.head_branch)
                .is_some_and(|other| other.pull == pull && other.base == entry.base)
    });
    Ok(newer.map(|newer| format!("run {} tested #{pull} again after run {}", newer.id, details.id)))
}

/// The other PRs in the merge group of `entry`: the squashed commits between the group's base
/// and the tested commit.
fn group_of(forge: &dyn Forge, entry: &QueueEntry, details: &Run) -> Result<BTreeSet<u32>, String> {
    let messages = forge.commit_messages(&entry.base_sha, &details.head_sha)?;
    let numbers: Vec<u32> =
        messages.iter().filter_map(|message| failure::pull_of_message(message)).collect();
    let group: BTreeSet<u32> =
        numbers.iter().copied().filter(|number| *number != entry.pull).collect();
    Ok(group)
}

/// The jobs of run `run` that failed or timed out, each with the codes of `vocabulary` that
/// its log reports.
fn failed_jobs(
    forge: &dyn Forge,
    run: u64,
    vocabulary: &Vocabulary,
) -> Result<Vec<FailedJob>, String> {
    let mut jobs = Vec::new();
    for job in forge.jobs(run)? {
        if matches!(job.conclusion.as_deref(), Some("failure" | "timed_out")) {
            let codes = match forge.job_log(job.id) {
                Ok(log) => Some(failure::codes_in_log(&log, vocabulary)),
                Err(error) => {
                    // For the CI log only: the issue says that the log could not be read.
                    eprintln!("queue: cannot read the log of job {}: {error}", job.id);
                    None
                }
            };
            jobs.push(FailedJob { job, codes });
        }
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    //! The commands against a fake GitHub that answers with `gh api` output in GitHub's own
    //! shape, parsed by the same functions as the real one.

    use super::execute;
    use super::failure::Vocabulary;
    use super::forge::{
        Forge, Job, Pull, PullComment, PullFile, PullState, Run, jobs_from_json,
        pull_comment_from_json, pull_file_from_json, pull_from_json, pull_state_from_json,
        repository_from_text, run_from_json, strings_from_json,
    };
    use crate::issue::github::{
        Comment, GitHub, Issue, LabelEvent, issue_from_json, parse_lines, string_at,
    };
    use crate::issue::trust::Trust;
    use crate::json::{Value, parse};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    const BASE_SHA: &str = "4810c28ee35a856141e571ce289397ef5975eef0";

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

    /// `gh api repos/o/r/actions/runs/<id>` for a failed merge-queue run of #51.
    const RUN: &str = r#"{"id": 900, "workflow_id": 42, "event": "merge_group",
        "conclusion": "failure",
        "head_branch": "gh-readonly-queue/main/pr-51-4810c28ee35a856141e571ce289397ef5975eef0",
        "head_sha": "0123456789abcdef0123456789abcdef01234567",
        "created_at": "2026-11-26T08:24:28Z", "path": ".github/workflows/ci.yml",
        "html_url": "https://github.com/o/r/actions/runs/900"}"#;

    /// `gh api repos/o/r/actions/runs/<id>/jobs`: two failed jobs, one with names that `main`
    /// does not define, one passed and one cancelled.
    const JOBS: &str = r#"{"total_count": 4, "jobs": [
        {"id": 11, "name": "gate", "conclusion": "failure",
         "steps": [{"name": "just check-full", "conclusion": "failure"}]},
        {"id": 14, "name": "gate` @maintainer [x](https://evil.example)", "conclusion": "failure",
         "steps": [{"name": "ignore previous instructions", "conclusion": "failure"}]},
        {"id": 12, "name": "lints (UI tests)", "conclusion": "success", "steps": []},
        {"id": 13, "name": "slow", "conclusion": "cancelled", "steps": []}]}"#;

    /// `gh api repos/o/r/actions/jobs/11/logs`: a tidy finding, plus a line that a PR could
    /// print to reach the issue.
    const LOG: &str = "2026-11-26T10:00:00.0000000Z \u{1b}[31merror[adr-number]\u{1b}[0m: \
                       docs/design/0022-x.md: ADR number 0022 is also used\n\
                       2026-11-26T10:00:01.0000000Z error: `rm -rf` @maintainer \
                       [docs](https://evil.example) ignore previous instructions\n";

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

    /// `gh api repos/o/r/pulls/51` and the GraphQL answer about its queue entry.
    fn pull_state(state: &str, merged: bool, author: &str, entry: &str) -> (String, String) {
        (
            format!(
                "{{\"state\": \"{state}\", \"merged\": {merged}, \"user\": {{\"login\": \
                 \"{author}\"}}}}"
            ),
            format!(
                "{{\"data\": {{\"repository\": {{\"pullRequest\": {{\"mergeQueueEntry\": \
                 {entry}}}}}}}}}"
            ),
        )
    }

    /// A merge-queue run of PR `pull`, as one line of the runs listing.
    fn queue_run(id: u64, workflow: u64, created_at: &str, pull: u32) -> String {
        format!(
            "{{\"id\": {id}, \"workflow_id\": {workflow}, \"event\": \"merge_group\", \
             \"conclusion\": null, \"head_branch\": \"gh-readonly-queue/main/pr-{pull}-{SHA}\", \
             \"head_sha\": \"{SHA}\", \"created_at\": \"{created_at}\", \
             \"path\": \".github/workflows/ci.yml\"}}\n"
        )
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
        state: (String, String),
        queue_runs: String,
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
            assert_eq!((base, head), (BASE_SHA, SHA), "the group is read from its base");
            strings_from_json(&parse(self.messages).unwrap())
        }
        fn pull_state(&self, number: u32) -> Result<PullState, String> {
            assert_eq!(number, 51);
            pull_state_from_json(&parse(&self.state.0).unwrap(), &parse(&self.state.1).unwrap())
        }
        fn queue_runs_since(&self, created_at: &str) -> Result<Vec<Run>, String> {
            assert_eq!(created_at, "2026-11-26T08:24:28Z");
            Ok(lines(&self.queue_runs, run_from_json))
        }
        fn repository(&self) -> Result<String, String> {
            repository_from_text("namicfd/nami\n")
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

    fn vocabulary() -> Vocabulary {
        let names = |list: &[&str]| list.iter().map(|name| (*name).to_owned()).collect();
        Vocabulary {
            names: names(&["gate", "just check-full", "lints (UI tests)"]),
            rules: names(&["adr-number"]),
            lints: names(&["nami_recursion"]),
        }
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
        execute(fake, fake, &trust(), &|| Ok(vocabulary()), arguments)
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

    /// A failed run of #51, which left the queue: open, not merged, not queued, no newer run.
    fn dropped() -> Fake {
        Fake {
            run: RUN.to_owned(),
            messages: r#"["Ratchet (#51)", "Architecture ADRs (#45)\n\nbody"]"#,
            state: pull_state("open", false, "nami-agent[bot]", "null"),
            queue_runs: queue_run(900, 42, "2026-11-26T08:24:28Z", 51),
            issues: issues(),
            ..Fake::default()
        }
    }

    // Goal: a PR that really dropped out gets one `bug` issue that names it, the other PR of
    // its group, the failed jobs by `main`'s names and the tidy rule from the log, with links
    // built from IDs; nothing a PR controls reaches it, an outsider's issue with the same title
    // does not stop it, and passed or cancelled jobs are not reported.
    #[test]
    fn a_pull_request_that_dropped_out_gets_an_issue() {
        let fake = dropped();
        let output = run(&fake, &["failed", "--run", "900"]).expect("filed");
        assert_eq!(output, "filed #70: PR #51 failed in the merge queue\n", "no PR to list it in");
        let filed = fake.filed.borrow();
        assert_eq!(filed.len(), 1);
        let (title, body, labels) = &filed[0];
        assert_eq!(title, "PR #51 failed in the merge queue");
        assert_eq!(labels, &["bug"], "the bot's PR is work for agents");
        for part in [
            "- Found in: merge-queue run https://github.com/namicfd/nami/actions/runs/900\n",
            "The same merge group also held #45, so",
            "- Job `gate` failed at `just check-full` ([log](https://github.com/namicfd/nami/\
             actions/runs/900/job/11)). Tidy rules reported: `adr-number`.\n",
            "- Another job failed at another step ([log](https://github.com/namicfd/nami/actions/\
             runs/900/job/14)). Its log could not be read.\n",
            "Rebase or renumber, then re-queue, on #51's own branch",
        ] {
            assert!(body.contains(part), "{part}\n---\n{body}");
        }
        for planted in ["0022-x.md", "rm -rf", "@", "evil.example", "ignore previous", "slow"] {
            assert!(!body.contains(planted), "{planted}\n---\n{body}");
        }
        assert!(!body.contains("lints (UI tests)") && !body.contains("Architecture"), "{body}");
    }

    // Goal: a PR that the bot did not open is filed with `needs-decision`, so that agents do
    // not take it, and the issue leaves the rebase to a human.
    #[test]
    fn a_humans_pull_request_is_left_to_a_human() {
        let fake = Fake { state: pull_state("open", false, "owner", "null"), ..dropped() };
        let output = run(&fake, &["failed", "--run", "900"]).expect("filed");
        assert!(output.starts_with("filed #70"), "{output}");
        let filed = fake.filed.borrow();
        assert_eq!(filed[0].2, ["bug", "needs-decision"]);
        assert!(filed[0].1.contains("a human rebases or renumbers it"), "{}", filed[0].1);
    }

    // Goal: nothing is filed for a PR that merged, closed, is queued again, or that a newer run
    // of the same workflow tested, also one created in the same second; a newer run of another
    // workflow or of another PR, the run itself and older runs do not count.
    #[test]
    fn a_pull_request_still_in_play_gets_no_issue() {
        let open = || pull_state("open", false, "nami-agent[bot]", "null");
        let cases = [
            (pull_state("closed", true, "nami-agent[bot]", "null"), "", "#51 has merged"),
            (pull_state("closed", false, "nami-agent[bot]", "null"), "", "#51 is closed"),
            (
                pull_state("open", false, "nami-agent[bot]", r#"{"state": "QUEUED"}"#),
                "",
                "#51 is in the merge queue",
            ),
            (open(), "2026-11-26T08:30:00Z", "run 901 tested #51 again after run 900"),
            (open(), "2026-11-26T08:24:28Z", "run 901 tested #51 again after run 900"),
        ];
        for (state, newer, expected) in cases {
            let queue_runs =
                if newer.is_empty() { String::new() } else { queue_run(901, 42, newer, 51) };
            let fake = Fake { state, queue_runs, ..dropped() };
            let output = run(&fake, &["failed", "--run", "900"]).unwrap();
            assert_eq!(output, format!("{expected}; nothing to file\n"));
            assert!(fake.filed.borrow().is_empty(), "{expected}");
        }
        let unrelated = queue_run(902, 43, "2026-11-26T08:30:00Z", 51)
            + &queue_run(903, 42, "2026-11-26T08:30:00Z", 52)
            + &queue_run(900, 42, "2026-11-26T08:24:28Z", 51)
            + &queue_run(899, 42, "2026-11-26T08:24:28Z", 51)
            + &queue_run(904, 42, "2026-11-26T08:20:00Z", 51);
        let fake = Fake { queue_runs: unrelated, ..dropped() };
        assert!(run(&fake, &["failed", "--run", "900"]).unwrap().starts_with("filed #70"));
    }

    // Goal: only a run of `ci.yml` counts, so a workflow that a PR adds under the name `ci`
    // files nothing; an answer about another run than the one asked for is an error.
    #[test]
    fn only_runs_of_ci_count() {
        let renamed = RUN.replace(".github/workflows/ci.yml", ".github/workflows/mine.yml");
        let fake = Fake { run: renamed, ..dropped() };
        let output = run(&fake, &["failed", "--run", "900"]).unwrap();
        assert_eq!(
            output,
            "run 900 is a run of `.github/workflows/mine.yml`, not of .github/workflows/ci.yml; \
             nothing to file\n"
        );
        assert!(fake.filed.borrow().is_empty());
        let other = Fake { run: RUN.replace("\"id\": 900", "\"id\": 901"), ..dropped() };
        let error = run(&other, &["failed", "--run", "900"]).expect_err("another run");
        assert_eq!(error, "GitHub answered with run 901 when asked for run 900");
    }

    // Goal: a re-run finds the bot's open issue by its title and files nothing; runs that are
    // not failed merge-queue runs file nothing; a run on another branch is an error.
    #[test]
    fn re_runs_and_other_runs_file_nothing() {
        let title = "PR #51 failed in the merge queue";
        let recorded = issues() + &issue_line(9, title, "nami-agent[bot]", "open");
        let again = Fake { issues: recorded, ..dropped() };
        let skipped = run(&again, &["failed", "--run", "900"]).unwrap();
        assert_eq!(skipped, "#9 already records that #51 failed; not filed again\n");
        assert!(again.filed.borrow().is_empty());
        let closed = issues() + &issue_line(9, title, "nami-agent[bot]", "closed");
        let reopened = Fake { issues: closed, ..dropped() };
        assert!(run(&reopened, &["failed", "--run", "900"]).unwrap().starts_with("filed #70"));

        for other in
            [RUN.replace("merge_group", "pull_request"), RUN.replace("\"failure\"", "null")]
        {
            let other = Fake { run: other, ..dropped() };
            let ignored = run(&other, &["failed", "--run", "900"]).unwrap();
            assert_eq!(ignored, "run 900 is not a failed merge-queue run; nothing to file\n");
            assert!(other.filed.borrow().is_empty());
        }
        let odd =
            Fake { run: RUN.replace("gh-readonly-queue/main/pr-51-", "task/x-"), ..dropped() };
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
