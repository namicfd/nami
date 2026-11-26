//! A pull request that fails in the merge queue becomes an issue (issue #51).
//!
//! GitHub tests each queue entry on its own branch, `gh-readonly-queue/<base>/pr-<n>-<sha>`,
//! whose tree is the base plus every PR ahead of it in the queue plus PR `<n>`. When that run
//! fails, GitHub removes PR `<n>` from the queue, and nothing tells anyone. The failure may
//! still come from a PR ahead of it; the issue then names those PRs too, since the tree held
//! them, and says that the cause may lie there.
//!
//! The issue quotes only what our own CI wrote: job and step names, and log lines. It never
//! quotes a PR's title or description.

use super::forge::Job;
use crate::issue::canonical_number;
use std::collections::BTreeSet;

/// The prefix of the branches that the merge queue tests.
pub const QUEUE_PREFIX: &str = "gh-readonly-queue/";

/// The most log lines quoted per failed job.
pub const ERROR_LINES_MAX: usize = 8;

/// The longest quoted log line, in characters; longer lines are cut.
pub const LINE_CHARS_MAX: usize = 200;

/// A failed job, as the issue reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedJob {
    /// The job's name and its page.
    pub job: Job,
    /// The first error lines of its log, or why there are none.
    pub lines: Result<Vec<String>, String>,
}

/// A failed merge-queue run, as the issue reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The PR that the queue entry was for, which GitHub removed from the queue.
    pub pull: u32,
    /// The other PRs in the tested tree: those ahead of it in the queue.
    pub ahead: BTreeSet<u32>,
    /// The run's page.
    pub run_url: String,
    /// The jobs that failed.
    pub jobs: Vec<FailedJob>,
}

/// The base branch and the PR of a merge-queue branch `gh-readonly-queue/<base>/pr-<n>-<sha>`,
/// or `None` for any other branch.
#[must_use]
pub fn queue_entry(branch: &str) -> Option<(String, u32)> {
    let rest = branch.strip_prefix(QUEUE_PREFIX)?;
    let (base, entry) = rest.rsplit_once('/')?;
    let (number, sha) = entry.strip_prefix("pr-")?.split_once('-')?;
    let is_sha = sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit());
    let pull = canonical_number(number).filter(|_| is_sha && !base.is_empty())?;
    assert!(pull > 0, "a canonical number is positive");
    Some((base.to_owned(), pull))
}

/// The PR that a squashed commit came from: the `(#<n>)` that ends its first line.
#[must_use]
pub fn pull_of_message(message: &str) -> Option<u32> {
    let subject = message.lines().next()?.trim_end();
    let (_, tail) = subject.strip_suffix(')')?.rsplit_once("(#")?;
    let pull = canonical_number(tail);
    assert!(pull.is_none_or(|pull| pull > 0), "a canonical number is positive");
    pull
}

/// The first lines of a job log that report an error, without timestamps and colours, each cut
/// to [`LINE_CHARS_MAX`]. Lines that start with `error`, report a failed or panicked test, or
/// are GitHub's own `##[error]` annotations count; the annotations only when nothing else does,
/// because they say little more than "exit code 1".
#[must_use]
pub fn error_lines(log: &str) -> Vec<String> {
    let cleaned: Vec<String> = log.lines().map(clean).collect();
    let own: Vec<&String> = cleaned.iter().filter(|line| is_error(line)).collect();
    let chosen: Vec<&String> = if own.is_empty() {
        cleaned.iter().filter(|line| line.starts_with("##[error]")).collect()
    } else {
        own
    };
    let lines: Vec<String> =
        chosen.into_iter().take(ERROR_LINES_MAX).map(|line| cut(line)).collect();
    assert!(lines.len() <= ERROR_LINES_MAX, "the quote is bounded");
    lines
}

/// True for a line that our tools print about an error.
fn is_error(line: &str) -> bool {
    line.starts_with("error")
        || line.starts_with("FAIL ")
        || line.starts_with("TIMEOUT ")
        || line.contains("panicked at")
}

/// A log line without its leading timestamp, ANSI colour codes and surrounding blanks.
fn clean(line: &str) -> String {
    let without_time = match line.split_once(' ') {
        Some((stamp, rest)) if is_timestamp(stamp) => rest,
        _ => line,
    };
    let mut text = String::with_capacity(without_time.len());
    let mut in_escape = false;
    for character in without_time.chars() {
        if in_escape {
            in_escape = !character.is_ascii_alphabetic();
        } else if character == '\u{1b}' {
            in_escape = true;
        } else if !character.is_control() {
            text.push(character);
        }
    }
    let cleaned = text.trim().to_owned();
    assert!(!cleaned.contains('\u{1b}'), "no escape is left");
    cleaned
}

/// True for GitHub's log timestamp, such as `2026-11-26T10:00:00.1234567Z`.
fn is_timestamp(word: &str) -> bool {
    let bytes = word.as_bytes();
    bytes.len() >= 20
        && bytes.first().is_some_and(u8::is_ascii_digit)
        && bytes.get(10) == Some(&b'T')
        && bytes.last() == Some(&b'Z')
}

/// `line`, cut to [`LINE_CHARS_MAX`] characters with an ellipsis.
fn cut(line: &str) -> String {
    if line.chars().count() <= LINE_CHARS_MAX {
        return line.to_owned();
    }
    let kept: String = line.chars().take(LINE_CHARS_MAX).collect();
    format!("{kept}…")
}

/// The title of the issue for PR `pull`. Re-runs find an open issue by this exact title.
#[must_use]
pub fn title(pull: u32) -> String {
    assert!(pull > 0, "GitHub numbers pull requests from 1");
    format!("PR #{pull} failed in the merge queue")
}

/// The finding for `failure`, in the format of `cargo xtask issue file`.
#[must_use]
pub fn finding(failure: &Failure) -> String {
    let pull = failure.pull;
    let mut pieces = vec![format!(
        "# {}\n\n- Found in: merge-queue run {}\n\n## What happened\n\nThe merge queue tested \
         #{pull} on top of `main` and the PRs ahead of it, the required checks failed, and \
         GitHub removed #{pull} from the queue.",
        title(pull),
        failure.run_url
    )];
    if !failure.ahead.is_empty() {
        let ahead: Vec<String> = failure.ahead.iter().map(|number| format!("#{number}")).collect();
        pieces.push(format!(
            " The tested tree also held {}, ahead of it in the queue, so the failure may come \
             from one of them instead: check whether their own queue runs failed too.",
            ahead.join(", ")
        ));
    }
    pieces.push("\n\n".to_owned());
    if failure.jobs.is_empty() {
        pieces.push("No job of the run reports a failure; read the run's page.\n".to_owned());
    }
    pieces.extend(failure.jobs.iter().map(job_section));
    pieces.push(format!(
        "\n## Proposed fix\n\nRebase or renumber, then re-queue. Rebase #{pull}'s branch onto \
         `main` and run `just check-full`: a clash that git merged without a conflict shows up \
         there. Two ADRs with one number are the usual case; renumber this PR's ADR: its file \
         name, its `# ADR-NNNN:` title, its row in `docs/design/README.md` and every reference \
         (ADR-0000). Push the fix to #{pull}'s branch, add `Fixes #<this issue>` to its \
         description, and add it to the merge queue again. A push dismisses an approval, so a \
         PR with protected paths needs the code owner again. If #{pull} passes on `main` as it \
         is, the cause was a PR ahead of it: say so on #{pull}, re-queue it and close this \
         issue. If #{pull} has merged or closed since, close this issue.\n"
    ));
    let text = pieces.concat();
    assert!(text.starts_with("# PR #"), "the finding starts with its title");
    text
}

/// The part of the finding about one failed job: its name, failed steps and first error lines.
fn job_section(failed: &FailedJob) -> String {
    let job = &failed.job;
    let steps: Vec<String> = job.failed_steps.iter().map(|step| format!("`{step}`")).collect();
    let steps =
        if steps.is_empty() { String::new() } else { format!(", step {}", steps.join(", ")) };
    let heading = format!("Job `{}` failed{steps} ([log]({})).\n\n", job.name, job.url);
    let details = match &failed.lines {
        Ok(lines) if lines.is_empty() => {
            "Its log has no line that reports an error; read the log.\n\n".to_owned()
        }
        Ok(lines) => {
            // Indented, the lines are a code block: GitHub makes no mention or link in it.
            let quoted: Vec<String> = lines.iter().map(|line| format!("    {line}\n")).collect();
            format!("First error lines:\n\n{}\n", quoted.concat())
        }
        Err(_) => "Its log could not be read; read it on GitHub.\n\n".to_owned(),
    };
    assert!(heading.contains(&job.name), "the section names its job");
    heading + &details
}

#[cfg(test)]
mod tests {
    use super::{FailedJob, Failure, error_lines, finding, pull_of_message, queue_entry, title};
    use crate::issue::Draft;
    use crate::queue::forge::Job;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn job(name: &str, steps: &[&str]) -> Job {
        Job {
            id: 7,
            name: name.to_owned(),
            conclusion: Some("failure".to_owned()),
            url: "https://github.com/o/r/actions/runs/1/job/7".to_owned(),
            failed_steps: steps.iter().map(|step| (*step).to_owned()).collect(),
        }
    }

    // Goal: a merge-queue branch names its base and PR; any other branch, or a malformed one,
    // names nothing.
    #[test]
    fn queue_branches_name_their_pull_request() {
        let branch = format!("gh-readonly-queue/main/pr-51-{SHA}");
        assert_eq!(queue_entry(&branch), Some(("main".to_owned(), 51)));
        let nested = format!("gh-readonly-queue/release/1.x/pr-7-{SHA}");
        assert_eq!(queue_entry(&nested), Some(("release/1.x".to_owned(), 7)));
        for bad in [
            format!("task/pr-51-{SHA}"),
            format!("gh-readonly-queue/pr-51-{SHA}"),
            format!("gh-readonly-queue//pr-51-{SHA}"),
            format!("gh-readonly-queue/main/51-{SHA}"),
            format!("gh-readonly-queue/main/pr-051-{SHA}"),
            format!("gh-readonly-queue/main/pr--{SHA}"),
            "gh-readonly-queue/main/pr-51-abc".to_owned(),
            format!("gh-readonly-queue/main/pr-51-{}", SHA.replace('a', "g")),
            "gh-readonly-queue/main/pr-51".to_owned(),
        ] {
            assert_eq!(queue_entry(&bad), None, "{bad}");
        }
    }

    // Goal: a squashed commit names its PR by the `(#n)` that ends its first line; a number in
    // the body or in the middle of the subject does not count.
    #[test]
    fn squashed_commits_name_their_pull_request() {
        assert_eq!(pull_of_message("Tidy rule (#34)"), Some(34));
        assert_eq!(pull_of_message("Fix (#12) and more (#34)  \n\nbody (#9)"), Some(34));
        assert_eq!(pull_of_message("No number\n\n(#9)"), None);
        assert_eq!(pull_of_message("Mentions (#12) early"), None);
        assert_eq!(pull_of_message("Zero (#0)"), None);
        assert_eq!(pull_of_message("Not a number (#x)"), None);
        assert_eq!(pull_of_message(""), None);
    }

    // Goal: error lines lose timestamps, colours and blanks, keep their order, are bounded in
    // number and length, and GitHub's annotations count only when nothing else does.
    #[test]
    fn error_lines_are_the_first_errors_of_our_tools() {
        let log = "2026-11-26T10:00:00.1234567Z    Compiling xtask v0.1.0\n\
                   2026-11-26T10:00:01.0000000Z \u{1b}[1m\u{1b}[31merror[adr-number]\u{1b}[0m: \
                   docs/design/0022-x.md: ADR number 0022 is also used\n\
                   2026-11-26T10:00:02.0000000Z         FAIL [   0.01s] xtask tests::one\n\
                   thread 'one' panicked at src/lib.rs:1:1:\n\
                   ##[error]Process completed with exit code 1.\n\
                   error: test failed\r\n";
        assert_eq!(
            error_lines(log),
            [
                "error[adr-number]: docs/design/0022-x.md: ADR number 0022 is also used",
                "FAIL [   0.01s] xtask tests::one",
                "thread 'one' panicked at src/lib.rs:1:1:",
                "error: test failed",
            ]
        );
        let annotations = "build ok\n##[error]Process completed with exit code 101.\n";
        assert_eq!(error_lines(annotations), ["##[error]Process completed with exit code 101."]);
        assert_eq!(error_lines("all good\n"), Vec::<String>::new());
        let many = "error: x\n".repeat(20);
        assert_eq!(error_lines(&many).len(), super::ERROR_LINES_MAX);
        let long = format!("error: {}", "é".repeat(300));
        let line = &error_lines(&long)[0];
        assert_eq!(line.chars().count(), super::LINE_CHARS_MAX + 1, "cut, with an ellipsis");
        assert!(line.ends_with('…'));
        let short = "2026-11-26 error: a date without a time\n";
        assert_eq!(error_lines(short), Vec::<String>::new(), "not a timestamp, not an error");
    }

    // Goal: a timestamp needs all four marks: length, a leading digit, `T` after the date and a
    // final `Z`; a word that misses any one of them is text.
    #[test]
    fn timestamps_need_every_mark() {
        assert!(super::is_timestamp("2026-11-26T10:00:00Z"), "the shortest form");
        assert!(super::is_timestamp("2026-11-26T10:00:00.1234567Z"));
        for word in [
            "2026-11-26T10:00Z",
            "x026-11-26T10:00:00.0Z",
            "2026-11-26 10:00:00.00Z",
            "2026-11-26T10:00:00.00",
            "xxxxxxxxxxxxxxxxxxxxxxxx",
        ] {
            assert!(!super::is_timestamp(word), "{word}");
        }
    }

    // Goal: the finding parses as `issue file` input, names the PR, the PRs ahead, each failed
    // job with its steps and error lines, and says to rebase or renumber, then re-queue.
    #[test]
    fn findings_name_the_pull_request_and_the_failed_jobs() {
        let failure = Failure {
            pull: 51,
            ahead: [45, 47].into(),
            run_url: "https://github.com/o/r/actions/runs/1".to_owned(),
            jobs: vec![
                FailedJob {
                    job: job("gate", &["just check-full"]),
                    lines: Ok(vec!["error[adr-number]: clash".to_owned()]),
                },
                FailedJob { job: job("lints (UI tests)", &[]), lines: Ok(Vec::new()) },
                FailedJob { job: job("other", &["a", "b"]), lines: Err("gone".to_owned()) },
            ],
        };
        let text = finding(&failure);
        let draft = Draft::parse(&text).expect("a finding that `issue file` accepts");
        assert_eq!(draft.title, title(51));
        assert_eq!(draft.title, "PR #51 failed in the merge queue");
        for part in [
            "- Found in: merge-queue run https://github.com/o/r/actions/runs/1\n",
            "GitHub removed #51 from the queue. The tested tree also held #45, #47, ahead",
            "Job `gate` failed, step `just check-full` ([log](https://github.com/o/r/actions/",
            "First error lines:\n\n    error[adr-number]: clash\n\n",
            "Job `lints (UI tests)` failed ([log]",
            "Its log has no line that reports an error",
            "Job `other` failed, step `a`, `b` ([log]",
            "Its log could not be read",
            "Rebase or renumber, then re-queue.",
            "`Fixes #<this issue>`",
        ] {
            assert!(text.contains(part), "{part}\n---\n{text}");
        }
        assert!(!text.contains("gone"), "a gh error is not quoted");
        let alone = Failure { ahead: [].into(), jobs: Vec::new(), ..failure };
        let lone = finding(&alone);
        assert!(!lone.contains("also held"), "{lone}");
        assert!(lone.contains("No job of the run reports a failure"), "{lone}");
        assert!(Draft::parse(&lone).is_ok(), "{lone}");
    }
}
