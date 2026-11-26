//! What `cargo xtask queue` needs from GitHub beyond issues: pull requests, their files and
//! comments, and the runs and jobs of a workflow.
//!
//! [`Forge`] is what the commands use; [`Gh`] implements it with `gh api`, as whoever
//! `GH_TOKEN` names. Parsing is kept apart from the process calls, so that it is tested on JSON
//! in the shape that GitHub returns.

use crate::issue::github::{Gh, optional_string, parse_lines, string_at};
use crate::json::{self, Value};

/// An open pull request: only its number and author are read, never its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pull {
    /// Its number.
    pub number: u32,
    /// The login of whoever opened it.
    pub author: String,
}

/// A file that a pull request changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullFile {
    /// Its path after the change, relative to the repository root.
    pub path: String,
    /// GitHub's status: `added`, `removed`, `modified`, `renamed`, `copied`, ...
    pub status: String,
    /// The path before a rename or copy.
    pub previous: Option<String>,
}

/// A comment on a pull request, with the ID that edits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullComment {
    /// The comment's ID.
    pub id: u64,
    /// The login of whoever wrote it.
    pub author: String,
    /// Its text.
    pub body: String,
}

/// A workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The event that started it, such as `merge_group`.
    pub event: String,
    /// `success`, `failure`, ...; `None` while it runs.
    pub conclusion: Option<String>,
    /// The branch it ran on; for the merge queue, `gh-readonly-queue/<base>/pr-<n>-<sha>`.
    pub head_branch: String,
    /// The commit it tested.
    pub head_sha: String,
    /// Its page on GitHub.
    pub url: String,
}

/// A job of a workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The job's ID, which names its log.
    pub id: u64,
    /// The job's name, as the workflow gives it.
    pub name: String,
    /// `success`, `failure`, `cancelled`, ...; `None` while it runs.
    pub conclusion: Option<String>,
    /// Its page on GitHub.
    pub url: String,
    /// The names of its steps that failed, in order.
    pub failed_steps: Vec<String>,
}

/// What the queue commands need from GitHub. Every error says what failed.
pub trait Forge {
    /// The file names (not paths) in `docs/design/` on `branch`.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked.
    fn adr_names_on(&self, branch: &str) -> Result<Vec<String>, String>;

    /// Every open pull request.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or there are too many.
    fn open_pulls(&self) -> Result<Vec<Pull>, String>;

    /// The files that pull request `number` changes.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or there are too many.
    fn pull_files(&self, number: u32) -> Result<Vec<PullFile>, String>;

    /// The comments on pull request `number`, oldest first.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or there are too many.
    fn pull_comments(&self, number: u32) -> Result<Vec<PullComment>, String>;

    /// Posts a comment on pull request `number`.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it or cannot be asked.
    fn post_comment(&self, number: u32, body: &str) -> Result<(), String>;

    /// Replaces the text of comment `id`.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it or cannot be asked.
    fn edit_comment(&self, id: u64, body: &str) -> Result<(), String>;

    /// The workflow run `id`.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or does not have it.
    fn run(&self, id: u64) -> Result<Run, String>;

    /// The jobs of workflow run `id`.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked.
    fn jobs(&self, id: u64) -> Result<Vec<Job>, String>;

    /// The log of job `id`, as plain text.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or the log has expired.
    fn job_log(&self, id: u64) -> Result<String, String>;

    /// The commit messages on `head` that `base` does not have, oldest first.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked.
    fn commit_messages(&self, base: &str, head: &str) -> Result<Vec<String>, String>;
}

/// Reads a pull request from GitHub's pulls API.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn pull_from_json(value: &Value) -> Result<Pull, String> {
    let number = value.get("number").and_then(as_id).and_then(|id| u32::try_from(id).ok());
    let number = number.ok_or("a pull request has no `number`")?;
    Ok(Pull { number, author: string_at(value, &["user", "login"])? })
}

/// Reads one entry of GitHub's pull-request files API.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn pull_file_from_json(value: &Value) -> Result<PullFile, String> {
    let previous = optional_string(value, "previous_filename")?;
    Ok(PullFile {
        path: string_at(value, &["filename"])?,
        status: string_at(value, &["status"])?,
        previous: (!previous.is_empty()).then_some(previous),
    })
}

/// Reads a comment from GitHub's issue-comments API, with its ID.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn pull_comment_from_json(value: &Value) -> Result<PullComment, String> {
    Ok(PullComment {
        id: value.get("id").and_then(as_id).ok_or("a comment has no `id`")?,
        author: string_at(value, &["user", "login"])?,
        body: optional_string(value, "body")?,
    })
}

/// Reads a workflow run from GitHub's actions API.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn run_from_json(value: &Value) -> Result<Run, String> {
    let conclusion = optional_string(value, "conclusion")?;
    Ok(Run {
        event: string_at(value, &["event"])?,
        conclusion: (!conclusion.is_empty()).then_some(conclusion),
        head_branch: string_at(value, &["head_branch"])?,
        head_sha: string_at(value, &["head_sha"])?,
        url: string_at(value, &["html_url"])?,
    })
}

/// Reads a job from GitHub's actions API.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn job_from_json(value: &Value) -> Result<Job, String> {
    let conclusion = optional_string(value, "conclusion")?;
    let steps = value.get("steps").and_then(Value::as_array).unwrap_or_default();
    let mut failed_steps = Vec::new();
    for step in steps {
        if optional_string(step, "conclusion")? == "failure" {
            failed_steps.push(string_at(step, &["name"])?);
        }
    }
    assert!(failed_steps.len() <= steps.len(), "failed steps are steps");
    Ok(Job {
        id: value.get("id").and_then(as_id).ok_or("a job has no `id`")?,
        name: string_at(value, &["name"])?,
        conclusion: (!conclusion.is_empty()).then_some(conclusion),
        url: string_at(value, &["html_url"])?,
        failed_steps,
    })
}

/// Reads the `jobs` array of GitHub's run-jobs API.
///
/// # Errors
///
/// When there is no `jobs` array, or a job is broken.
pub fn jobs_from_json(value: &Value) -> Result<Vec<Job>, String> {
    let jobs = value.get("jobs").and_then(Value::as_array).ok_or("GitHub sent no `jobs` array")?;
    jobs.iter().map(job_from_json).collect()
}

/// Reads a JSON array of strings, such as the commit messages that `commit_messages` asks for.
///
/// # Errors
///
/// When the value is not an array of strings.
pub fn strings_from_json(value: &Value) -> Result<Vec<String>, String> {
    let items =
        value.as_array().ok_or_else(|| format!("expected an array, not {}", value.kind()))?;
    let strings: Option<Vec<String>> =
        items.iter().map(|item| item.as_str().map(str::to_owned)).collect();
    strings.ok_or_else(|| "expected an array of strings".to_owned())
}

/// A JSON number that is a positive ID.
fn as_id(value: &Value) -> Option<u64> {
    let Value::Number(digits) = value else {
        return None;
    };
    digits.parse().ok().filter(|id| *id > 0)
}

/// The text of a request body `{"body": <text>}`.
fn body_request(body: &str) -> String {
    format!("{{\"body\": {}}}", json::quote(body))
}

impl Forge for Gh {
    fn adr_names_on(&self, branch: &str) -> Result<Vec<String>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/contents/docs/design?ref={branch}");
        let entries = parse_lines(&self.api(&[&path, "--jq", ".[]"], None)?)?;
        entries.iter().map(|entry| string_at(entry, &["name"])).collect()
    }

    fn open_pulls(&self) -> Result<Vec<Pull>, String> {
        let path = "repos/{owner}/{repo}/pulls?state=open&per_page=100";
        self.list(path)?.iter().map(pull_from_json).collect()
    }

    fn pull_files(&self, number: u32) -> Result<Vec<PullFile>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}/files?per_page=100");
        self.list(&path)?.iter().map(pull_file_from_json).collect()
    }

    fn pull_comments(&self, number: u32) -> Result<Vec<PullComment>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/issues/{number}/comments?per_page=100");
        self.list(&path)?.iter().map(pull_comment_from_json).collect()
    }

    fn post_comment(&self, number: u32, body: &str) -> Result<(), String> {
        let path = format!("repos/{{owner}}/{{repo}}/issues/{number}/comments");
        let _ =
            self.one(&["--method", "POST", &path, "--input", "-"], Some(&body_request(body)))?;
        Ok(())
    }

    fn edit_comment(&self, id: u64, body: &str) -> Result<(), String> {
        let path = format!("repos/{{owner}}/{{repo}}/issues/comments/{id}");
        let _ =
            self.one(&["--method", "PATCH", &path, "--input", "-"], Some(&body_request(body)))?;
        Ok(())
    }

    fn run(&self, id: u64) -> Result<Run, String> {
        run_from_json(&self.one(&[&format!("repos/{{owner}}/{{repo}}/actions/runs/{id}")], None)?)
    }

    fn jobs(&self, id: u64) -> Result<Vec<Job>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/actions/runs/{id}/jobs?per_page=100");
        jobs_from_json(&self.one(&[&path], None)?)
    }

    fn job_log(&self, id: u64) -> Result<String, String> {
        self.api(&[&format!("repos/{{owner}}/{{repo}}/actions/jobs/{id}/logs")], None)
    }

    fn commit_messages(&self, base: &str, head: &str) -> Result<Vec<String>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/compare/{base}...{head}?per_page=100");
        strings_from_json(&self.one(&[&path, "--jq", "[.commits[].commit.message]"], None)?)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Job, PullFile, job_from_json, jobs_from_json, pull_comment_from_json, pull_file_from_json,
        pull_from_json, run_from_json, strings_from_json,
    };
    use crate::json::parse;

    /// A job as GitHub's run-jobs API returns it, trimmed to the fields that matter.
    const JOB: &str = r#"{"id": 5123456789, "name": "gate", "conclusion": "failure",
        "html_url": "https://github.com/o/r/actions/runs/1/job/5123456789",
        "steps": [{"name": "Set up job", "conclusion": "success"},
                  {"name": "just check-full", "conclusion": "failure"},
                  {"name": "Post", "conclusion": null}]}"#;

    // Goal: pull requests, their files and their comments are read with the fields that the
    // clash check needs; a missing previous name is `None`, and broken entries are errors.
    #[test]
    fn pulls_files_and_comments_are_read() {
        let pull = parse(r#"{"number": 45, "user": {"login": "nami-agent[bot]"}, "title": "x"}"#);
        let pull = pull_from_json(&pull.unwrap()).expect("a pull request");
        assert_eq!((pull.number, pull.author.as_str()), (45, "nami-agent[bot]"));
        assert!(
            pull_from_json(&parse(r#"{"number": 0, "user": {"login": "a"}}"#).unwrap())
                .expect_err("number 0")
                .contains("`number`")
        );
        assert!(
            pull_from_json(&parse(r#"{"number": 4294967296, "user": {"login": "a"}}"#).unwrap())
                .is_err()
        );

        let renamed = r#"{"filename": "docs/design/0023-x.md", "status": "renamed",
            "previous_filename": "docs/design/0022-x.md"}"#;
        let renamed = pull_file_from_json(&parse(renamed).unwrap()).expect("a renamed file");
        assert_eq!(
            renamed,
            PullFile {
                path: "docs/design/0023-x.md".to_owned(),
                status: "renamed".to_owned(),
                previous: Some("docs/design/0022-x.md".to_owned()),
            }
        );
        let added = parse(r#"{"filename": "a.md", "status": "added"}"#).unwrap();
        assert_eq!(pull_file_from_json(&added).unwrap().previous, None);
        assert!(pull_file_from_json(&parse(r#"{"status": "added"}"#).unwrap()).is_err());

        let comment =
            r#"{"id": 9876543210, "user": {"login": "github-actions[bot]"}, "body": "b"}"#;
        let comment = pull_comment_from_json(&parse(comment).unwrap()).expect("a comment");
        assert_eq!((comment.id, comment.author.as_str()), (9_876_543_210, "github-actions[bot]"));
        let no_id = parse(r#"{"user": {"login": "a"}, "body": "b"}"#).unwrap();
        assert!(pull_comment_from_json(&no_id).expect_err("no id").contains("`id`"));
    }

    // Goal: a run is read with its event, conclusion, branch and commit; a run still in progress
    // has no conclusion.
    #[test]
    fn runs_are_read() {
        let text = r#"{"event": "merge_group", "conclusion": "failure",
            "head_branch": "gh-readonly-queue/main/pr-51-abc", "head_sha": "abc",
            "html_url": "https://github.com/o/r/actions/runs/7"}"#;
        let run = run_from_json(&parse(text).unwrap()).expect("a run");
        assert_eq!(run.event, "merge_group");
        assert_eq!(run.conclusion.as_deref(), Some("failure"));
        assert_eq!(
            (run.head_branch.as_str(), run.head_sha.as_str()),
            ("gh-readonly-queue/main/pr-51-abc", "abc")
        );
        assert_eq!(run.url, "https://github.com/o/r/actions/runs/7");
        let running = text.replace("\"failure\"", "null");
        assert_eq!(run_from_json(&parse(&running).unwrap()).unwrap().conclusion, None);
        let broken = text.replace("\"head_sha\"", "\"sha\"");
        assert!(run_from_json(&parse(&broken).unwrap()).expect_err("no sha").contains("head_sha"));
    }

    // Goal: a job is read with the names of its failed steps only; a job list without `jobs` is
    // an error.
    #[test]
    fn jobs_are_read() {
        let job = job_from_json(&parse(JOB).unwrap()).expect("a job");
        assert_eq!(
            job,
            Job {
                id: 5_123_456_789,
                name: "gate".to_owned(),
                conclusion: Some("failure".to_owned()),
                url: "https://github.com/o/r/actions/runs/1/job/5123456789".to_owned(),
                failed_steps: vec!["just check-full".to_owned()],
            }
        );
        let list = parse(&format!("{{\"total_count\": 1, \"jobs\": [{JOB}]}}")).unwrap();
        assert_eq!(jobs_from_json(&list).expect("one job").len(), 1);
        assert!(jobs_from_json(&parse("{}").unwrap()).expect_err("no jobs").contains("`jobs`"));
        let unnamed = JOB.replace("\"name\": \"just check-full\", ", "");
        assert!(job_from_json(&parse(&unnamed).unwrap()).is_err(), "a failed step without a name");
        let no_steps = parse(r#"{"id": 1, "name": "g", "conclusion": null, "html_url": "u"}"#);
        assert_eq!(job_from_json(&no_steps.unwrap()).unwrap().failed_steps, Vec::<String>::new());
    }

    // Goal: a comment's request body is a JSON object whose `body` is the text, quotes and line
    // breaks included.
    #[test]
    fn comment_requests_carry_the_text() {
        let request = parse(&super::body_request("say \"hi\"\nthen go")).expect("valid JSON");
        let body = request.get("body").and_then(crate::json::Value::as_str);
        assert_eq!(body, Some("say \"hi\"\nthen go"));
        assert_eq!(request.as_object().map(<[_]>::len), Some(1), "only the body");
    }

    // Goal: commit messages arrive as a JSON array of strings; anything else is an error.
    #[test]
    fn strings_are_read() {
        let messages = strings_from_json(&parse(r#"["a (#1)\n\nbody", "b (#2)"]"#).unwrap());
        assert_eq!(messages, Ok(vec!["a (#1)\n\nbody".to_owned(), "b (#2)".to_owned()]));
        assert!(
            strings_from_json(&parse("{}").unwrap()).expect_err("object").contains("an object")
        );
        assert!(strings_from_json(&parse("[1]").unwrap()).is_err(), "not a string");
    }
}
