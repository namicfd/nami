//! The independent review has run on the pull request, and everything raised is resolved by
//! the reviewer (ADR-0008, ADR-0022).
//!
//! The rule, for every rule-path PR: no request for changes by the reviewer app stands, the bot
//! that the repository variable `NAMI_REVIEWER_LOGIN` names (such as `nami-reviewer[bot]`, a
//! GitHub App separate from the agents' `nami-agent[bot]`); the PR was not opened by that app;
//! and every review thread that the app opened is resolved, by that app. The ratchet counts
//! only the app's own threads. Everyone else's threads, resolved or not and by whomever, are
//! left to the ruleset, which requires every conversation resolved before merge, and are never
//! a ratchet finding.
//!
//! An additive diff ([`super::class`]) merges on the ratchet's proof alone, so it needs no
//! review by the app. An edit to existing rule code needs the app's latest decision to be an
//! approval of the head commit ([`check`]). A review of an earlier commit does not
//! count, not even when the later commits are fixes: the reviewer re-reviews the new head.
//! Until the app exists (#59), a code owner's approval of the head stands in for an edit, as
//! for everything else the ratchet cannot prove.
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

use super::{Check, Finding, Step, Verdict};
use crate::issue::github::parse_lines;
use crate::json::{self, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The agents' own app, which writes the pull requests and can never be the reviewer.
pub const AUTHOR_APP: &str = "nami-agent[bot]";

/// The reviewer app's login, as the runbook names it; CI reads the one in use from
/// `NAMI_REVIEWER_LOGIN`.
pub const REVIEWER_APP: &str = "nami-reviewer[bot]";

/// Most review threads one page of [`THREADS_QUERY`] holds.
const THREADS_PER_PAGE: u32 = 100;

/// Most pages of review threads the gate reads: 5000 threads with [`THREADS_PER_PAGE`], far
/// above any pull request's. A pull request with more fails the gate.
const THREAD_PAGES_MAX: u32 = 50;

/// The state of a review that no longer counts.
const DISMISSED: &str = "DISMISSED";

/// The state of a review that asks for changes.
const CHANGES_REQUESTED: &str = "CHANGES_REQUESTED";

/// The state of a review that leaves the reviewer's decision as it was.
const COMMENTED: &str = "COMMENTED";

/// The state of an approving review.
const APPROVED: &str = "APPROVED";

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
    /// GitHub's id of the review.
    pub id: u64,
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

/// One page of review threads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadPage {
    /// The page's threads.
    pub threads: Vec<Thread>,
    /// The cursor of the next page; `None` on the last.
    pub next: Option<String>,
}

impl ThreadPage {
    /// All of `threads`, as the only page.
    #[must_use]
    pub fn last(threads: Vec<Thread>) -> Self {
        Self { threads, next: None }
    }
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

    /// One page of the review threads of the pull request: the first with `after` `None`, and
    /// then the one after the cursor `after` that the page before gave.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked.
    fn threads(&self, pull: u32, after: Option<&str>) -> Result<ThreadPage, String>;

    /// The state each dismissed review had before it was dismissed, by review id, in upper
    /// case as reviews give it.
    ///
    /// # Errors
    ///
    /// When GitHub cannot be asked.
    fn dismissed(&self, pull: u32) -> Result<BTreeMap<u64, String>, String>;
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

/// Whether the reviewer's requests for changes stand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Objection {
    /// None, or the reviewer withdrew it with a later review.
    Withdrawn,
    /// The reviewer asked for changes, and nothing since withdraws it.
    Standing,
    /// Someone dismissed the reviewer's request for changes. Any writer can, the authoring
    /// app included, so only a later approval by the reviewer withdraws it.
    Dismissed,
}

/// Whether the reviewer's requests for changes stand, from its reviews in order. A review
/// that was dismissed counts by the state it had, from `dismissed`; one whose state is not
/// known counts as a dismissed request for changes.
fn objection(reviews: &[&Review], dismissed: &BTreeMap<u64, String>) -> Objection {
    let mut objection = Objection::Withdrawn;
    for review in reviews {
        let was = dismissed.get(&review.id).map(String::as_str);
        objection = match (review.state.as_str(), objection) {
            (CHANGES_REQUESTED, _) => Objection::Standing,
            (DISMISSED, _) if was.is_none_or(|state| state == CHANGES_REQUESTED) => {
                Objection::Dismissed
            }
            (APPROVED, _) | (COMMENTED, Objection::Standing) => Objection::Withdrawn,
            (_, unchanged) => unchanged,
        };
    }
    objection
}

/// What is wrong with the threads that the reviewer opened: one that is open, or that someone
/// else resolved. Everyone else's threads are left to the ruleset.
fn thread_problems(threads: &[Thread], reviewer: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for thread in threads {
        let opened_by_reviewer =
            thread.author.as_ref().is_some_and(|author| author.is_app(reviewer));
        let resolver = thread.resolved_by.as_ref().map(ToString::to_string);
        if !opened_by_reviewer {
            continue;
        }
        if !thread.resolved {
            problems.push(format!(
                "a review thread by {reviewer} on {} is not resolved; fix it, or answer why the \
                 finding is wrong, and have the reviewer resolve it (ADR-0008)",
                thread.path
            ));
        } else if !thread.resolved_by.as_ref().is_some_and(|by| by.is_app(reviewer)) {
            problems.push(format!(
                "a review thread on {} was resolved by {}, not by {reviewer}; only the reviewer \
                 resolves its threads for the ratchet (ADR-0022)",
                thread.path,
                resolver.unwrap_or_else(|| "a deleted account".to_owned())
            ));
        }
    }
    assert!(problems.len() <= threads.len(), "at most one problem per thread");
    problems
}

/// The step [`Step::ReviewGate`]: applies the review rule to pull request `pull`, whose head
/// must still be `judged`, for the reviewer app `reviewer`, into `verdict`. It records the
/// finding that the reviewer opened the PR, then those of its requests for changes, then those
/// of its threads, page by page, each as soon as it is read. Whatever cannot be read fails the
/// step without dropping a finding read before or after it (review of #57, round 13): GitHub
/// cannot be asked, the head has moved on from `judged`, or `reviewer` is not an app other than
/// the agents' own. It records the app's approval, which an edit needs, only when the
/// reviewer's latest decision approves the head and the gate read everything and found nothing.
pub fn check(
    reviews: &dyn Reviews,
    pull: u32,
    judged: &str,
    reviewer: &str,
    verdict: &mut Verdict,
) {
    assert!(pull > 0, "pull requests are numbered from 1");
    if !reviewer.ends_with("[bot]") || reviewer == AUTHOR_APP {
        return verdict.fail(
            Step::ReviewGate,
            format!(
                "the reviewer `{reviewer}` must be a GitHub App other than {AUTHOR_APP}, named \
                 with `[bot]`"
            ),
        );
    }
    let (found, failed) = (verdict.findings.len(), verdict.failed.len());
    let app = Actor::app(reviewer);
    let place = format!("pull request #{pull}");
    verdict.ran(Check::Review);
    match reviews.pull(pull) {
        Ok(request) => {
            if request.author == app {
                let message = format!(
                    "{reviewer} opened this pull request, so it cannot review it (ADR-0022)"
                );
                verdict.record(Finding::new(&place, Check::Review, message));
            }
            if let Err(reason) = same_head(&request, judged, pull) {
                verdict.fail(Step::ReviewGate, reason);
            }
        }
        Err(reason) => verdict.fail(Step::ReviewGate, reason),
    }
    let decided = decisions(reviews, pull, judged, &app, verdict);
    thread_findings(reviews, pull, reviewer, verdict);
    let Some((reviewed, approved)) = decided else {
        return;
    };
    verdict.notes.push(format!(
        "{reviewer} {} the head commit.",
        match (approved, reviewed) {
            (true, _) => "approved",
            (false, true) => "reviewed, without approving,",
            (false, false) => "has not reviewed",
        }
    ));
    let clean = verdict.findings.len() == found && verdict.failed.len() == failed;
    verdict.progress.app_approved = approved && clean;
}

/// The reviewer's requests for changes on pull request `pull`, as findings into `verdict`, and
/// whether it reviewed the commit `judged` and its latest decision approves it; `None` if the
/// reviews cannot be read, which fails [`Step::ReviewGate`].
fn decisions(
    reviews: &dyn Reviews,
    pull: u32,
    judged: &str,
    app: &Actor,
    verdict: &mut Verdict,
) -> Option<(bool, bool)> {
    let all = match reviews.reviews(pull) {
        Ok(all) => all,
        Err(reason) => {
            verdict.fail(Step::ReviewGate, reason);
            return None;
        }
    };
    let own: Vec<&Review> = all.iter().filter(|review| review.author == *app).collect();
    let reviewed_head = own.iter().any(|review| {
        review.commit == judged && (review.state == COMMENTED || review.state == APPROVED)
    });
    let approved = latest_decision(&all, app)
        .is_some_and(|decision| decision.state == APPROVED && decision.commit == judged);
    assert!(reviewed_head || !approved, "an approval of the head is a review of it");
    // Without the dismissals, a standing request for changes still stands: the dismissals only
    // tell what a dismissed review was, and a standing one has none after it. A dismissed one
    // is then not known to be a request for changes, so only the failure says it.
    let dismissed = reviews.dismissed(pull);
    let none = BTreeMap::new();
    let place = format!("pull request #{pull}");
    let reviewer = &app.login;
    match (objection(&own, dismissed.as_ref().unwrap_or(&none)), &dismissed) {
        (Objection::Standing, _) => {
            let message = format!(
                "{reviewer} requests changes on this pull request; fix them, and have it review \
                 the head again (ADR-0008)"
            );
            verdict.record(Finding::new(&place, Check::Review, message));
        }
        (Objection::Dismissed, Ok(_)) => {
            let message = format!(
                "{reviewer}'s request for changes was dismissed, which does not withdraw it; only \
                 a later approval by {reviewer} does (ADR-0022)"
            );
            verdict.record(Finding::new(&place, Check::Review, message));
        }
        (Objection::Dismissed, Err(_)) | (Objection::Withdrawn, _) => {}
    }
    if let Err(reason) = dismissed {
        verdict.fail(Step::ReviewGate, reason);
    }
    Some((reviewed_head, approved))
}

/// The findings of the reviewer's threads on pull request `pull`, into `verdict`, page by page,
/// each page's as soon as it is read. A page that cannot be read, or more than
/// [`THREAD_PAGES_MAX`] pages, fails [`Step::ReviewGate`].
fn thread_findings(reviews: &dyn Reviews, pull: u32, reviewer: &str, verdict: &mut Verdict) {
    let place = format!("pull request #{pull}");
    let mut after: Option<String> = None;
    for _page in 0..THREAD_PAGES_MAX {
        let page = match reviews.threads(pull, after.as_deref()) {
            Ok(page) => page,
            Err(reason) => return verdict.fail(Step::ReviewGate, reason),
        };
        for problem in thread_problems(&page.threads, reviewer) {
            verdict.record(Finding::new(&place, Check::Review, problem));
        }
        match page.next {
            Some(next) if after.as_deref() == Some(next.as_str()) => {
                let reason = format!("GitHub gave the thread cursor `{next}` twice");
                return verdict.fail(Step::ReviewGate, reason);
            }
            Some(next) => after = Some(next),
            None => return,
        }
    }
    let most = THREADS_PER_PAGE * THREAD_PAGES_MAX;
    verdict.fail(
        Step::ReviewGate,
        format!(
            "the pull request has more than {most} review threads, more than the ratchet reads; \
             resolve them, or open a new pull request"
        ),
    );
}

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
        let id = match value.get("id") {
            Some(Value::Number(digits)) => digits.parse::<u64>().ok(),
            _ => None,
        };
        found.push(Review {
            id: id.ok_or("GitHub sent a review without an id")?,
            author: Actor { login: login.to_owned(), bot },
            commit: field("commit_id"),
            state: field("state"),
        });
    }
    Ok(found)
}

/// Reads the earlier states of dismissed reviews from `gh api .../events --jq` output: one
/// `<review id> <state>` line per `review_dismissed` event.
///
/// # Errors
///
/// Returns what is wrong if a line has another form.
pub fn dismissed_from(text: &str) -> Result<BTreeMap<u64, String>, String> {
    let mut found = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let parsed = line
            .split_once(' ')
            .and_then(|(id, state)| Some((id.parse::<u64>().ok()?, state.trim().to_uppercase())));
        let (id, state) = parsed.ok_or_else(|| format!("GitHub sent `{line}` as a dismissal"))?;
        let _replaced = found.insert(id, state);
    }
    Ok(found)
}

/// Reads one page of threads from the GraphQL answer of [`THREADS_QUERY`].
///
/// # Errors
///
/// Returns what is missing if the answer has another shape, or if it says there are more pages
/// and gives no cursor.
pub fn threads_from(value: &Value) -> Result<ThreadPage, String> {
    let threads = ["data", "repository", "pullRequest", "reviewThreads"]
        .iter()
        .try_fold(value, |inner, key| inner.get(key))
        .ok_or("GitHub sent no review threads; check the pull request number")?;
    let info = threads.get("pageInfo");
    let next = match info.and_then(|info| info.get("hasNextPage")) {
        Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some(
            info.and_then(|info| info.get("endCursor"))
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .ok_or("GitHub says there are more review threads, and gives no cursor")?
                .to_owned(),
        ),
        _ => return Err("GitHub sent review threads without `hasNextPage`".to_owned()),
    };
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
    assert!(found.len() <= nodes.len(), "one thread per node");
    Ok(ThreadPage { threads: found, next })
}

/// The GraphQL query for one page of [`THREADS_PER_PAGE`] review threads of a pull request,
/// after the cursor `$c`, or the first page without it.
const THREADS_QUERY: &str = "query($o:String!,$r:String!,$n:Int!,$c:String){repository(owner:$o,\
     name:$r){pullRequest(number:$n){reviewThreads(first:100,after:$c){pageInfo{hasNextPage \
     endCursor}nodes{isResolved path resolvedBy{__typename login} comments(first:1){nodes{author\
     {__typename login}}}}}}}}";

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

    fn dismissed(&self, pull: u32) -> Result<BTreeMap<u64, String>, String> {
        let path = format!("repos/{{owner}}/{{repo}}/issues/{pull}/events?per_page=100");
        let jq = ".[] | select(.event == \"review_dismissed\") | \
                  \"\\(.dismissed_review.review_id) \\(.dismissed_review.state)\"";
        dismissed_from(&self.api(&["--paginate", &path, "--jq", jq])?)
    }

    fn threads(&self, pull: u32, after: Option<&str>) -> Result<ThreadPage, String> {
        let query = format!("query={THREADS_QUERY}");
        let number = format!("n={pull}");
        let cursor = after.map(|after| format!("c={after}"));
        let mut arguments =
            vec!["graphql", "-f", &query, "-F", "o={owner}", "-F", "r={repo}", "-F", &number];
        if let Some(cursor) = &cursor {
            arguments.extend(["-f", cursor]);
        }
        let text = self.api(&arguments)?;
        let value = json::parse(&text)
            .map_err(|error| format!("gh printed invalid JSON: {}", error.message))?;
        threads_from(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::{AUTHOR_APP, Actor, GhReviews, Pull, REVIEWER_APP, Review, Reviews, Thread};
    use super::{THREADS_PER_PAGE, THREADS_QUERY, ThreadPage, Verdict};
    use super::{check, dismissed_from, owner_approved, reviews_from, threads_from};
    use crate::json;
    use std::collections::BTreeMap;

    struct Fixed {
        author: Actor,
        reviews: Vec<Review>,
        threads: Vec<Thread>,
        dismissed: BTreeMap<u64, String>,
    }

    impl Reviews for Fixed {
        fn pull(&self, _pull: u32) -> Result<Pull, String> {
            Ok(Pull { head: "b".to_owned(), author: self.author.clone() })
        }
        fn reviews(&self, _pull: u32) -> Result<Vec<Review>, String> {
            Ok(self.reviews.clone())
        }
        fn threads(&self, _pull: u32, after: Option<&str>) -> Result<ThreadPage, String> {
            // Pages of a hundred, as GitHub's, with the page number as the cursor.
            let size = usize::try_from(THREADS_PER_PAGE).expect("a page size fits");
            let index = after.map_or(0, |cursor| cursor.parse::<usize>().expect("a cursor"));
            let threads = self.threads.chunks(size).nth(index).unwrap_or_default().to_vec();
            let next = ((index + 1) * size < self.threads.len()).then(|| (index + 1).to_string());
            Ok(ThreadPage { threads, next })
        }
        fn dismissed(&self, _pull: u32) -> Result<BTreeMap<u64, String>, String> {
            Ok(self.dismissed.clone())
        }
    }

    /// The review gate on pull request 7 at `judged`, for `reviewer`, into a verdict of its own.
    fn gate(fixed: &dyn Reviews, judged: &str, reviewer: &str) -> Verdict {
        let mut verdict = Verdict::default();
        check(fixed, 7, judged, reviewer, &mut verdict);
        verdict
    }

    /// True if the gate gave no verdict.
    fn fails(fixed: &dyn Reviews, judged: &str, reviewer: &str) -> bool {
        !gate(fixed, judged, reviewer).failed.is_empty()
    }

    fn fixed(reviews: Vec<Review>, threads: Vec<Thread>) -> Fixed {
        Fixed { author: Actor::app(AUTHOR_APP), reviews, threads, dismissed: BTreeMap::new() }
    }

    fn review(author: Actor, commit: &str, state: &str) -> Review {
        Review { id: 0, author, commit: commit.to_owned(), state: state.to_owned() }
    }

    fn numbered(id: u64, state: &str) -> Review {
        Review { id, ..review(reviewer(), "b", state) }
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

    /// The findings' messages, then `not reviewed` if the reviewer did not review the head.
    fn messages(fixed: &Fixed) -> Vec<String> {
        let verdict = gate(fixed, "b", REVIEWER_APP);
        assert!(verdict.failed.is_empty(), "{:?}", verdict.failed);
        let unreviewed =
            verdict.notes.iter().any(|note| note.ends_with("has not reviewed the head commit."));
        let findings = verdict.findings.into_iter().map(|finding| finding.message);
        findings.chain(unreviewed.then(|| "not reviewed".to_owned())).collect()
    }

    // Goal: the reviewer app's review on the head, with every thread resolved by it, passes.
    // A later request for changes, an open thread, a thread resolved by someone else, and a PR
    // opened by the reviewer are findings; no review of the head by the app (the author's own,
    // a dismissed or an earlier one) is no finding, only shown as such; a head that moved on is
    // no verdict.
    #[test]
    fn review_rule_holds_on_the_head() {
        let good =
            fixed(vec![review(reviewer(), "b", "COMMENTED")], vec![thread(Some(reviewer()))]);
        assert_eq!(messages(&good), Vec::<String>::new());
        let own = fixed(vec![review(Actor::app(AUTHOR_APP), "b", "COMMENTED")], vec![]);
        let unreviewed = gate(&own, "b", REVIEWER_APP);
        assert!(unreviewed.findings.is_empty(), "a missing review is no finding");
        assert!(!unreviewed.progress.app_approved, "{unreviewed:?}");
        assert_eq!(messages(&own), ["not reviewed"]);
        let dismissed = messages(&fixed(vec![review(reviewer(), "b", "DISMISSED")], vec![]));
        assert!(dismissed[0].contains("was dismissed") && dismissed.len() == 2, "{dismissed:?}");
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
        assert!(fails(&good, AUTHOR_APP.trim_end_matches("[bot]"), REVIEWER_APP));
        let moved = gate(&good, "a", REVIEWER_APP).failed.join("; ");
        assert!(moved.contains("review gate: the head of #7 is now b, not the commit judged, a"));
        assert!(fails(&good, "b", AUTHOR_APP), "the author's app is never the reviewer");
        assert!(fails(&good, "b", "nami-reviewer"), "the reviewer is an app");
    }

    // Goal: the gate shows an approval, which an edit to existing rule code needs, only when the
    // reviewer's latest decision approves the head: not a comment, not an approval of an earlier
    // commit, not one that a later request for changes or a dismissal replaced, and not an
    // approval by someone else; a later comment leaves the approval standing.
    #[test]
    fn an_approval_counts_only_as_the_latest_decision_on_the_head() {
        let approved = |reviews: Vec<Review>| {
            gate(&fixed(reviews, vec![]), "b", REVIEWER_APP).progress.app_approved
        };
        let head = review(reviewer(), "b", "APPROVED");
        assert!(approved(vec![head.clone()]));
        let commented = vec![head.clone(), review(reviewer(), "b", "COMMENTED")];
        assert!(approved(commented), "a comment decides nothing");
        let refused = [
            vec![review(reviewer(), "b", "COMMENTED")],
            vec![review(reviewer(), "a", "APPROVED"), review(reviewer(), "b", "COMMENTED")],
            vec![head.clone(), review(reviewer(), "b", "CHANGES_REQUESTED")],
            vec![review(reviewer(), "b", "DISMISSED")],
            vec![review(Actor::user("owner"), "b", "APPROVED")],
        ];
        for (index, reviews) in refused.into_iter().enumerate() {
            assert!(!approved(reviews), "case {index}");
        }
        let with_thread = fixed(vec![head.clone()], vec![thread(None)]);
        assert!(!gate(&with_thread, "b", REVIEWER_APP).progress.app_approved, "an open thread");
        let moved =
            gate(&fixed(vec![review(reviewer(), "a", "APPROVED")], vec![]), "a", REVIEWER_APP);
        assert!(!moved.progress.app_approved, "a head that moved on");
    }

    /// Reviews whose dismissals and second page of threads cannot be read, nor the reviews
    /// unless the flag says so.
    struct Failing(Fixed, bool);

    impl Reviews for Failing {
        fn pull(&self, pull: u32) -> Result<Pull, String> {
            self.0.pull(pull)
        }
        fn reviews(&self, pull: u32) -> Result<Vec<Review>, String> {
            if self.1 { self.0.reviews(pull) } else { Err("no reviews".to_owned()) }
        }
        fn threads(&self, pull: u32, after: Option<&str>) -> Result<ThreadPage, String> {
            if after.is_some() {
                Err("no second page".to_owned())
            } else {
                self.0.threads(pull, after)
            }
        }
        fn dismissed(&self, _pull: u32) -> Result<BTreeMap<u64, String>, String> {
            Err("no dismissals".to_owned())
        }
    }

    // Goal: the gate records each finding as it reads it, and what it cannot read fails it
    // without dropping another (review of #57, round 13): a PR opened by the reviewer with
    // unreadable reviews, a standing request for changes with unreadable dismissals, and an open
    // thread on the first page with a second page that cannot be read are all findings. Threads
    // are read page by page past the first hundred, and a dismissed review of unknown state
    // without the dismissals is no finding, only the failure.
    #[test]
    fn the_gate_records_what_it_read_before_a_failure() {
        let resolved = (0..THREADS_PER_PAGE).map(|_| thread(Some(reviewer())));
        let threads: Vec<Thread> = [thread(None)].into_iter().chain(resolved).collect();
        let opened = Fixed { author: reviewer(), ..fixed(vec![], threads) };
        let broken = gate(&Failing(opened, false), "b", REVIEWER_APP);
        let found: Vec<&str> =
            broken.findings.iter().map(|finding| finding.message.as_str()).collect();
        assert!(found.len() == 2 && found[0].contains("opened this pull request"), "{found:?}");
        assert!(found[1].contains("is not resolved"), "{found:?}");
        assert_eq!(broken.failed, ["review gate: no reviews", "review gate: no second page"]);
        let many: Vec<Thread> = (0..=THREADS_PER_PAGE)
            .map(|_| thread(Some(reviewer())))
            .chain([thread(None)])
            .collect();
        let paged =
            gate(&fixed(vec![review(reviewer(), "b", "COMMENTED")], many), "b", REVIEWER_APP);
        assert!(paged.failed.is_empty() && paged.findings.len() == 1, "{paged:?}");
        let standing = vec![numbered(1, "DISMISSED"), numbered(2, "CHANGES_REQUESTED")];
        let stands = gate(&Failing(fixed(standing, vec![]), true), "b", REVIEWER_APP);
        assert!(stands.findings[0].message.contains("requests changes"), "{stands:?}");
        assert_eq!(stands.failed, ["review gate: no dismissals"]);
        let unknown =
            gate(&Failing(fixed(vec![numbered(1, "DISMISSED")], vec![]), true), "b", REVIEWER_APP);
        assert!(unknown.findings.is_empty() && unknown.failed.len() == 1, "{unknown:?}");
        assert!(THREADS_QUERY.contains(&format!("first:{THREADS_PER_PAGE},")), "the page size");
    }

    // Goal: a user account that took the reviewer app's name is not the reviewer: its review,
    // its threads and its resolutions are someone else's, and its PR is not the app's.
    #[test]
    fn a_user_named_like_the_reviewer_is_not_the_reviewer() {
        let impostor = Actor::user("nami-reviewer");
        let review_by_user = fixed(vec![review(impostor.clone(), "b", "COMMENTED")], vec![]);
        assert_eq!(messages(&review_by_user), ["not reviewed"]);
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

    // Goal: the reviewer's request for changes stands until it withdraws it: a later review of
    // its own withdraws a standing one, but a dismissed one, which any writer can dismiss,
    // stands until the reviewer approves. A dismissed review of unknown state counts as a
    // dismissed request for changes; a dismissed approval is just not a review.
    #[test]
    fn requests_for_changes_stand_until_the_reviewer_withdraws_them() {
        let with = |reviews: Vec<Review>, states: &[(u64, &str)]| {
            let dismissed = states.iter().map(|(id, state)| (*id, (*state).to_owned())).collect();
            messages(&Fixed { dismissed, ..fixed(reviews, vec![]) })
        };
        let withdrawn = with(vec![numbered(1, "CHANGES_REQUESTED"), numbered(2, "COMMENTED")], &[]);
        assert_eq!(withdrawn, Vec::<String>::new());
        let dismissed = [(1, "CHANGES_REQUESTED")];
        let stands = with(vec![numbered(1, "DISMISSED"), numbered(2, "COMMENTED")], &dismissed);
        assert!(stands.len() == 1 && stands[0].contains("was dismissed"), "{stands:?}");
        let approved = with(vec![numbered(1, "DISMISSED"), numbered(2, "APPROVED")], &dismissed);
        assert_eq!(approved, Vec::<String>::new(), "a later approval withdraws it");
        let unknown = with(vec![numbered(1, "DISMISSED"), numbered(2, "COMMENTED")], &[]);
        assert!(unknown[0].contains("was dismissed"), "an unknown state fails closed");
        let stale = [(1, "APPROVED")];
        let after = with(vec![numbered(1, "DISMISSED"), numbered(2, "COMMENTED")], &stale);
        assert_eq!(after, Vec::<String>::new(), "a dismissed approval is no objection");
        let standing = with(vec![numbered(1, "COMMENTED"), numbered(2, "CHANGES_REQUESTED")], &[]);
        assert!(standing[0].contains("requests changes"), "{standing:?}");
    }

    // Goal: dismissals are read as `<review id> <state>` lines, in upper case.
    #[test]
    fn dismissals_are_read() {
        let found = dismissed_from("5 changes_requested\n\n6 approved\n").unwrap();
        assert_eq!(found.get(&5).map(String::as_str), Some("CHANGES_REQUESTED"));
        assert_eq!(found.get(&6).map(String::as_str), Some("APPROVED"));
        assert!(dismissed_from("x approved\n").is_err());
    }

    // Goal: reviews are read from the REST answer with their id, state and account type,
    // skipping deleted accounts; a login whose type disagrees with its `[bot]` is an error.
    #[test]
    fn reviews_are_read() {
        let text = "{\"id\":1,\"user\":{\"login\":\"nami-reviewer[bot]\",\"type\":\"Bot\"},\
                    \"commit_id\":\"abc\",\"state\":\"COMMENTED\"}\n\
                    {\"id\":2,\"user\":{\"login\":\"nami-reviewer\",\"type\":\"User\"},\
                    \"commit_id\":\"abc\",\"state\":\"APPROVED\"}\n\
                    {\"user\":null,\"commit_id\":\"def\"}\n";
        assert_eq!(
            reviews_from(text).unwrap(),
            [
                Review { id: 1, ..review(reviewer(), "abc", "COMMENTED") },
                Review { id: 2, ..review(Actor::user("nami-reviewer"), "abc", "APPROVED") }
            ]
        );
        assert!(reviews_from("{\"user\":{\"login\":3}}\n").is_err(), "a login is a string");
        let disagree = "{\"user\":{\"login\":\"x[bot]\",\"type\":\"User\"}}\n";
        assert!(reviews_from(disagree).unwrap_err().contains("type disagrees"));
    }

    // Goal: threads are read with their author and who resolved them, as GitHub names them:
    // an author `Bot` with a bare login, a resolver `User` whose login ends in `[bot]`. An
    // answer with more pages gives the cursor of the next; one that says so without a cursor,
    // or of another shape, is an error.
    #[test]
    fn threads_are_read() {
        let answer = |info: &str| {
            json::parse(&format!(
                "{{\"data\":{{\"repository\":{{\"pullRequest\":{{\"reviewThreads\":{{\
                 \"pageInfo\":{{{info}}},\"nodes\":[\
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
        let page = threads_from(&answer("\"hasNextPage\":false")).unwrap();
        let found = &page.threads;
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
        assert_eq!(page.next, None, "the last page");
        let more = threads_from(&answer("\"hasNextPage\":true,\"endCursor\":\"Y3Vy\"")).unwrap();
        assert_eq!((more.threads.len(), more.next.as_deref()), (2, Some("Y3Vy")));
        let lost = threads_from(&answer("\"hasNextPage\":true")).unwrap_err();
        assert!(lost.contains("gives no cursor"), "{lost}");
        assert!(threads_from(&answer("")).unwrap_err().contains("without `hasNextPage`"));
        assert!(threads_from(&json::parse("{\"data\":null}").unwrap()).is_err());
    }

    /// A stand-in for `gh api` that answers for pull request 7, or fails for any other.
    const FAKE_GH: &str = r#"#!/bin/sh
case "$*" in
    *pulls/7\ --jq*) echo "0123456789abcdef0123456789abcdef01234567 nami-agent[bot] Bot" ;;
    *pulls/7/reviews*)
        user='"user":{"login":"nami-reviewer[bot]","type":"Bot"}'
        echo "{\"id\":3,$user,\"commit_id\":\"abc\",\"state\":\"COMMENTED\"}" ;;
    *issues/7/events*) echo "3 changes_requested" ;;
    *n=7\ -f\ c=UDI*)
        thread='{"isResolved":false,"path":"b.rs","resolvedBy":null,"comments":{"nodes":[]}}'
        threads="{\"pageInfo\":{\"hasNextPage\":false},\"nodes\":[$thread]}"
        echo "{\"data\":{\"repository\":{\"pullRequest\":{\"reviewThreads\":$threads}}}}" ;;
    *n=7)
        thread='{"isResolved":false,"path":"a.rs","resolvedBy":null,"comments":{"nodes":[]}}'
        threads="{\"pageInfo\":{\"hasNextPage\":true,\"endCursor\":\"UDI\"},\"nodes\":[$thread]}"
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
        let comment = Review { id: 3, ..review(reviewer(), "abc", "COMMENTED") };
        assert_eq!(github.reviews(7).unwrap(), [comment]);
        assert_eq!(
            github.dismissed(7).unwrap().get(&3).map(String::as_str),
            Some("CHANGES_REQUESTED")
        );
        let first = github.threads(7, None).unwrap();
        let open = |path: &str| Thread {
            path: path.to_owned(),
            author: None,
            resolved: false,
            resolved_by: None,
        };
        assert_eq!(first, ThreadPage { threads: vec![open("a.rs")], next: Some("UDI".to_owned()) });
        let second = github.threads(7, first.next.as_deref()).unwrap();
        assert_eq!(second, ThreadPage::last(vec![open("b.rs")]), "the page after the cursor");
        assert!(github.pull(8).unwrap_err().contains("as the head commit of #8"));
        assert!(github.pull(9).unwrap_err().contains("of type `User`"));
        let failed = github.reviews(9).unwrap_err();
        assert!(failed.contains("HTTP 404") && failed.contains("GH_TOKEN"), "{failed}");
        assert!(github.threads(9, None).is_err(), "a failed query");
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
