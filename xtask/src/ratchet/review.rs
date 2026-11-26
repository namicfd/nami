//! The independent review has run on the pull request, and everything raised is resolved by
//! the reviewer (ADR-0008, ADR-0022).
//!
//! The rule: the reviewer app, the bot that the repository variable `NAMI_REVIEWER_LOGIN` names
//! (such as `nami-reviewer[bot]`, a GitHub App separate from the agents' `nami-agent[bot]`), has
//! reviewed the PR's head commit, and its latest review there is neither dismissed nor a
//! request for changes; the PR
//! was not opened by that app; and every review thread that the app opened is resolved, by that
//! app. The ratchet counts only the app's own threads. Everyone else's threads, resolved or not
//! and by whomever, are left to the ruleset, which requires every conversation resolved before
//! merge, and are never a ratchet finding. A review of an earlier commit does not count, not
//! even when the later commits are fixes: the reviewer re-reviews the new head and resolves the
//! threads that are fixed then.
//!
//! The gate counts the reviews that bot posts, whatever posts them. The review is meant to run
//! in CI (pending #59), where no authoring session holds the app's key; the gate itself does
//! not depend on where the review ran.
//!
//! **Identity.** An app is matched by its exact login with `[bot]` and by its account type,
//! never by a login alone: a user account named `nami-reviewer` is not the reviewer app. The
//! REST API gives `user.type == "Bot"` and the login with `[bot]`. GraphQL gives an author as
//! `__typename: "Bot"` and the login without `[bot]`, which [`Actor`] adds back; it gives
//! `resolvedBy` as a `User` whose login already ends in `[bot]`. GitHub logins cannot hold `[`,
//! so only an app has such a login.
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

/// The reviewer app's login, as the runbook names it; CI reads the one in use from
/// `NAMI_REVIEWER_LOGIN`.
pub const REVIEWER_APP: &str = "nami-reviewer[bot]";

/// Most review threads one query returns. A pull request with more is an error.
const THREADS_MAX: u32 = 100;

/// The state of a review that no longer counts.
const DISMISSED: &str = "DISMISSED";

/// The state of a review that asks for changes.
const CHANGES_REQUESTED: &str = "CHANGES_REQUESTED";

/// The state of a review that leaves the reviewer's decision as it was.
const COMMENTED: &str = "COMMENTED";

/// A GitHub account: its login, with `[bot]` for an app, and whether it is an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    /// The login; an app's ends in `[bot]`.
    pub login: String,
    /// True for a GitHub App, false for a user account.
    pub bot: bool,
}

impl Actor {
    /// A user account.
    #[must_use]
    pub fn user(login: &str) -> Self {
        Self { login: login.to_owned(), bot: false }
    }

    /// A GitHub App, by its login with `[bot]`.
    #[must_use]
    pub fn app(login: &str) -> Self {
        assert!(login.ends_with("[bot]"), "an app's login ends in `[bot]`");
        Self { login: login.to_owned(), bot: true }
    }

    /// True if this is the app `app`, by type and exact login.
    #[must_use]
    pub fn is_app(&self, app: &str) -> bool {
        self.bot && self.login == app
    }

    /// Reads a GraphQL actor: `__typename` `Bot` with the bare login, or a `User` whose login
    /// ends in `[bot]`, which is how GitHub names an app in `resolvedBy`. `None` for a deleted
    /// account.
    fn from_graphql(actor: Option<&Value>) -> Option<Self> {
        let actor = actor.filter(|actor| **actor != Value::Null)?;
        let login = actor.get("login").and_then(Value::as_str)?;
        let kind = actor.get("__typename").and_then(Value::as_str).unwrap_or_default();
        Some(if kind == "Bot" {
            Self {
                login: format!("{}[bot]", login.strip_suffix("[bot]").unwrap_or(login)),
                bot: true,
            }
        } else {
            Self { login: login.to_owned(), bot: login.ends_with("[bot]") }
        })
    }
}

impl std::fmt::Display for Actor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.login)
    }
}

/// A pull request: its head commit and who opened it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pull {
    /// The head commit.
    pub head: String,
    /// Whoever opened it.
    pub author: Actor,
}

/// One submitted review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    /// The reviewer.
    pub author: Actor,
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
    /// Who opened it; `None` for a deleted account.
    pub author: Option<Actor>,
    /// True if it is resolved.
    pub resolved: bool,
    /// Who resolved it; `None` if it is open, or for a deleted account.
    pub resolved_by: Option<Actor>,
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

/// The latest review by `who` that decided something: not a comment, which leaves the
/// decision as it was. Reviews come oldest first.
fn latest_decision<'r>(reviews: &'r [Review], who: &Actor) -> Option<&'r Review> {
    reviews.iter().rev().find(|review| review.author == *who && review.state != COMMENTED)
}

/// Fails, for no verdict, if the pull request's head has moved on from the commit judged.
fn same_head(request: &Pull, judged: &str, pull: u32) -> Result<(), String> {
    if request.head == judged {
        return Ok(());
    }
    Err(format!(
        "the head of #{pull} is now {}, not the commit judged, {judged}; the run for the new \
         head decides",
        request.head
    ))
}

/// Applies the review rule to pull request `pull`, whose head must still be `judged`, for the
/// reviewer app `reviewer`.
///
/// # Errors
///
/// No verdict if GitHub cannot be asked, if the head has moved on from `judged`, or if
/// `reviewer` is not an app other than the agents' own.
pub fn check(
    reviews: &dyn Reviews,
    pull: u32,
    judged: &str,
    reviewer: &str,
) -> Result<Vec<Finding>, String> {
    assert!(pull > 0, "pull requests are numbered from 1");
    if !reviewer.ends_with("[bot]") || reviewer == AUTHOR_APP {
        return Err(format!(
            "the reviewer `{reviewer}` must be a GitHub App other than {AUTHOR_APP}, named with \
             `[bot]`"
        ));
    }
    let app = Actor::app(reviewer);
    let place = format!("pull request #{pull}");
    let request = reviews.pull(pull)?;
    same_head(&request, judged, pull)?;
    let mut findings = Vec::new();
    let mut report = |message: String| findings.push(Finding::new(&place, Check::Review, message));
    if request.author == app {
        report(format!("{reviewer} opened this pull request, so it cannot review it (ADR-0022)"));
    }
    let all = reviews.reviews(pull)?;
    let on_head = all.iter().rev().find(|review| review.author == app && review.commit == judged);
    let blocked = latest_decision(&all, &app).map(|review| review.state.as_str());
    match on_head.map(|review| review.state.as_str()) {
        None | Some(DISMISSED) => report(format!(
            "{reviewer} has not reviewed the head commit {judged} of #{pull}; have the reviewer \
             review the head, which also resolves the threads that are fixed (ADR-0008)"
        )),
        Some(_) if blocked == Some(CHANGES_REQUESTED) => report(format!(
            "{reviewer}'s latest decision on this pull request requests changes; fix them, and \
             have it review the head again (ADR-0008)"
        )),
        Some(_) => {}
    }
    for thread in reviews.threads(pull)? {
        let opened_by_reviewer =
            thread.author.as_ref().is_some_and(|author| author.is_app(reviewer));
        if !opened_by_reviewer {
            continue;
        }
        let resolver = thread.resolved_by.as_ref().map(ToString::to_string);
        if !thread.resolved {
            report(format!(
                "a review thread by {reviewer} on {} is not resolved; fix it, or answer why the \
                 finding is wrong, and have the reviewer resolve it (ADR-0008)",
                thread.path
            ));
        } else if !thread.resolved_by.as_ref().is_some_and(|by| by.is_app(reviewer)) {
            report(format!(
                "a review thread on {} was resolved by {}, not by {reviewer}; only the reviewer \
                 resolves its threads for the ratchet (ADR-0022)",
                thread.path,
                resolver.unwrap_or_else(|| "a deleted account".to_owned())
            ));
        }
    }
    Ok(findings)
}

/// The state of an approving review.
const APPROVED: &str = "APPROVED";

/// True if a user that the base's CODEOWNERS (`codeowners`) names approved the commit judged,
/// which must still be the pull request's head, and no code owner's latest decision requests
/// changes. Each owner counts by their latest review that decided something: a later request
/// for changes, or the dismissal of the approval, cancels it. Such an approval stands in for
/// whatever the ratchet cannot prove, so that a rule-only PR needs the owner, never the admin
/// bypass (ADR-0022).
///
/// # Errors
///
/// No verdict if GitHub cannot be asked, or the head has moved on from `judged`.
pub fn owner_approved(
    reviews: &dyn Reviews,
    pull: u32,
    judged: &str,
    codeowners: &str,
) -> Result<bool, String> {
    let owners = crate::issue::trust::Trust::from_codeowners(codeowners);
    same_head(&reviews.pull(pull)?, judged, pull)?;
    let all = reviews.reviews(pull)?;
    let mut approved = false;
    for review in &all {
        if review.author.bot || !owners.is_owner(&review.author.login) {
            continue;
        }
        let latest = latest_decision(&all, &review.author);
        assert!(latest.is_some() || review.state == COMMENTED, "a decision is its own latest");
        match latest.map(|decision| (decision.state.as_str(), decision.commit == judged)) {
            Some((CHANGES_REQUESTED, _)) => return Ok(false),
            Some((APPROVED, true)) => approved = true,
            _ => {}
        }
    }
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
        let Some(user) = value.get("user").filter(|user| **user != Value::Null) else {
            continue;
        };
        let login = user
            .get("login")
            .and_then(Value::as_str)
            .ok_or("GitHub sent a review whose user has no login")?;
        let bot = user.get("type").and_then(Value::as_str) == Some("Bot");
        if bot != login.ends_with("[bot]") {
            return Err(format!("GitHub sent a review by `{login}` whose type disagrees"));
        }
        let field =
            |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
        found.push(Review {
            author: Actor { login: login.to_owned(), bot },
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
        let first = node
            .get("comments")
            .and_then(|comments| comments.get("nodes"))
            .and_then(Value::as_array)
            .and_then(|comments| comments.first());
        let resolved_by = Actor::from_graphql(node.get("resolvedBy")).filter(|_| is_resolved);
        found.push(Thread {
            path: node.get("path").and_then(Value::as_str).unwrap_or("the pull request").to_owned(),
            author: Actor::from_graphql(first.and_then(|comment| comment.get("author"))),
            resolved: is_resolved,
            resolved_by,
        });
    }
    Ok(found)
}

/// The GraphQL query for the review threads of a pull request.
const THREADS_QUERY: &str = "query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r)\
     {pullRequest(number:$n){reviewThreads(first:100){pageInfo{hasNextPage}nodes{isResolved \
     path resolvedBy{__typename login} comments(first:1){nodes{author{__typename login}}}}}}}}";

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
        let jq = ".head.sha + \" \" + .user.login + \" \" + .user.type";
        let text = self.api(&[&path, "--jq", jq])?;
        let mut words = text.split_whitespace();
        let (Some(sha), Some(login), Some(kind), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return Err(format!("GitHub sent `{}` for #{pull}", text.trim()));
        };
        if sha.len() < 40 || !sha.chars().all(|character| character.is_ascii_hexdigit()) {
            return Err(format!("GitHub sent `{sha}` as the head commit of #{pull}"));
        }
        let author = Actor { login: login.to_owned(), bot: kind == "Bot" };
        if author.bot != login.ends_with("[bot]") {
            return Err(format!("GitHub sent `{login}` of type `{kind}` as the author of #{pull}"));
        }
        Ok(Pull { head: sha.to_owned(), author })
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
    use super::{AUTHOR_APP, Actor, GhReviews, Pull, REVIEWER_APP, Review, Reviews, Thread};
    use super::{check, owner_approved, reviews_from, threads_from};
    use crate::json;

    struct Fixed {
        author: Actor,
        reviews: Vec<Review>,
        threads: Vec<Thread>,
    }

    impl Reviews for Fixed {
        fn pull(&self, _pull: u32) -> Result<Pull, String> {
            Ok(Pull { head: "b".to_owned(), author: self.author.clone() })
        }
        fn reviews(&self, _pull: u32) -> Result<Vec<Review>, String> {
            Ok(self.reviews.clone())
        }
        fn threads(&self, _pull: u32) -> Result<Vec<Thread>, String> {
            Ok(self.threads.clone())
        }
    }

    fn fixed(reviews: Vec<Review>, threads: Vec<Thread>) -> Fixed {
        Fixed { author: Actor::app(AUTHOR_APP), reviews, threads }
    }

    fn review(author: Actor, commit: &str, state: &str) -> Review {
        Review { author, commit: commit.to_owned(), state: state.to_owned() }
    }

    fn reviewer() -> Actor {
        Actor::app(REVIEWER_APP)
    }

    fn thread(resolved_by: Option<Actor>) -> Thread {
        Thread {
            path: "a.rs".to_owned(),
            author: Some(reviewer()),
            resolved: resolved_by.is_some(),
            resolved_by,
        }
    }

    fn human_thread(resolved_by: Option<Actor>) -> Thread {
        Thread { author: Some(Actor::user("maintainer")), ..thread(resolved_by) }
    }

    fn messages(fixed: &Fixed) -> Vec<String> {
        let found = check(fixed, 7, "b", REVIEWER_APP).unwrap();
        found.into_iter().map(|finding| finding.message).collect()
    }

    // Goal: the reviewer app's review on the head, with every thread resolved by it, passes.
    // The author's own review, a dismissed or earlier review, a later request for changes, an
    // open thread, a thread resolved by someone else, and a PR opened by the reviewer are
    // findings; a head that moved on from the commit judged is no verdict.
    #[test]
    fn review_rule_holds_on_the_head() {
        let good =
            fixed(vec![review(reviewer(), "b", "COMMENTED")], vec![thread(Some(reviewer()))]);
        assert_eq!(messages(&good), Vec::<String>::new());
        let own = fixed(vec![review(Actor::app(AUTHOR_APP), "b", "COMMENTED")], vec![]);
        assert!(messages(&own)[0].contains("has not reviewed the head commit b"));
        let dismissed = fixed(vec![review(reviewer(), "b", "DISMISSED")], vec![]);
        assert_eq!(messages(&dismissed).len(), 1);
        let earlier = fixed(vec![review(reviewer(), "a", "APPROVED")], vec![]);
        assert_eq!(messages(&earlier).len(), 1);
        let changes = vec![
            review(reviewer(), "b", "COMMENTED"),
            review(reviewer(), "b", "CHANGES_REQUESTED"),
        ];
        assert!(messages(&fixed(changes, vec![]))[0].contains("requests changes"));
        let open = fixed(
            vec![review(reviewer(), "b", "COMMENTED")],
            vec![thread(None), thread(Some(Actor::app(AUTHOR_APP)))],
        );
        let found = messages(&open);
        assert!(
            found[0].contains("is not resolved")
                && found[1].contains("resolved by nami-agent[bot], not by"),
            "{found:?}"
        );
        let by_reviewer = Fixed {
            author: reviewer(),
            ..fixed(vec![review(reviewer(), "b", "COMMENTED")], vec![])
        };
        assert!(messages(&by_reviewer)[0].contains("opened this pull request"));
        assert!(check(&good, 7, AUTHOR_APP.trim_end_matches("[bot]"), REVIEWER_APP).is_err());
        let moved = check(&good, 7, "a", REVIEWER_APP).unwrap_err();
        assert!(moved.contains("is now b, not the commit judged, a"), "{moved}");
        assert!(
            check(&good, 7, "b", AUTHOR_APP).is_err(),
            "the author's app is never the reviewer"
        );
        assert!(check(&good, 7, "b", "nami-reviewer").is_err(), "the reviewer is an app");
    }

    // Goal: a user account that took the reviewer app's name is not the reviewer: its review,
    // its threads and its resolutions are someone else's, and its PR is not the app's.
    #[test]
    fn a_user_named_like_the_reviewer_is_not_the_reviewer() {
        let impostor = Actor::user("nami-reviewer");
        let review_by_user = fixed(vec![review(impostor.clone(), "b", "COMMENTED")], vec![]);
        assert!(messages(&review_by_user)[0].contains("has not reviewed the head commit"));
        let threads = vec![thread(Some(impostor.clone())), human_thread(None)];
        let resolved_by_user = fixed(vec![review(reviewer(), "b", "COMMENTED")], threads);
        let found = messages(&resolved_by_user);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("resolved by nami-reviewer, not by"), "{found:?}");
        let opened_by_user = Fixed {
            author: impostor.clone(),
            ..fixed(vec![review(reviewer(), "b", "COMMENTED")], vec![])
        };
        assert_eq!(messages(&opened_by_user), Vec::<String>::new());
        let with_brackets = Actor { login: REVIEWER_APP.to_owned(), bot: false };
        assert!(!with_brackets.is_app(REVIEWER_APP) && !impostor.is_app(REVIEWER_APP));
        assert!(reviewer().is_app(REVIEWER_APP), "the app by type and login");
    }

    // Goal: reviews are read from the REST answer with their state and account type, skipping
    // deleted accounts; a login whose type disagrees with its `[bot]` is an error.
    #[test]
    fn reviews_are_read() {
        let text = "{\"user\":{\"login\":\"nami-reviewer[bot]\",\"type\":\"Bot\"},\
                    \"commit_id\":\"abc\",\"state\":\"COMMENTED\"}\n\
                    {\"user\":{\"login\":\"nami-reviewer\",\"type\":\"User\"},\
                    \"commit_id\":\"abc\",\"state\":\"APPROVED\"}\n\
                    {\"user\":null,\"commit_id\":\"def\"}\n";
        assert_eq!(
            reviews_from(text).unwrap(),
            [
                review(reviewer(), "abc", "COMMENTED"),
                review(Actor::user("nami-reviewer"), "abc", "APPROVED")
            ]
        );
        assert!(reviews_from("{\"user\":{\"login\":3}}\n").is_err(), "a login is a string");
        let disagree = "{\"user\":{\"login\":\"x[bot]\",\"type\":\"User\"}}\n";
        assert!(reviews_from(disagree).unwrap_err().contains("type disagrees"));
    }

    // Goal: threads are read with their author and who resolved them, as GitHub names them:
    // an author `Bot` with a bare login, a resolver `User` whose login ends in `[bot]`. An
    // answer with more pages, or of another shape, is an error.
    #[test]
    fn threads_are_read() {
        let answer = |more: &str| {
            json::parse(&format!(
                "{{\"data\":{{\"repository\":{{\"pullRequest\":{{\"reviewThreads\":{{\
                 \"pageInfo\":{{\"hasNextPage\":{more}}},\"nodes\":[\
                 {{\"isResolved\":true,\"path\":\"a.rs\",\"resolvedBy\":\
                 {{\"__typename\":\"User\",\"login\":\"nami-reviewer[bot]\"}},\
                 \"comments\":{{\"nodes\":[{{\"author\":\
                 {{\"__typename\":\"Bot\",\"login\":\"nami-reviewer\"}}}}]}}}},\
                 {{\"isResolved\":false,\"path\":\"b.rs\",\"resolvedBy\":null,\
                 \"comments\":{{\"nodes\":[\
                 {{\"author\":{{\"__typename\":\"User\",\"login\":\"nami-reviewer\"}}}}]}}}}]\
                 }}}}}}}}}}"
            ))
            .unwrap()
        };
        let found = threads_from(&answer("false")).unwrap();
        assert_eq!(found[0], thread(Some(reviewer())));
        assert_eq!(
            found[1],
            Thread {
                path: "b.rs".to_owned(),
                author: Some(Actor::user("nami-reviewer")),
                resolved: false,
                resolved_by: None
            }
        );
        assert!(threads_from(&answer("true")).unwrap_err().contains("more than 100"));
        assert!(threads_from(&json::parse("{\"data\":null}").unwrap()).is_err());
    }

    /// A stand-in for `gh api` that answers for pull request 7, or fails for any other.
    const FAKE_GH: &str = r#"#!/bin/sh
case "$*" in
    *pulls/7\ --jq*) echo "0123456789abcdef0123456789abcdef01234567 nami-agent[bot] Bot" ;;
    *pulls/7/reviews*)
        user='"user":{"login":"nami-reviewer[bot]","type":"Bot"}'
        echo "{$user,\"commit_id\":\"abc\",\"state\":\"COMMENTED\"}" ;;
    *n=7*)
        thread='{"isResolved":false,"path":"a.rs","resolvedBy":null,"comments":{"nodes":[]}}'
        threads="{\"pageInfo\":{\"hasNextPage\":false},\"nodes\":[$thread]}"
        echo "{\"data\":{\"repository\":{\"pullRequest\":{\"reviewThreads\":$threads}}}}" ;;
    *pulls/8\ --jq*) echo "not-a-commit someone User" ;;
    *pulls/9\ --jq*) echo "0123456789abcdef0123456789abcdef01234567 nami-agent[bot] User" ;;
    *) echo "HTTP 404: Not Found" >&2; exit 1 ;;
esac
"#;

    // Goal: `gh api` answers are read into the pull request, reviews and threads, and a failing
    // `gh`, an answer that is not a commit, or a type that disagrees with the login is an error,
    // never an empty list.
    #[test]
    fn gh_answers_are_read() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-fake-gh");
        scratch.write("gh", FAKE_GH);
        let program = scratch.root().join("gh");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let github = GhReviews::using(scratch.root(), &program);
        let pull = github.pull(7).unwrap();
        assert_eq!((pull.head.len(), pull.author), (40, Actor::app(AUTHOR_APP)));
        assert_eq!(github.reviews(7).unwrap(), [review(reviewer(), "abc", "COMMENTED")]);
        let threads = github.threads(7).unwrap();
        let open =
            Thread { path: "a.rs".to_owned(), author: None, resolved: false, resolved_by: None };
        assert_eq!(threads, [open]);
        assert!(github.pull(8).unwrap_err().contains("as the head commit of #8"));
        assert!(github.pull(9).unwrap_err().contains("of type `User`"));
        let failed = github.reviews(9).unwrap_err();
        assert!(failed.contains("HTTP 404") && failed.contains("GH_TOKEN"), "{failed}");
        assert!(github.threads(9).is_err(), "a failed query");
    }

    // Goal: only a code owner's approval of the head counts, by each owner's latest decision: not
    // a comment, not an approval of an earlier commit, not an approval later dismissed or
    // followed by a request for changes, not when another owner requests changes, and not an
    // approval by someone CODEOWNERS does not name. A comment after an approval keeps it, and a
    // head that moved on is no verdict.
    #[test]
    fn code_owner_approvals_are_found() {
        let codeowners = "/xtask/ @Owner @Other\n";
        let approved = |reviews: Vec<Review>| {
            owner_approved(&fixed(reviews, vec![]), 7, "b", codeowners).unwrap()
        };
        let owner = || Actor::user("owner");
        let other = || Actor::user("other");
        assert!(approved(vec![review(owner(), "b", "APPROVED")]), "logins ignore case");
        assert!(approved(vec![
            review(owner(), "b", "APPROVED"),
            review(owner(), "b", "COMMENTED")
        ]));
        assert!(!approved(vec![review(owner(), "b", "COMMENTED")]));
        assert!(!approved(vec![review(owner(), "a", "APPROVED")]));
        assert!(!approved(vec![review(owner(), "b", "DISMISSED")]), "a dismissed approval");
        let then_changes =
            vec![review(owner(), "b", "APPROVED"), review(owner(), "b", "CHANGES_REQUESTED")];
        assert!(!approved(then_changes));
        let then_old = vec![review(owner(), "b", "APPROVED"), review(owner(), "a", "APPROVED")];
        assert!(!approved(then_old), "the latest decision counts");
        let blocked =
            vec![review(owner(), "b", "APPROVED"), review(other(), "a", "CHANGES_REQUESTED")];
        assert!(!approved(blocked), "another owner's request for changes blocks");
        assert!(!approved(vec![review(reviewer(), "b", "APPROVED")]));
        let named_like_owner = Actor { login: "owner".to_owned(), bot: true };
        assert!(!approved(vec![review(named_like_owner, "b", "APPROVED")]), "apps own nothing");
        assert!(!approved(vec![]));
        let moved = owner_approved(&fixed(vec![], vec![]), 7, "a", codeowners).unwrap_err();
        assert!(moved.contains("not the commit judged"), "{moved}");
    }

    // Goal: only the reviewer app's own threads count: someone else's thread is left to the
    // ruleset whether open, resolved by its author, or resolved by the reviewer app.
    #[test]
    fn only_the_reviewer_s_own_threads_count() {
        let with =
            |threads: Vec<Thread>| fixed(vec![review(reviewer(), "b", "COMMENTED")], threads);
        assert_eq!(messages(&with(vec![human_thread(None)])), Vec::<String>::new());
        let by_author = human_thread(Some(Actor::user("maintainer")));
        assert_eq!(messages(&with(vec![by_author])), Vec::<String>::new());
        let by_reviewer = human_thread(Some(reviewer()));
        assert_eq!(messages(&with(vec![by_reviewer])), Vec::<String>::new());
        let deleted = Thread { author: None, ..thread(None) };
        assert_eq!(messages(&with(vec![deleted])), Vec::<String>::new(), "no author, not its own");
        assert_eq!(messages(&with(vec![thread(None)])).len(), 1, "its own open thread");
        let gone = Thread { resolved: true, ..thread(None) };
        assert!(messages(&with(vec![gone]))[0].contains("by a deleted account"));
    }
}
