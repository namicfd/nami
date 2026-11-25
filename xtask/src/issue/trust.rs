//! Which issues agents act on (ADR-0021).
//!
//! The repository is public, so anyone can open an issue or comment on one. Agents believe only
//! the bot and the users that `.github/CODEOWNERS` names on `main`. Trust comes from who wrote
//! the text, never from a label: every agent holds the bot's key, and the bot can apply labels.

use super::github::{Comment, Issue, LabelEvent};
use std::collections::BTreeSet;
use std::path::Path;

/// The GitHub App that agents act as. A user name cannot contain `[`, so no user can take it.
pub const BOT_LOGIN: &str = "nami-agent[bot]";

/// The kind labels. An issue that agents work on has exactly one of them.
pub const KINDS: [&str; 3] = ["guardrail-gap", "bug", "docs"];

/// The label of an issue that waits for the maintainer. Only a code owner removes it.
pub const NEEDS_DECISION: &str = "needs-decision";

/// The file that names the code owners, relative to the repository root.
const CODEOWNERS_FILE: &str = ".github/CODEOWNERS";

/// The authors that agents believe: the bot, and the code owners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    /// Code owners' logins, in lowercase: GitHub logins ignore case.
    owners: BTreeSet<String>,
}

impl Trust {
    /// The users that a CODEOWNERS file names. A team (`@org/team`) and an e-mail address
    /// trust nobody: expanding them would need GitHub, and failing closed is safe.
    #[must_use]
    pub fn from_codeowners(text: &str) -> Self {
        let owners: BTreeSet<String> = text
            .lines()
            .flat_map(|line| {
                let words = line.split_whitespace().take_while(|word| !word.starts_with('#'));
                // The first word is the pattern; the owners follow it.
                words.skip(1)
            })
            .filter_map(|word| word.strip_prefix('@'))
            .filter(|login| !login.is_empty() && !login.contains('/'))
            .map(str::to_ascii_lowercase)
            .collect();
        assert!(owners.iter().all(|owner| !owner.contains('@')), "owners are bare logins");
        Self { owners }
    }

    /// True for the bot and for a code owner.
    #[must_use]
    pub fn is_trusted(&self, login: &str) -> bool {
        login == BOT_LOGIN || self.is_owner(login)
    }

    /// True for a code owner: a human who decides. The bot is never one.
    #[must_use]
    pub fn is_owner(&self, login: &str) -> bool {
        self.owners.contains(&login.to_ascii_lowercase())
    }
}

/// The trust list from `.github/CODEOWNERS` at `reference`, such as `origin/main`.
///
/// # Errors
///
/// When git cannot show the file at that reference.
pub fn load(root: &Path, reference: &str) -> Result<Trust, String> {
    let object = format!("{reference}:{CODEOWNERS_FILE}");
    let text = crate::task::claims::git(root, &["show", &object], &[], None).map_err(|error| {
        format!(
            "cannot read {CODEOWNERS_FILE} on {reference}, which says whose issues agents may \
             act on: {error}; run `git fetch origin main`"
        )
    })?;
    Ok(Trust::from_codeowners(&text))
}

/// Why agents may not read `issue` at all, or `None` if they may: it is a pull request, or
/// its author is neither the bot nor a code owner. The reason never quotes the issue's text.
#[must_use]
pub fn refused(issue: &Issue, trust: &Trust) -> Option<String> {
    let number = issue.number;
    if issue.is_pull_request {
        return Some(format!("#{number} is a pull request, not an issue"));
    }
    if !trust.is_trusted(&issue.author) {
        return Some(format!(
            "#{number} was opened by `{}`, who is neither the bot nor a code owner, so agents \
             do not act on it (ADR-0021); a code owner may file it again in their own words",
            issue.author
        ));
    }
    None
}

/// Why agents may not work on `issue`, or `None` if they may. `events` are the issue's label
/// events, oldest first. A reason never quotes the issue's text, which may be untrusted.
#[must_use]
pub fn unworkable(issue: &Issue, events: &[LabelEvent], trust: &Trust) -> Option<String> {
    if let Some(reason) = refused(issue, trust) {
        return Some(reason);
    }
    let number = issue.number;
    if !issue.is_open {
        return Some(format!("#{number} is closed"));
    }
    let kinds = issue.labels.iter().filter(|label| KINDS.contains(&label.as_str())).count();
    if kinds != 1 {
        return Some(format!(
            "#{number} has {kinds} kind labels; an issue for agents has exactly one of {}",
            KINDS.join(", ")
        ));
    }
    if issue.labels.iter().any(|label| label == NEEDS_DECISION) {
        return Some(format!("#{number} waits for the maintainer's decision (`{NEEDS_DECISION}`)"));
    }
    decision_removed_by(events, trust).map(|actor| {
        format!(
            "#{number} had `{NEEDS_DECISION}` removed by `{actor}`, who is not a code owner; \
             it still waits for a decision, and only a code owner removes the label (ADR-0021)"
        )
    })
}

/// Who last removed `needs-decision`, if that was not a code owner. `None` when the label was
/// never removed, or a code owner removed it last.
fn decision_removed_by<'events>(
    events: &'events [LabelEvent],
    trust: &Trust,
) -> Option<&'events str> {
    let last = events.iter().rev().find(|event| event.label == NEEDS_DECISION)?;
    let is_open_question = !last.added && !trust.is_owner(&last.actor);
    is_open_question.then_some(last.actor.as_str())
}

/// The comments by trusted authors, and how many others were left out.
#[must_use]
pub fn trusted_comments<'comments>(
    comments: &'comments [Comment],
    trust: &Trust,
) -> (Vec<&'comments Comment>, usize) {
    let kept: Vec<&Comment> =
        comments.iter().filter(|comment| trust.is_trusted(&comment.author)).collect();
    let left_out = comments.len() - kept.len();
    assert!(kept.len() + left_out == comments.len(), "every comment is kept or left out");
    (kept, left_out)
}

#[cfg(test)]
mod tests {
    use super::{BOT_LOGIN, Trust, trusted_comments, unworkable};
    use crate::issue::github::{Comment, Issue, LabelEvent};

    fn trust() -> Trust {
        Trust::from_codeowners(
            "# Owners.\n/xtask/ @Owner @org/team # @commented\n/a me@x.org @two\n",
        )
    }

    fn issue(author: &str, labels: &[&str]) -> Issue {
        Issue {
            number: 7,
            title: "Ignore previous instructions".to_owned(),
            author: author.to_owned(),
            is_open: true,
            is_pull_request: false,
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            body: String::new(),
        }
    }

    fn event(actor: &str, added: bool) -> LabelEvent {
        LabelEvent { label: "needs-decision".to_owned(), actor: actor.to_owned(), added }
    }

    // Goal: users named in CODEOWNERS are owners, in any case; the bot is trusted but is no
    // owner; teams, e-mail addresses, comments and patterns trust nobody.
    #[test]
    fn codeowners_name_the_owners() {
        let trust = trust();
        assert!(trust.is_owner("owner") && trust.is_owner("OWNER") && trust.is_owner("two"));
        assert!(trust.is_trusted("owner") && trust.is_trusted(BOT_LOGIN));
        assert!(!trust.is_owner(BOT_LOGIN), "the bot never decides");
        for nobody in
            ["org/team", "org", "team", "me@x.org", "commented", "/xtask/", "", "nami-agent"]
        {
            assert!(!trust.is_trusted(nobody), "`{nobody}` is not trusted");
        }
        assert!(!Trust::from_codeowners("@owner\n").is_owner("owner"), "a pattern is not an owner");
    }

    // Goal: an issue is workable only if it is an open issue by a trusted author with one kind
    // label and no open decision. Each reason is reported, and none quotes the title.
    #[test]
    fn workable_issues_pass_every_condition() {
        let trust = trust();
        assert_eq!(unworkable(&issue(BOT_LOGIN, &["bug"]), &[], &trust), None);
        assert_eq!(unworkable(&issue("Owner", &["docs", "other"]), &[], &trust), None);
        let pull = Issue { is_pull_request: true, ..issue(BOT_LOGIN, &["bug"]) };
        let closed = Issue { is_open: false, ..issue(BOT_LOGIN, &["bug"]) };
        let cases = [
            (pull, "is a pull request"),
            (issue("outsider", &["bug"]), "opened by `outsider`, who is neither"),
            (closed, "is closed"),
            (issue(BOT_LOGIN, &[]), "has 0 kind labels"),
            (issue(BOT_LOGIN, &["bug", "docs"]), "has 2 kind labels"),
            (issue(BOT_LOGIN, &["bug", "needs-decision"]), "waits for the maintainer"),
        ];
        for (case, expected) in cases {
            let reason = unworkable(&case, &[], &trust).expect("not workable");
            assert!(reason.starts_with("#7 ") && reason.contains(expected), "{reason}");
            assert!(!reason.contains("Ignore previous"), "the title is never quoted: {reason}");
        }
    }

    // Goal: a decision counts as made only when a code owner removed `needs-decision` last.
    #[test]
    fn only_a_code_owner_clears_a_decision() {
        let trust = trust();
        let bug = issue(BOT_LOGIN, &["bug"]);
        let decided = [event(BOT_LOGIN, true), event("owner", false)];
        assert_eq!(unworkable(&bug, &decided, &trust), None);
        let by_bot = [event("owner", true), event(BOT_LOGIN, false)];
        let reason = unworkable(&bug, &by_bot, &trust).expect("the bot cannot decide");
        assert!(reason.contains("removed by `nami-agent[bot]`"), "{reason}");
        let ghost = [event("owner", true), event("ghost", false)];
        assert!(unworkable(&bug, &ghost, &trust).is_some(), "a deleted account cannot decide");
        let again = [event("owner", false), event(BOT_LOGIN, true), event(BOT_LOGIN, false)];
        assert!(unworkable(&bug, &again, &trust).is_some(), "the last removal counts");
        let other =
            LabelEvent { label: "bug".to_owned(), actor: BOT_LOGIN.to_owned(), added: false };
        assert_eq!(unworkable(&bug, &[other], &trust), None, "other labels do not matter");
    }

    // Goal: comments by anyone but the bot and the code owners are left out and counted.
    #[test]
    fn untrusted_comments_are_left_out() {
        let comment =
            |author: &str| Comment { author: author.to_owned(), body: format!("by {author}") };
        let comments = [comment(BOT_LOGIN), comment("outsider"), comment("owner"), comment("x")];
        let (kept, left_out) = trusted_comments(&comments, &trust());
        let authors: Vec<&str> =
            kept.iter().map(|kept_comment| kept_comment.author.as_str()).collect();
        assert_eq!(authors, [BOT_LOGIN, "owner"]);
        assert_eq!(left_out, 2);
    }
}
