//! What `cargo xtask review` needs from GitHub: a pull request's facts, its changed files with
//! their patches, and the reviewer app's open threads; and, as the reviewer app, resolving a
//! thread and posting a review.
//!
//! [`Forge`] reads and [`Poster`] writes; [`Gh`] implements both with `gh api`, as whoever its
//! token names. The commands read through one client and write through another, so that no
//! read acts as the reviewer app and no write as the workflow's token (ADR-0023, rule 2).
//! Parsing is kept apart from the process calls, so that it is tested on JSON in the shape
//! that GitHub returns.

use crate::issue::github::{Gh, optional_string, string_at};
use crate::json::{self, Value};

/// What the review commands read about a pull request. Its title and body are the author's
/// text: the review reads them as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullFacts {
    /// The login of whoever opened it.
    pub author: String,
    /// True when the author is a GitHub App (`user.type` `Bot`).
    pub author_is_app: bool,
    /// True while it is open.
    pub is_open: bool,
    /// True while it is a draft.
    pub is_draft: bool,
    /// The commit its branch points to.
    pub head_sha: String,
    /// Its branch's name.
    pub head_ref: String,
    /// The `owner/name` of its branch's repository; `None` when that repository is gone.
    pub head_repository: Option<String>,
    /// The `owner/name` of the repository it merges into.
    pub base_repository: String,
    /// The branch it merges into.
    pub base_ref: String,
    /// The commit of the base branch that GitHub compares it with.
    pub base_sha: String,
    /// Its title.
    pub title: String,
    /// Its description.
    pub body: String,
}

/// A file that the pull request changes, with GitHub's patch of it, if GitHub sent one: it
/// sends none for a binary file or a very large diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Its path in the head.
    pub path: String,
    /// The unified diff of its hunks, without file headers.
    pub patch: Option<String>,
}

/// A review thread that the reviewer app opened and has not resolved itself. A thread that
/// someone else resolved is still open: only the reviewer resolves its threads (ADR-0022), so
/// the author cannot clear a finding by resolving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenThread {
    /// Its GraphQL node ID, which resolves it.
    pub id: String,
    /// The file it is on.
    pub path: String,
    /// The line it is on in the current diff, if GitHub still places it.
    pub line: Option<u32>,
    /// Its first comment's text: the finding.
    pub body: String,
    /// The later comments, with their authors' logins: the author's answers, for example why
    /// the finding is wrong.
    pub replies: Vec<(String, String)>,
    /// True when someone other than the reviewer resolved it: the reviewer opens it again,
    /// or resolves it itself if the head fixes it.
    pub resolved_by_other: bool,
}

/// What the review commands read from GitHub. Every error says what failed.
pub trait Forge {
    /// Pull request `number`.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or answers in another shape.
    fn pull(&self, number: u32) -> Result<PullFacts, String>;

    /// The files that pull request `number` changes.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or there are too many.
    fn files(&self, number: u32) -> Result<Vec<ChangedFile>, String>;

    /// The threads on pull request `number` whose first comment is by the app `reviewer` (its
    /// login with `[bot]`), and which that app has not resolved ([`OpenThread`]).
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or there are more threads than one page holds.
    fn open_threads(&self, number: u32, reviewer: &str) -> Result<Vec<OpenThread>, String>;
}

/// What the review commands do as the reviewer app.
pub trait Poster {
    /// Resolves the review thread `id`.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it.
    fn resolve(&self, id: &str) -> Result<(), String>;

    /// Opens the review thread `id` again.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it.
    fn unresolve(&self, id: &str) -> Result<(), String>;

    /// Posts the review `request` (the JSON body of `POST /pulls/{number}/reviews`) on pull
    /// request `number`; returns the review's URL.
    ///
    /// # Errors
    ///
    /// When GitHub refuses it.
    fn review(&self, number: u32, request: &str) -> Result<String, String>;
}

/// Most threads read: one page. A pull request with more fails the command.
const THREADS_MAX: u32 = 100;

/// Most comments read of one thread: the finding and its replies.
const COMMENTS_MAX: u32 = 30;

/// The open threads query: the first [`THREADS_MAX`] threads, each with who resolved it and
/// its first [`COMMENTS_MAX`] comments.
const THREADS_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(\
     owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100){pageInfo{\
     hasNextPage}nodes{id isResolved resolvedBy{__typename login} path line comments(first:30)\
     {nodes{author{__typename login} body}}}}}}}";

/// Resolves one thread.
const RESOLVE_MUTATION: &str =
    "mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}";

/// Opens one thread again.
const UNRESOLVE_MUTATION: &str =
    "mutation($id:ID!){unresolveReviewThread(input:{threadId:$id}){thread{isResolved}}}";

impl Forge for Gh {
    fn pull(&self, number: u32) -> Result<PullFacts, String> {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}");
        pull_from_json(&self.one(&[&path], None)?)
    }

    fn files(&self, number: u32) -> Result<Vec<ChangedFile>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}/files?per_page=100");
        self.list(&path)?.iter().map(file_from_json).collect()
    }

    fn open_threads(&self, number: u32, reviewer: &str) -> Result<Vec<OpenThread>, String> {
        let query = format!("query={THREADS_QUERY}");
        let number = format!("number={number}");
        let arguments =
            ["graphql", "-f", &query, "-F", "owner={owner}", "-F", "name={repo}", "-F", &number];
        threads_from_json(&self.one(&arguments, None)?, reviewer)
    }
}

impl Poster for Gh {
    fn resolve(&self, id: &str) -> Result<(), String> {
        self.set_resolved(id, RESOLVE_MUTATION, "resolveReviewThread", true)
    }

    fn unresolve(&self, id: &str) -> Result<(), String> {
        self.set_resolved(id, UNRESOLVE_MUTATION, "unresolveReviewThread", false)
    }

    fn review(&self, number: u32, request: &str) -> Result<String, String> {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews");
        let posted = self.one(&["--method", "POST", &path, "--input", "-"], Some(request))?;
        string_at(&posted, &["html_url"])
    }
}

impl Gh {
    /// Runs `mutation`, whose field is `field`, on thread `id`, and checks that the thread's
    /// `isResolved` is then `resolved`.
    fn set_resolved(
        &self,
        id: &str,
        mutation: &str,
        field: &str,
        resolved: bool,
    ) -> Result<(), String> {
        assert!(mutation.contains(field), "the mutation names its field");
        let query = format!("query={mutation}");
        let id_field = format!("id={id}");
        let answer = self.one(&["graphql", "-f", &query, "-f", &id_field], None)?;
        let path = ["data", field, "thread", "isResolved"];
        let state = path.iter().try_fold(&answer, |inner, key| inner.get(key));
        if state.and_then(Value::as_bool) == Some(resolved) {
            return Ok(());
        }
        Err(format!("GitHub did not change thread {id} with {field}"))
    }
}

/// Reads `GET /repos/{owner}/{repo}/pulls/{number}`.
///
/// # Errors
///
/// When a field the commands need is missing or has another type.
pub fn pull_from_json(value: &Value) -> Result<PullFacts, String> {
    let flag = |key: &str| {
        value.get(key).and_then(Value::as_bool).ok_or_else(|| format!("GitHub sent no `{key}`"))
    };
    let head_repository = match value.get("head").and_then(|head| head.get("repo")) {
        Some(Value::Null) => None,
        Some(repository) => Some(string_at(repository, &["full_name"])?),
        None => return Err("GitHub sent no `head.repo`".to_owned()),
    };
    let state = string_at(value, &["state"])?;
    let facts = PullFacts {
        author: string_at(value, &["user", "login"])?,
        author_is_app: string_at(value, &["user", "type"])? == "Bot",
        is_open: state == "open",
        is_draft: flag("draft")?,
        head_sha: string_at(value, &["head", "sha"])?,
        head_ref: string_at(value, &["head", "ref"])?,
        head_repository,
        base_repository: string_at(value, &["base", "repo", "full_name"])?,
        base_ref: string_at(value, &["base", "ref"])?,
        base_sha: string_at(value, &["base", "sha"])?,
        title: string_at(value, &["title"])?,
        body: optional_string(value, "body")?,
    };
    assert!(!facts.head_sha.is_empty(), "a pull request has a head commit");
    Ok(facts)
}

/// Reads one entry of `GET /repos/{owner}/{repo}/pulls/{number}/files`.
///
/// # Errors
///
/// When the entry has no file name, or a patch that is not text.
pub fn file_from_json(value: &Value) -> Result<ChangedFile, String> {
    let path = string_at(value, &["filename"])?;
    let patch = match value.get("patch") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => return Err(format!("GitHub sent {} as a file's patch", other.kind())),
    };
    assert!(!path.is_empty(), "a changed file has a name");
    Ok(ChangedFile { path, patch })
}

/// Reads the answer to [`THREADS_QUERY`]: the threads whose first comment's author is the app
/// `reviewer`, and which that app has not resolved ([`OpenThread`]). GraphQL names an author
/// app as `__typename` `Bot` with its login without `[bot]`, so a user whose login looks the
/// same is never taken for the app; it names `resolvedBy` as a `User` whose login already ends
/// in `[bot]` (as `xtask/src/ratchet/review.rs` reads it too).
///
/// # Errors
///
/// When the answer has another shape, or more threads or comments than one page holds.
pub fn threads_from_json(answer: &Value, reviewer: &str) -> Result<Vec<OpenThread>, String> {
    let bare = reviewer.strip_suffix("[bot]").ok_or("the reviewer is an app's login with [bot]")?;
    let path = ["data", "repository", "pullRequest", "reviewThreads"];
    let threads = path.iter().try_fold(answer, |inner, key| inner.get(key));
    let threads = threads.ok_or_else(|| format!("GitHub sent no `{}`", path.join(".")))?;
    let more = threads.get("pageInfo").and_then(|info| info.get("hasNextPage"));
    if more.and_then(Value::as_bool) != Some(false) {
        return Err(format!("the pull request has more than {THREADS_MAX} review threads"));
    }
    let nodes = threads.get("nodes").and_then(Value::as_array).ok_or("GitHub sent no threads")?;
    let mut open = Vec::new();
    for node in nodes {
        if let Some(thread) = open_thread(node, bare, reviewer)? {
            open.push(thread);
        }
    }
    assert!(open.len() <= nodes.len(), "only threads that GitHub sent");
    Ok(open)
}

/// The thread `node` if the app (`bare` login, `reviewer` with `[bot]`) opened it and has not
/// resolved it.
fn open_thread(node: &Value, bare: &str, reviewer: &str) -> Result<Option<OpenThread>, String> {
    let comments = node.get("comments").and_then(|comments| comments.get("nodes"));
    let comments = comments.and_then(Value::as_array).unwrap_or_default();
    let Some((first, later)) = comments.split_first() else { return Ok(None) };
    let author = |comment: &Value, key: &str| {
        let found = comment.get("author").and_then(|author| author.get(key));
        found.and_then(Value::as_str).unwrap_or_default().to_owned()
    };
    if author(first, "__typename") != "Bot" || author(first, "login") != bare {
        return Ok(None);
    }
    let resolved = node.get("isResolved").and_then(Value::as_bool);
    let resolved = resolved.ok_or("GitHub sent a thread without `isResolved`")?;
    let resolver = node.get("resolvedBy").and_then(|by| by.get("login")).and_then(Value::as_str);
    if resolved && resolver == Some(reviewer) {
        return Ok(None);
    }
    if comments.len() >= usize::try_from(COMMENTS_MAX).unwrap_or(usize::MAX) {
        return Err(format!("a review thread has {COMMENTS_MAX} comments or more"));
    }
    let replies = later
        .iter()
        .map(|reply| Ok((author(reply, "login"), optional_string(reply, "body")?)))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Some(OpenThread {
        id: string_at(node, &["id"])?,
        path: string_at(node, &["path"])?,
        line: node.get("line").and_then(line_of),
        body: optional_string(first, "body")?,
        replies,
        resolved_by_other: resolved,
    }))
}

/// A thread's line, which GitHub sends as `null` once the thread is outdated.
fn line_of(value: &Value) -> Option<u32> {
    let Value::Number(text) = value else { return None };
    let line: u32 = text.parse().ok()?;
    assert!(line > 0, "GitHub counts lines from 1");
    Some(line)
}

/// The JSON request body of a review: `event` on `commit`, with `body`, and one inline
/// comment per `(path, line, text)`, on the head's side.
#[must_use]
pub fn review_request(
    commit: &str,
    event: &str,
    body: &str,
    comments: &[(String, u32, String)],
) -> String {
    assert!(matches!(event, "APPROVE" | "COMMENT"), "the reviewer approves or comments, only");
    assert!(!commit.is_empty(), "a review names the commit it reviewed");
    let comments: Vec<String> = comments
        .iter()
        .map(|(path, line, text)| {
            format!(
                "{{\"path\": {}, \"line\": {line}, \"side\": \"RIGHT\", \"body\": {}}}",
                json::quote(path),
                json::quote(text)
            )
        })
        .collect();
    format!(
        "{{\"commit_id\": {}, \"event\": {}, \"body\": {}, \"comments\": [{}]}}",
        json::quote(commit),
        json::quote(event),
        json::quote(body),
        comments.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::{file_from_json, pull_from_json, review_request, threads_from_json};
    use crate::json::parse;

    // Goal: a pull request is read with its author's kind, its draft state and both
    // repositories; a deleted head repository is `None`; a missing field is an error.
    #[test]
    fn pulls_are_read() {
        let pull = r#"{"user": {"login": "nami-ci[bot]", "type": "Bot"}, "state": "open",
            "draft": false, "title": "t", "body": null,
            "head": {"sha": "abc", "ref": "bot/passes", "repo": {"full_name": "o/n"}},
            "base": {"ref": "main", "sha": "def", "repo": {"full_name": "o/n"}}}"#;
        let facts = pull_from_json(&parse(pull).unwrap()).unwrap();
        assert!(facts.author_is_app && facts.is_open && !facts.is_draft);
        assert_eq!((facts.head_ref.as_str(), facts.body.as_str()), ("bot/passes", ""));
        assert_eq!(facts.head_repository.as_deref(), Some("o/n"));
        let gone = pull.replace(r#""repo": {"full_name": "o/n"}},"#, r#""repo": null},"#);
        assert_eq!(pull_from_json(&parse(&gone).unwrap()).unwrap().head_repository, None);
        let no_draft = pull.replace(r#""draft": false,"#, "");
        assert!(pull_from_json(&parse(&no_draft).unwrap()).is_err());
    }

    // Goal: a file's patch is read when GitHub sends one, and is `None` when it does not.
    #[test]
    fn files_are_read() {
        let file = parse(r#"{"filename": "a.rs", "patch": "@@ -1 +1 @@\n-a\n+b"}"#).unwrap();
        assert_eq!(file_from_json(&file).unwrap().patch.as_deref(), Some("@@ -1 +1 @@\n-a\n+b"));
        let binary = parse(r#"{"filename": "a.bin"}"#).unwrap();
        assert_eq!(file_from_json(&binary).unwrap().patch, None);
    }

    // Goal: the open threads are the reviewer app's threads that the app has not resolved: not
    // one it resolved, not one by a user with the same bare login, not another app's; a thread
    // that the author resolved is still open, marked as resolved by another, with its replies;
    // a second page is an error.
    #[test]
    fn only_the_reviewer_s_open_threads_count() {
        let thread = |id: &str, resolver: &str, kind: &str, login: &str| {
            let (resolved, by) = if resolver.is_empty() {
                ("false", "null".to_owned())
            } else {
                ("true", format!(r#"{{"__typename": "User", "login": "{resolver}"}}"#))
            };
            format!(
                r#"{{"id": "{id}", "isResolved": {resolved}, "resolvedBy": {by}, "path": "a.rs",
                "line": 3, "comments": {{"nodes": [{{"author": {{"__typename": "{kind}",
                "login": "{login}"}}, "body": "**Blocking** x"}}, {{"author": {{"__typename":
                "Bot", "login": "nami-agent"}}, "body": "Not a bug: see a.rs:9"}}]}}}}"#
            )
        };
        let answer = |more: bool, nodes: &[String]| {
            parse(&format!(
                r#"{{"data": {{"repository": {{"pullRequest": {{"reviewThreads": {{"pageInfo":
                {{"hasNextPage": {more}}}, "nodes": [{}]}}}}}}}}}}"#,
                nodes.join(",")
            ))
            .unwrap()
        };
        let nodes = [
            thread("T1", "", "Bot", "nami-reviewer"),
            thread("T2", "nami-reviewer[bot]", "Bot", "nami-reviewer"),
            thread("T3", "", "User", "nami-reviewer"),
            thread("T4", "", "Bot", "nami-agent"),
            thread("T5", "nami-agent[bot]", "Bot", "nami-reviewer"),
        ];
        let open = threads_from_json(&answer(false, &nodes), "nami-reviewer[bot]").unwrap();
        let ids: Vec<(&str, bool)> =
            open.iter().map(|found| (found.id.as_str(), found.resolved_by_other)).collect();
        assert_eq!(ids, [("T1", false), ("T5", true)]);
        assert_eq!(open[0].line, Some(3));
        let reply = ("nami-agent".to_owned(), "Not a bug: see a.rs:9".to_owned());
        assert_eq!(open[1].replies, [reply]);
        assert!(threads_from_json(&answer(true, &nodes), "nami-reviewer[bot]").is_err());
        assert!(threads_from_json(&answer(false, &nodes), "nami-reviewer").is_err());
    }

    // Goal: a review request is valid JSON with the commit, the event and every comment on
    // the head's side, its text quoted.
    #[test]
    fn review_requests_are_json() {
        let comments = [("a.rs".to_owned(), 7, "say \"why\"".to_owned())];
        let request = review_request("abc", "COMMENT", "body", &comments);
        let value = parse(&request).unwrap();
        assert_eq!(value.get("event").and_then(|event| event.as_str()), Some("COMMENT"));
        let comment = &value.get("comments").unwrap().as_array().unwrap()[0];
        assert_eq!(comment.get("side").and_then(|side| side.as_str()), Some("RIGHT"));
        assert_eq!(comment.get("body").and_then(|body| body.as_str()), Some("say \"why\""));
    }
}
