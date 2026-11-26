//! GitHub issues through the `gh` command line (ADR-0021).
//!
//! [`GitHub`] is what the commands use. [`Gh`] implements it by running `gh api` in the
//! checkout, so it acts as whoever `GH_TOKEN` names, and `{owner}/{repo}` is the checkout's
//! GitHub repository. Tests implement the trait with fixed data. Parsing is kept apart from the
//! process calls, so that it is tested on JSON in the shape that GitHub returns.

use crate::json::{self, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Most entries one listing may return. A longer list is an error, not truncated.
pub const LIST_MAX: usize = 2_000;

/// One issue or pull request: GitHub's issues API returns both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Its number in the repository.
    pub number: u32,
    /// Its title.
    pub title: String,
    /// The login of whoever opened it.
    pub author: String,
    /// True while it is open.
    pub is_open: bool,
    /// True for a pull request.
    pub is_pull_request: bool,
    /// The names of its labels.
    pub labels: Vec<String>,
    /// Its description; empty if it has none.
    pub body: String,
}

/// One comment on an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// The login of whoever wrote it.
    pub author: String,
    /// Its text.
    pub body: String,
}

/// A label that someone added to an issue or removed from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelEvent {
    /// The label's name.
    pub label: String,
    /// The login of whoever added or removed it.
    pub actor: String,
    /// True if it was added, false if it was removed.
    pub added: bool,
}

/// What the issue commands need from GitHub. Every error says what failed and how to fix it.
pub trait GitHub {
    /// The issue or pull request with this number.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or does not have it.
    fn issue(&self, number: u32) -> Result<Issue, String>;

    /// Every open issue and pull request, oldest first.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or there are more than [`LIST_MAX`].
    fn open_issues(&self) -> Result<Vec<Issue>, String>;

    /// The comments on an issue, oldest first.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or there are more than [`LIST_MAX`].
    fn comments(&self, number: u32) -> Result<Vec<Comment>, String>;

    /// The labels added to and removed from an issue, oldest first.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or there are more than [`LIST_MAX`] events.
    fn label_events(&self, number: u32) -> Result<Vec<LabelEvent>, String>;

    /// Opens an issue and returns its number.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it or cannot be asked.
    fn create(&self, title: &str, body: &str, labels: &[&str]) -> Result<u32, String>;
}

/// Reads an issue or pull request in the shape of GitHub's issues API.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn issue_from_json(value: &Value) -> Result<Issue, String> {
    let number = value.get("number").and_then(as_number).ok_or("an issue has no `number`")?;
    let state = string_at(value, &["state"])?;
    let is_open = match state.as_str() {
        "open" => true,
        "closed" => false,
        other => return Err(format!("issue #{number} has the unknown state `{other}`")),
    };
    let labels = value.get("labels").and_then(Value::as_array);
    let labels = labels.ok_or_else(|| format!("issue #{number} has no `labels` array"))?;
    let labels: Option<Vec<String>> = labels
        .iter()
        .map(|label| label.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let issue = Issue {
        number,
        title: string_at(value, &["title"])?,
        author: string_at(value, &["user", "login"])?,
        is_open,
        is_pull_request: value.get("pull_request").is_some_and(|field| *field != Value::Null),
        labels: labels.ok_or_else(|| format!("issue #{number} has a label without a name"))?,
        body: optional_string(value, "body")?,
    };
    assert!(issue.number > 0, "GitHub numbers issues from 1");
    Ok(issue)
}

/// Reads a comment in the shape of GitHub's issue-comments API.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn comment_from_json(value: &Value) -> Result<Comment, String> {
    Ok(Comment {
        author: string_at(value, &["user", "login"])?,
        body: optional_string(value, "body")?,
    })
}

/// Reads one entry of GitHub's issue-events API: a label event, or `None` for any other event.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn label_event_from_json(value: &Value) -> Result<Option<LabelEvent>, String> {
    let added = match string_at(value, &["event"])?.as_str() {
        "labeled" => true,
        "unlabeled" => false,
        _ => return Ok(None),
    };
    // GitHub reports a deleted account as a null actor. It is nobody's decision.
    let actor = match value.get("actor") {
        Some(Value::Null) | None => "ghost".to_owned(),
        Some(_) => string_at(value, &["actor", "login"])?,
    };
    Ok(Some(LabelEvent { label: string_at(value, &["label", "name"])?, actor, added }))
}

/// Parses `gh api --jq '.[]'` output: one JSON value per line.
///
/// # Errors
///
/// A line that is not JSON, or more than [`LIST_MAX`] lines.
pub fn parse_lines(text: &str) -> Result<Vec<Value>, String> {
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.len() > LIST_MAX {
        return Err(format!("GitHub returned {} entries, more than {LIST_MAX}", lines.len()));
    }
    let values: Result<Vec<Value>, String> = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            json::parse(line).map_err(|error| {
                format!("gh printed invalid JSON on line {}: {}", index + 1, error.message)
            })
        })
        .collect();
    let values = values?;
    assert_eq!(values.len(), lines.len(), "one value per line");
    Ok(values)
}

/// The text at `path` below `value`: `["user", "login"]` is `value.user.login`.
pub(crate) fn string_at(value: &Value, path: &[&str]) -> Result<String, String> {
    let found = path.iter().try_fold(value, |inner, key| inner.get(key));
    let text = found.and_then(Value::as_str);
    text.map(str::to_owned).ok_or_else(|| format!("GitHub sent no string at `{}`", path.join(".")))
}

/// A string field that GitHub may send as `null`: an issue or comment without text.
pub(crate) fn optional_string(value: &Value, key: &str) -> Result<String, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(other) => Err(format!("GitHub sent {} as `{key}`", other.kind())),
    }
}

/// A JSON number that is a valid issue number.
fn as_number(value: &Value) -> Option<u32> {
    let Value::Number(digits) = value else {
        return None;
    };
    digits.parse().ok().filter(|number| *number > 0)
}

/// The GitHub repository, `owner/name`, that `remote` of the checkout at `root` points to, or
/// `None` if git cannot tell or the remote is not on GitHub.
#[must_use]
pub fn repository_of(root: &Path, remote: &str) -> Option<String> {
    let url = crate::task::claims::git(root, &["remote", "get-url", remote], &[], None).ok()?;
    repository_of_url(url.trim())
}

/// The `owner/name` of a GitHub remote URL: `https://github.com/owner/name(.git)`,
/// `git@github.com:owner/name(.git)` or `ssh://git@github.com/owner/name(.git)`.
#[must_use]
pub fn repository_of_url(url: &str) -> Option<String> {
    let path = ["https://github.com/", "git@github.com:", "ssh://git@github.com/"]
        .iter()
        .find_map(|prefix| url.strip_prefix(prefix))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let is_part = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    let (owner, name) = path.split_once('/')?;
    (is_part(owner) && is_part(name)).then(|| path.to_owned())
}

/// [`GitHub`] through `gh api`, run in a checkout of the repository.
#[derive(Debug, Clone)]
pub struct Gh {
    root: PathBuf,
}

impl Gh {
    /// Runs `gh` in the checkout at `root`, which decides the repository.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }

    /// Runs `gh api` with `arguments`, and `input` on stdin; returns what gh printed.
    pub(crate) fn api(&self, arguments: &[&str], input: Option<&str>) -> Result<String, String> {
        let mut command = Command::new("gh");
        let _ = command.arg("api").args(arguments).current_dir(&self.root);
        let _ = command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
        let _ = command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot run gh: {error}; install the GitHub CLI"))?;
        if let Some(text) = input {
            let mut pipe = child.stdin.take().expect("stdin is piped");
            std::io::Write::write_all(&mut pipe, text.as_bytes())
                .map_err(|error| format!("cannot write to gh: {error}"))?;
        }
        let output = child.wait_with_output().map_err(|error| format!("gh failed: {error}"))?;
        if output.status.success() {
            return String::from_utf8(output.stdout)
                .map_err(|_| "gh printed invalid UTF-8".to_owned());
        }
        Err(format!(
            "`gh api {}` failed: {}; check that GitHub is reachable and that `GH_TOKEN` is set \
             (`GH_TOKEN=$(xtask/agent-token.sh)`)",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }

    /// Every entry of a paginated listing at `path`.
    pub(crate) fn list(&self, path: &str) -> Result<Vec<Value>, String> {
        parse_lines(&self.api(&["--paginate", path, "--jq", ".[]"], None)?)
    }

    /// One JSON value from `gh api`.
    pub(crate) fn one(&self, arguments: &[&str], input: Option<&str>) -> Result<Value, String> {
        let text = self.api(arguments, input)?;
        json::parse(&text).map_err(|error| format!("gh printed invalid JSON: {}", error.message))
    }
}

impl GitHub for Gh {
    fn issue(&self, number: u32) -> Result<Issue, String> {
        issue_from_json(&self.one(&[&format!("repos/{{owner}}/{{repo}}/issues/{number}")], None)?)
    }

    fn open_issues(&self) -> Result<Vec<Issue>, String> {
        let path = "repos/{owner}/{repo}/issues?state=open&sort=created&direction=asc&per_page=100";
        self.list(path)?.iter().map(issue_from_json).collect()
    }

    fn comments(&self, number: u32) -> Result<Vec<Comment>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/issues/{number}/comments?per_page=100");
        self.list(&path)?.iter().map(comment_from_json).collect()
    }

    fn label_events(&self, number: u32) -> Result<Vec<LabelEvent>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/issues/{number}/events?per_page=100");
        let events: Result<Vec<Option<LabelEvent>>, String> =
            self.list(&path)?.iter().map(label_event_from_json).collect();
        Ok(events?.into_iter().flatten().collect())
    }

    fn create(&self, title: &str, body: &str, labels: &[&str]) -> Result<u32, String> {
        let labels: Vec<String> = labels.iter().map(|label| json::quote(label)).collect();
        let request = format!(
            "{{\"title\": {}, \"body\": {}, \"labels\": [{}]}}",
            json::quote(title),
            json::quote(body),
            labels.join(", ")
        );
        let arguments = ["--method", "POST", "repos/{owner}/{repo}/issues", "--input", "-"];
        Ok(issue_from_json(&self.one(&arguments, Some(&request))?)?.number)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Comment, Issue, LabelEvent, comment_from_json, issue_from_json, label_event_from_json,
        parse_lines,
    };
    use crate::json::parse;

    /// An issue as GitHub's API returns it, with the fields that are not read left out.
    const ISSUE: &str = r#"{"number": 12, "title": "Loop rule misses cycle()",
        "user": {"login": "nami-agent[bot]", "type": "Bot"}, "state": "open",
        "labels": [{"name": "guardrail-gap", "color": "d93f0b"}], "body": "Text."}"#;

    // Goal: every field of an issue is read, a pull request is told apart, and a missing body
    // is empty text.
    #[test]
    fn issues_are_read() {
        let issue = issue_from_json(&parse(ISSUE).unwrap()).expect("a complete issue");
        let expected = Issue {
            number: 12,
            title: "Loop rule misses cycle()".to_owned(),
            author: "nami-agent[bot]".to_owned(),
            is_open: true,
            is_pull_request: false,
            labels: vec!["guardrail-gap".to_owned()],
            body: "Text.".to_owned(),
        };
        assert_eq!(issue, expected);
        let pull = ISSUE.replace("\"body\": \"Text.\"", "\"body\": null, \"pull_request\": {}");
        let pull = issue_from_json(&parse(&pull).unwrap()).expect("a pull request");
        assert!(pull.is_pull_request && pull.body.is_empty(), "{pull:?}");
        let not_pull = ISSUE.replace("\"state\"", "\"pull_request\": null, \"state\"");
        assert!(!issue_from_json(&parse(&not_pull).unwrap()).unwrap().is_pull_request);
        let closed = ISSUE.replace("\"open\"", "\"closed\"");
        assert!(!issue_from_json(&parse(&closed).unwrap()).unwrap().is_open, "closed");
    }

    // Goal: an issue with a missing or mistyped field is an error that names the field.
    #[test]
    fn broken_issues_are_errors() {
        let cases = [
            (ISSUE.replace("\"number\": 12", "\"number\": 0"), "no `number`"),
            (ISSUE.replace("\"number\": 12", "\"number\": \"12\""), "no `number`"),
            (ISSUE.replace("\"open\"", "\"merged\""), "unknown state `merged`"),
            (ISSUE.replace("\"title\"", "\"name\""), "`title`"),
            (ISSUE.replace("\"login\"", "\"name\""), "`user.login`"),
            (ISSUE.replace("\"labels\"", "\"tags\""), "no `labels` array"),
            (ISSUE.replace("{\"name\": \"guardrail-gap\",", "{"), "a label without a name"),
            (ISSUE.replace("\"Text.\"", "7"), "a number as `body`"),
        ];
        for (text, expected) in cases {
            let error = issue_from_json(&parse(&text).unwrap()).expect_err("broken issue");
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }

    // Goal: comments and label events are read; other events are skipped, and a deleted
    // account is the actor `ghost`.
    #[test]
    fn comments_and_label_events_are_read() {
        let comment = parse(r#"{"user": {"login": "someone"}, "body": "Hi"}"#).unwrap();
        let expected = Comment { author: "someone".to_owned(), body: "Hi".to_owned() };
        assert_eq!(comment_from_json(&comment), Ok(expected));
        assert!(comment_from_json(&parse(r#"{"body": "Hi"}"#).unwrap()).is_err(), "no author");

        let event = |kind: &str, actor: &str| {
            let text =
                format!(r#"{{"event": "{kind}", "actor": {actor}, "label": {{"name": "x"}}}}"#);
            label_event_from_json(&parse(&text).unwrap())
        };
        let label = |actor: &str, added| LabelEvent {
            label: "x".to_owned(),
            actor: actor.to_owned(),
            added,
        };
        assert_eq!(event("labeled", r#"{"login": "a"}"#), Ok(Some(label("a", true))));
        assert_eq!(event("unlabeled", r#"{"login": "a"}"#), Ok(Some(label("a", false))));
        assert_eq!(event("unlabeled", "null"), Ok(Some(label("ghost", false))));
        assert_eq!(event("closed", r#"{"login": "a"}"#), Ok(None));
        assert!(event("labeled", "{}").is_err(), "an actor without a login");
    }

    // Goal: a GitHub remote URL in each of git's forms names its `owner/name`; another host or
    // a malformed path names nothing. This checkout's remote is on GitHub.
    #[test]
    fn remote_urls_name_their_repository() {
        for url in [
            "https://github.com/namicfd/nami",
            "https://github.com/namicfd/nami/",
            "https://github.com/namicfd/nami.git",
            "git@github.com:namicfd/nami.git",
            "ssh://git@github.com/namicfd/nami",
        ] {
            assert_eq!(super::repository_of_url(url).as_deref(), Some("namicfd/nami"), "{url}");
        }
        for url in [
            "https://gitlab.com/namicfd/nami",
            "https://github.com/namicfd",
            "https://github.com/namicfd/nami/tree",
            "https://github.com//nami",
            "https://github.com/a b/nami",
            "/tmp/remote.git",
        ] {
            assert_eq!(super::repository_of_url(url), None, "{url}");
        }
        let scratch = crate::tidy::scratch::Scratch::new("repository-of");
        let git = |arguments: &[&str]| {
            crate::task::claims::git(scratch.root(), arguments, &[], None).expect("git runs")
        };
        let _ = git(&["init", "--quiet"]);
        let _ = git(&["remote", "add", "origin", "git@github.com:namicfd/nami.git"]);
        let _ = git(&["remote", "add", "elsewhere", "https://gitlab.com/namicfd/nami"]);
        assert_eq!(super::repository_of(scratch.root(), "origin").as_deref(), Some("namicfd/nami"));
        assert_eq!(super::repository_of(scratch.root(), "elsewhere"), None, "not on GitHub");
        assert_eq!(super::repository_of(scratch.root(), "no-such-remote"), None);
    }

    // Goal: `gh --jq '.[]'` output becomes one value per line; blank lines are skipped, and a
    // broken line names its number.
    #[test]
    fn lines_are_parsed() {
        let values = parse_lines("{\"a\": 1}\n\n[2]\n").expect("two JSON lines");
        assert_eq!(values.len(), 2);
        assert_eq!(parse_lines(""), Ok(Vec::new()));
        let error = parse_lines("{}\n{\n").expect_err("a broken line");
        assert!(error.contains("line 2"), "{error}");
        let many = "{}\n".repeat(super::LIST_MAX + 1);
        assert!(parse_lines(&many).expect_err("too many").contains("more than"), "bound");
        assert_eq!(
            parse_lines(&"{}\n".repeat(super::LIST_MAX)).map(|v| v.len()),
            Ok(super::LIST_MAX)
        );
    }
}
