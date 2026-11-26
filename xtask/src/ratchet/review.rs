//! The independent review has run on the pull request, and everything raised is resolved by
//! the reviewer (ADR-0008, ADR-0022).
//!
//! The rule: the reviewer app (`nami-reviewer[bot]`, a GitHub App separate from the agents'
//! `nami-agent[bot]`, whose key the authoring sessions never hold) has submitted a review on
//! the PR's head commit that is not dismissed; the PR was not opened by that app; and every
//! review thread is resolved, by that app. A review of an earlier commit does not count, not
//! even when the later commits are fixes: `review-pr` re-reviews the new head and resolves the
//! threads that are fixed then.
//!
//! [`Reviews`] is what the check needs from GitHub. [`GhReviews`] asks through `gh api`, as
//! whoever `GH_TOKEN` names; the tests use fixed data.

use super::{Check, Finding};
use crate::issue::github::parse_lines;
use crate::json::{self, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The agents' own app, which writes the pull requests and can never be the reviewer.
pub const AUTHOR_APP: &str = "nami-agent[bot]";

/// The reviewer app's login, as the runbook names it.
pub const REVIEWER_APP: &str = "nami-reviewer[bot]";

/// Most review threads one query returns. A pull request with more is an error.
const THREADS_MAX: u32 = 100;

/// The state of a review that no longer counts.
const DISMISSED: &str = "DISMISSED";

/// A pull request: its head commit and who opened it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pull {
    /// The head commit.
    pub head: String,
    /// The login of whoever opened it.
    pub author: String,
}

/// One submitted review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    /// The reviewer's login.
    pub author: String,
    /// The commit the review was submitted on.
    pub commit: String,
    /// `COMMENTED`, `APPROVED`, `CHANGES_REQUESTED` or `DISMISSED`.
    pub state: String,
}

/// One review thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    /// The file it is on.
    pub path: String,
    /// Who opened it.
    pub author: String,
    /// Who resolved it, if it is resolved.
    pub resolved_by: Option<String>,
}

/// What the review check needs from GitHub. Every error says what failed.
pub trait Reviews {
    /// The pull request.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked.
    fn pull(&self, pull: u32) -> Result<Pull, String>;

    /// Every review submitted on the pull request.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked.
    fn reviews(&self, pull: u32) -> Result<Vec<Review>, String>;

    /// Every review thread of the pull request.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked, or the pull request has more threads than one query reads.
    fn threads(&self, pull: u32) -> Result<Vec<Thread>, String>;
}

/// A login without the `[bot]` that the REST API adds and GraphQL leaves out.
fn bare(login: &str) -> &str {
    login.strip_suffix("[bot]").unwrap_or(login)
}

/// Applies the review rule to pull request `pull`, for the reviewer app `reviewer`.
///
/// # Errors
///
/// Returns what failed if GitHub cannot be asked, or if `reviewer` is not an app other than
/// the agents' own.
pub fn check(reviews: &dyn Reviews, pull: u32, reviewer: &str) -> Result<Vec<Finding>, String> {
    assert!(pull > 0, "pull requests are numbered from 1");
    if !reviewer.ends_with("[bot]") || bare(reviewer) == bare(AUTHOR_APP) {
        return Err(format!(
            "the reviewer `{reviewer}` must be a GitHub App other than {AUTHOR_APP}"
        ));
    }
    let place = format!("pull request #{pull}");
    let request = reviews.pull(pull)?;
    let mut findings = Vec::new();
    let mut report = |message: String| findings.push(Finding::new(&place, Check::Review, message));
    if bare(&request.author) == bare(reviewer) {
        report(format!("{reviewer} opened this pull request, so it cannot review it (ADR-0022)"));
    }
    let on_head = reviews.reviews(pull)?.into_iter().any(|review| {
        bare(&review.author) == bare(reviewer)
            && review.commit == request.head
            && review.state != DISMISSED
    });
    if !on_head {
        report(format!(
            "{reviewer} has not reviewed the head commit {}; run `/review-pr {pull}` in a fresh \
             session with the reviewer's key, which reviews the head and resolves the threads \
             that are fixed (ADR-0008)",
            request.head
        ));
    }
    for thread in reviews.threads(pull)? {
        match &thread.resolved_by {
            None => report(format!(
                "a review thread by {} on {} is not resolved; fix it, or answer why the finding \
                 is wrong, and have the reviewer resolve it (ADR-0008)",
                thread.author, thread.path
            )),
            Some(by) if bare(by) != bare(reviewer) => report(format!(
                "a review thread on {} was resolved by {by}, not by {reviewer}; only the \
                 reviewer resolves threads for the ratchet (ADR-0022)",
                thread.path
            )),
            Some(_) => {}
        }
    }
    Ok(findings)
}

/// The state of an approving review.
const APPROVED: &str = "APPROVED";

/// True if a user that the base's CODEOWNERS (`codeowners`) names approved the pull request's
/// head commit. Such an approval stands in for whatever the ratchet cannot prove, so that a
/// rule-only PR needs the owner, never the admin bypass (ADR-0022).
///
/// # Errors
///
/// Returns what failed if GitHub cannot be asked.
pub fn owner_approved(reviews: &dyn Reviews, pull: u32, codeowners: &str) -> Result<bool, String> {
    let owners = crate::issue::trust::Trust::from_codeowners(codeowners);
    let head = reviews.pull(pull)?.head;
    let approved = reviews.reviews(pull)?.into_iter().any(|review| {
        review.state == APPROVED && review.commit == head && owners.is_owner(&review.author)
    });
    Ok(approved)
}

/// Reads the reviews from `gh api ... --jq '.[]'` output. A review by a deleted account has no
/// user and is skipped.
///
/// # Errors
///
/// Returns what is missing if a line is not a review.
pub fn reviews_from(text: &str) -> Result<Vec<Review>, String> {
    let mut found = Vec::new();
    for value in parse_lines(text)? {
        let Some(author) = value.get("user").and_then(|user| user.get("login")) else {
            continue;
        };
        let author = author.as_str().ok_or("GitHub sent a review whose user has no login")?;
        let field =
            |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
        found.push(Review {
            author: author.to_owned(),
            commit: field("commit_id"),
            state: field("state"),
        });
    }
    Ok(found)
}

/// Reads the threads from the GraphQL answer of [`THREADS_QUERY`].
///
/// # Errors
///
/// Returns what is missing if the answer has another shape, or if there are more threads than
/// one query reads.
pub fn threads_from(value: &Value) -> Result<Vec<Thread>, String> {
    let threads = ["data", "repository", "pullRequest", "reviewThreads"]
        .iter()
        .try_fold(value, |inner, key| inner.get(key))
        .ok_or("GitHub sent no review threads; check the pull request number")?;
    let has_more = threads.get("pageInfo").and_then(|info| info.get("hasNextPage"));
    if has_more != Some(&Value::Bool(false)) {
        return Err(format!(
            "the pull request has more than {THREADS_MAX} review threads, more than the ratchet \
             reads; resolve them, or open a new pull request"
        ));
    }
    let nodes =
        threads.get("nodes").and_then(Value::as_array).ok_or("GitHub sent no thread list")?;
    let mut found = Vec::new();
    for node in nodes {
        let is_resolved = node
            .get("isResolved")
            .and_then(Value::as_bool)
            .ok_or("a thread has no `isResolved`")?;
        let login = |actor: Option<&Value>| {
            actor.and_then(|actor| actor.get("login")).and_then(Value::as_str).map(str::to_owned)
        };
        let first = node
            .get("comments")
            .and_then(|comments| comments.get("nodes"))
            .and_then(Value::as_array)
            .and_then(|comments| comments.first());
        let resolved_by = is_resolved.then(|| {
            login(node.get("resolvedBy")).unwrap_or_else(|| "a deleted account".to_owned())
        });
        found.push(Thread {
            path: node.get("path").and_then(Value::as_str).unwrap_or("the pull request").to_owned(),
            author: login(first.and_then(|comment| comment.get("author")))
                .unwrap_or_else(|| "a deleted account".to_owned()),
            resolved_by,
        });
    }
    Ok(found)
}

/// The GraphQL query for the review threads of a pull request.
const THREADS_QUERY: &str = "query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r)\
     {pullRequest(number:$n){reviewThreads(first:100){pageInfo{hasNextPage}nodes{isResolved \
     path resolvedBy{login} comments(first:1){nodes{author{login}}}}}}}}";

/// [`Reviews`] through `gh api`, run in a checkout of the repository.
#[derive(Debug, Clone)]
pub struct GhReviews {
    root: PathBuf,
    program: PathBuf,
}

impl GhReviews {
    /// Runs `gh` in the checkout at `root`, which decides the repository.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self::using(root, Path::new("gh"))
    }

    /// Runs `program` as `gh`: for the tests, which stand in for GitHub.
    #[must_use]
    pub fn using(root: &Path, program: &Path) -> Self {
        assert!(!program.as_os_str().is_empty(), "a program to run");
        Self { root: root.to_path_buf(), program: program.to_path_buf() }
    }

    fn api(&self, arguments: &[&str]) -> Result<String, String> {
        let output = Command::new(&self.program)
            .arg("api")
            .args(arguments)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("cannot run gh: {error}; install the GitHub CLI"))?;
        if output.status.success() {
            return String::from_utf8(output.stdout)
                .map_err(|_| "gh printed invalid UTF-8".to_owned());
        }
        Err(format!(
            "`gh api {}` failed: {}; check that GitHub is reachable and that `GH_TOKEN` is set",
            arguments.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

impl Reviews for GhReviews {
    fn pull(&self, pull: u32) -> Result<Pull, String> {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{pull}");
        let text = self.api(&[&path, "--jq", ".head.sha + \" \" + .user.login"])?;
        let (sha, author) = text
            .trim()
            .split_once(' ')
            .ok_or_else(|| format!("GitHub sent `{}` for #{pull}", text.trim()))?;
        if sha.len() < 40 || !sha.chars().all(|character| character.is_ascii_hexdigit()) {
            return Err(format!("GitHub sent `{sha}` as the head commit of #{pull}"));
        }
        Ok(Pull { head: sha.to_owned(), author: author.to_owned() })
    }

    fn reviews(&self, pull: u32) -> Result<Vec<Review>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{pull}/reviews?per_page=100");
        reviews_from(&self.api(&["--paginate", &path, "--jq", ".[]"])?)
    }

    fn threads(&self, pull: u32) -> Result<Vec<Thread>, String> {
        let query = format!("query={THREADS_QUERY}");
        let number = format!("n={pull}");
        let arguments =
            ["graphql", "-f", &query, "-F", "o={owner}", "-F", "r={repo}", "-F", &number];
        let text = self.api(&arguments)?;
        let value = json::parse(&text)
            .map_err(|error| format!("gh printed invalid JSON: {}", error.message))?;
        threads_from(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::{AUTHOR_APP, GhReviews, Pull, REVIEWER_APP, Review, Reviews, Thread};
    use super::{check, owner_approved, reviews_from, threads_from};
    use crate::json;

    struct Fixed {
        author: &'static str,
        reviews: Vec<Review>,
        threads: Vec<Thread>,
    }

    impl Reviews for Fixed {
        fn pull(&self, _pull: u32) -> Result<Pull, String> {
            Ok(Pull { head: "b".to_owned(), author: self.author.to_owned() })
        }
        fn reviews(&self, _pull: u32) -> Result<Vec<Review>, String> {
            Ok(self.reviews.clone())
        }
        fn threads(&self, _pull: u32) -> Result<Vec<Thread>, String> {
            Ok(self.threads.clone())
        }
    }

    fn review(author: &str, commit: &str, state: &str) -> Review {
        Review { author: author.to_owned(), commit: commit.to_owned(), state: state.to_owned() }
    }

    fn thread(resolved_by: Option<&str>) -> Thread {
        Thread {
            path: "a.rs".to_owned(),
            author: REVIEWER_APP.to_owned(),
            resolved_by: resolved_by.map(str::to_owned),
        }
    }

    fn messages(fixed: &Fixed) -> Vec<String> {
        check(fixed, 7, REVIEWER_APP).unwrap().into_iter().map(|finding| finding.message).collect()
    }

    // Goal: the reviewer app's review on the head, with every thread resolved by it, passes.
    // The author's own review, a dismissed or earlier review, an open thread, a thread resolved
    // by someone else, and a PR opened by the reviewer are findings.
    #[test]
    fn review_rule_holds_on_the_head() {
        let good = Fixed {
            author: AUTHOR_APP,
            reviews: vec![review("nami-reviewer[bot]", "b", "COMMENTED")],
            threads: vec![thread(Some("nami-reviewer"))],
        };
        assert_eq!(messages(&good), Vec::<String>::new());
        let own = Fixed {
            author: AUTHOR_APP,
            reviews: vec![review(AUTHOR_APP, "b", "COMMENTED")],
            threads: vec![],
        };
        assert!(messages(&own)[0].contains("has not reviewed the head commit b"));
        let dismissed = Fixed {
            author: AUTHOR_APP,
            reviews: vec![review(REVIEWER_APP, "b", "DISMISSED")],
            threads: vec![],
        };
        assert_eq!(messages(&dismissed).len(), 1);
        let earlier = Fixed {
            author: AUTHOR_APP,
            reviews: vec![review(REVIEWER_APP, "a", "APPROVED")],
            threads: vec![],
        };
        assert_eq!(messages(&earlier).len(), 1);
        let open = Fixed {
            author: AUTHOR_APP,
            reviews: vec![review(REVIEWER_APP, "b", "COMMENTED")],
            threads: vec![thread(None), thread(Some("nami-agent"))],
        };
        let found = messages(&open);
        assert!(
            found[0].contains("is not resolved")
                && found[1].contains("resolved by nami-agent, not by"),
            "{found:?}"
        );
        let by_reviewer = Fixed {
            author: REVIEWER_APP,
            reviews: vec![review(REVIEWER_APP, "b", "COMMENTED")],
            threads: vec![],
        };
        assert!(messages(&by_reviewer)[0].contains("opened this pull request"));
        assert!(check(&good, 7, AUTHOR_APP).is_err(), "the author's app is never the reviewer");
        assert!(check(&good, 7, "maintainer").is_err(), "the reviewer is an app");
    }

    // Goal: reviews are read from the REST answer with their state, skipping deleted accounts.
    #[test]
    fn reviews_are_read() {
        let text = "{\"user\":{\"login\":\"nami-reviewer[bot]\"},\"commit_id\":\"abc\",\
                    \"state\":\"COMMENTED\"}\n\
                    {\"user\":null,\"commit_id\":\"def\"}\n";
        assert_eq!(reviews_from(text).unwrap(), [review(REVIEWER_APP, "abc", "COMMENTED")]);
        assert!(reviews_from("{\"user\":{\"login\":3}}\n").is_err(), "a login is a string");
    }

    // Goal: threads are read with who resolved them; an answer with more pages, or of another
    // shape, is an error.
    #[test]
    fn threads_are_read() {
        let answer = |more: &str| {
            json::parse(&format!(
                "{{\"data\":{{\"repository\":{{\"pullRequest\":{{\"reviewThreads\":{{\
                 \"pageInfo\":{{\"hasNextPage\":{more}}},\"nodes\":[\
                 {{\"isResolved\":true,\"path\":\"a.rs\",\
                 \"resolvedBy\":{{\"login\":\"nami-reviewer\"}},\"comments\":{{\"nodes\":[]}}}},\
                 {{\"isResolved\":false,\"path\":\"b.rs\",\"resolvedBy\":null,\
                 \"comments\":{{\"nodes\":[\
                 {{\"author\":{{\"login\":\"maintainer\"}}}}]}}}}]}}}}}}}}}}"
            ))
            .unwrap()
        };
        let found = threads_from(&answer("false")).unwrap();
        assert_eq!(found[0].resolved_by.as_deref(), Some("nami-reviewer"));
        assert_eq!(
            found[1],
            Thread { path: "b.rs".to_owned(), author: "maintainer".to_owned(), resolved_by: None }
        );
        assert!(threads_from(&answer("true")).unwrap_err().contains("more than 100"));
        assert!(threads_from(&json::parse("{\"data\":null}").unwrap()).is_err());
    }

    /// A stand-in for `gh api` that answers for pull request 7, or fails for any other.
    const FAKE_GH: &str = r#"#!/bin/sh
case "$*" in
    *pulls/7\ --jq*) echo "0123456789abcdef0123456789abcdef01234567 nami-agent[bot]" ;;
    *pulls/7/reviews*)
        user='"user":{"login":"nami-reviewer[bot]"}'
        echo "{$user,\"commit_id\":\"abc\",\"state\":\"COMMENTED\"}" ;;
    *n=7*)
        thread='{"isResolved":false,"path":"a.rs","resolvedBy":null,"comments":{"nodes":[]}}'
        threads="{\"pageInfo\":{\"hasNextPage\":false},\"nodes\":[$thread]}"
        echo "{\"data\":{\"repository\":{\"pullRequest\":{\"reviewThreads\":$threads}}}}" ;;
    *pulls/8\ --jq*) echo "not-a-commit someone" ;;
    *) echo "HTTP 404: Not Found" >&2; exit 1 ;;
esac
"#;

    // Goal: `gh api` answers are read into the pull request, reviews and threads, and a failing
    // `gh` or an answer that is not a commit is an error, never an empty list.
    #[test]
    fn gh_answers_are_read() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-fake-gh");
        scratch.write("gh", FAKE_GH);
        let program = scratch.root().join("gh");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let github = GhReviews::using(scratch.root(), &program);
        let pull = github.pull(7).unwrap();
        assert_eq!((pull.head.len(), pull.author.as_str()), (40, AUTHOR_APP));
        assert_eq!(github.reviews(7).unwrap(), [review(REVIEWER_APP, "abc", "COMMENTED")]);
        let threads = github.threads(7).unwrap();
        assert_eq!(
            threads,
            [Thread {
                path: "a.rs".to_owned(),
                author: "a deleted account".to_owned(),
                resolved_by: None
            }]
        );
        assert!(github.pull(8).unwrap_err().contains("as the head commit of #8"));
        let failed = github.reviews(9).unwrap_err();
        assert!(failed.contains("HTTP 404") && failed.contains("GH_TOKEN"), "{failed}");
        assert!(github.threads(9).is_err(), "a failed query");
    }

    // Goal: only an approval by a code owner on the head commit counts: not a comment, not an
    // approval of an earlier commit, and not an approval by someone CODEOWNERS does not name.
    #[test]
    fn code_owner_approvals_are_found() {
        let codeowners = "/xtask/ @Owner\n";
        let approved = |reviews: Vec<Review>| {
            let fixed = Fixed { author: AUTHOR_APP, reviews, threads: vec![] };
            owner_approved(&fixed, 7, codeowners).unwrap()
        };
        assert!(approved(vec![review("owner", "b", "APPROVED")]), "logins ignore case");
        assert!(!approved(vec![review("owner", "b", "COMMENTED")]));
        assert!(!approved(vec![review("owner", "a", "APPROVED")]));
        assert!(!approved(vec![review(REVIEWER_APP, "b", "APPROVED")]));
        assert!(!approved(vec![]));
    }
}
