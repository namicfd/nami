//! Task claims on the `claims` branch (ADR-0011).
//!
//! A claim is the file `claims/<task-id>` on the remote branch `claims`. Claiming builds a commit
//! on top of the branch's current tip with git plumbing (a private index, no working-tree
//! changes) and pushes it with a lease on the tip it was built on. Git rejects that push if the
//! branch moved, so when two agents claim at once exactly one push wins; the other refetches and
//! finds the task taken.
//!
//! Two sessions of the bot share one identity, and git times are whole seconds, so two claims
//! built on one tip in the same second would be the same commit, and the second push would be
//! "up to date" (#32). So every claim commit carries a nonce, and a push counts as a win only if
//! git reports that it moved the branch to our commit.

use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// The branch that holds the claims.
pub const BRANCH: &str = "claims";

/// The local ref the claims branch is fetched into.
const LOCAL_REF: &str = "refs/nami/claims";

/// Attempts before giving up when other claims keep landing first.
const PUSH_ATTEMPTS_MAX: u32 = 8;

/// Claim and release commits this process has built: part of each commit's nonce.
static COMMITS_BUILT: AtomicU32 = AtomicU32::new(0);

/// One claimed task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The claimed task.
    pub task: String,
    /// The claim file: who holds it.
    pub holder: String,
    /// When it was claimed: the claim commit's committer time, in strict ISO 8601.
    pub claimed_at: String,
    /// The same time as git describes it relative to now ("2 hours ago"), for people.
    pub age: String,
}

/// Why a claim or release did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// Someone else holds the task.
    AlreadyClaimed(String),
    /// Releasing a task that nobody holds.
    NotClaimed,
    /// A git command failed.
    Git(String),
}

impl fmt::Display for ClaimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyClaimed(holder) => write!(formatter, "already claimed: {holder}"),
            Self::NotClaimed => write!(formatter, "not claimed"),
            Self::Git(message) => write!(formatter, "git failed: {message}"),
        }
    }
}

/// Fetches the claims branch. Returns its tip, or `None` if the branch does not exist yet.
///
/// # Errors
///
/// Returns the git error if the remote cannot be reached.
pub fn fetch(root: &Path, remote: &str) -> Result<Option<String>, ClaimError> {
    // The full ref: `ls-remote` matches patterns by suffix, so `claims` alone would also match
    // a branch such as `task/claims`.
    let full_ref = format!("refs/heads/{BRANCH}");
    let heads = git(root, &["ls-remote", remote, &full_ref], &[], None)?;
    if heads.trim().is_empty() {
        return Ok(None);
    }
    let refspec = format!("+refs/heads/{BRANCH}:{LOCAL_REF}");
    let _ = git(root, &["fetch", "--quiet", "--no-tags", remote, &refspec], &[], None)?;
    let tip = git(root, &["rev-parse", LOCAL_REF], &[], None)?;
    Ok(Some(tip.trim().to_owned()))
}

/// Lists the claims at `tip`.
///
/// # Errors
///
/// Returns the git error if the commit cannot be read.
pub fn list(root: &Path, tip: Option<&str>) -> Result<Vec<Claim>, ClaimError> {
    let Some(tip) = tip else {
        return Ok(Vec::new());
    };
    // With a pathspec, a missing `claims/` directory (every claim released) lists nothing and
    // exits 0, so any error here is real and must not read as "no claims".
    let paths = git(root, &["ls-tree", "--name-only", tip, "--", "claims/"], &[], None)?;
    let mut claims = Vec::new();
    for path in paths.lines().filter(|path| !path.is_empty()) {
        let task = path.strip_prefix("claims/").expect("ls-tree lists paths below claims/");
        let holder = git(root, &["show", &format!("{tip}:{path}")], &[], None)?;
        let time = git(root, &["log", "-1", "--format=%cI%n%cr", tip, "--", path], &[], None)?;
        let mut lines = time.lines();
        let (claimed_at, age) =
            (lines.next().unwrap_or_default(), lines.next().unwrap_or_default());
        assert!(!claimed_at.is_empty(), "every claim file was added by a commit");
        claims.push(Claim {
            task: task.to_owned(),
            holder: holder.trim().to_owned(),
            claimed_at: claimed_at.to_owned(),
            age: age.to_owned(),
        });
    }
    Ok(claims)
}

/// Claims `task` for `holder`, retrying while other claims land first.
///
/// `commit_environment` is set only for the `git commit-tree` that builds the claim commit:
/// its author, committer and dates (`GIT_AUTHOR_*`, `GIT_COMMITTER_*`). The CLI passes none, so
/// the commit takes them from the process; tests pin them to make two claims look alike.
///
/// # Errors
///
/// Fails if the task is already claimed, or if git fails.
pub fn claim(
    root: &Path,
    remote: &str,
    commit_environment: &[(&str, &str)],
    task: &str,
    holder: &str,
) -> Result<(), ClaimError> {
    assert!(!task.is_empty() && !task.contains('/'), "task ids are single path components");
    assert!(!holder.trim().is_empty(), "a claim names its holder");
    update(root, remote, commit_environment, task, Some(holder))
}

/// Releases the claim on `task`. `commit_environment` is as for [`claim`].
///
/// # Errors
///
/// Fails if the task is not claimed, or if git fails.
pub fn release(
    root: &Path,
    remote: &str,
    commit_environment: &[(&str, &str)],
    task: &str,
) -> Result<(), ClaimError> {
    assert!(!task.is_empty() && !task.contains('/'), "task ids are single path components");
    update(root, remote, commit_environment, task, None)
}

/// Adds (`Some(holder)`) or removes (`None`) the claim file and pushes the result.
fn update(
    root: &Path,
    remote: &str,
    commit_environment: &[(&str, &str)],
    task: &str,
    holder: Option<&str>,
) -> Result<(), ClaimError> {
    let mut last_error = String::new();
    for _ in 0..PUSH_ATTEMPTS_MAX {
        let tip = fetch(root, remote)?;
        let current = list(root, tip.as_deref())?;
        let existing = current.iter().find(|claim| claim.task == task);
        match (holder, existing) {
            (Some(_), Some(claim)) => return Err(ClaimError::AlreadyClaimed(claim.holder.clone())),
            (None, None) => return Err(ClaimError::NotClaimed),
            (Some(_), None) | (None, Some(_)) => {}
        }
        let commit = commit_change(root, commit_environment, tip.as_deref(), task, holder)?;
        match push(root, remote, &commit, tip.as_deref())? {
            Pushed::Landed => return Ok(()),
            // Another claim landed first: refetch and try again.
            Pushed::Lost(summary) => last_error = summary,
        }
    }
    Err(ClaimError::Git(format!(
        "the claims branch kept moving; gave up after {PUSH_ATTEMPTS_MAX} attempts: {last_error}"
    )))
}

/// What a push of a claim commit did.
#[derive(Debug, PartialEq, Eq)]
enum Pushed {
    /// The remote's branch holds our commit: we won.
    Landed,
    /// Another claim landed first: git's summary of why ours did not.
    Lost(String),
}

/// What `git push --porcelain` reported for our refspec, read only when the remote does not hold
/// our commit.
#[derive(Debug, PartialEq, Eq)]
enum Report {
    /// The branch moved under us (non-fast-forward, stale lease, fetch first), or already held
    /// this commit: retrying can win.
    Lost(String),
    /// The remote rejected the update itself: a hook, branch protection or permissions, or a
    /// racing update that took its ref lock first.
    RemoteRejected(String),
    /// Nothing that explains why our commit is missing.
    Unclear,
}

/// Pushes `commit` to the claims branch if the branch is still at `tip` (`None`: if it does not
/// exist yet). Whether it landed is decided by what the remote holds afterwards, not by git's
/// exit code: a push can land and still fail, when the remote dies after updating the branch.
/// Any failure that retrying cannot fix is an error.
fn push(root: &Path, remote: &str, commit: &str, tip: Option<&str>) -> Result<Pushed, ClaimError> {
    // Defence in depth: git already rejects a push that is not a fast-forward. The lease names
    // the tip the commit was built on (empty: the branch must not exist yet), and the check
    // below decides the win.
    let lease = format!("--force-with-lease=refs/heads/{BRANCH}:{}", tip.unwrap_or_default());
    let refspec = format!("{commit}:refs/heads/{BRANCH}");
    let mut command = Command::new("git");
    let _ = command.args(["push", "--porcelain", &lease, remote, &refspec]);
    let _ = command.current_dir(root).stdin(Stdio::null());
    let output = run(command, None)
        .map_err(|error| ClaimError::Git(format!("cannot run git push {remote}: {error}")))?;
    let now = fetch(root, remote)?;
    if let Some(now) = now.as_deref()
        && is_ancestor(root, commit, now)?
    {
        return Ok(Pushed::Landed);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match push_report(&String::from_utf8_lossy(&output.stdout), &refspec) {
        Report::Lost(summary) => Ok(Pushed::Lost(summary)),
        // The remote also rejects an update that lost the race for its ref lock ("reference
        // already exists", "cannot lock ref"). If the branch moved, that is what happened.
        Report::RemoteRejected(summary) if now.as_deref() != tip => Ok(Pushed::Lost(summary)),
        Report::RemoteRejected(summary) => Err(ClaimError::Git(format!(
            "{remote} refused the claim push, and retrying will not help: {summary}; check the \
             remote's hooks, branch protection and permissions: {}",
            stderr.trim()
        ))),
        Report::Unclear => Err(ClaimError::Git(format!(
            "git push {remote} {refspec} did not land: {}",
            stderr.trim()
        ))),
    }
}

/// Whether the remote's claims branch, fetched at `tip`, is `commit` or has it as an ancestor.
/// The nonce makes `commit` unique, so if the branch holds it, it was our push.
fn is_ancestor(root: &Path, commit: &str, tip: &str) -> Result<bool, ClaimError> {
    assert!(!commit.is_empty() && !tip.is_empty(), "both commits are named");
    let mut command = Command::new("git");
    let _ = command.args(["merge-base", "--is-ancestor", commit, tip]);
    let _ = command.current_dir(root).stdin(Stdio::null());
    let output = run(command, None)
        .map_err(|error| ClaimError::Git(format!("cannot run git merge-base: {error}")))?;
    // Exit 0: an ancestor; exit 1: not one; anything else: git failed.
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(ClaimError::Git(format!(
            "git merge-base --is-ancestor {commit} {tip}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// Reads what `git push --porcelain` said about `refspec`: the line is
/// `<flag> TAB <refspec> TAB <summary>`.
fn push_report(porcelain: &str, refspec: &str) -> Report {
    assert!(refspec.contains(':'), "a refspec names its source and destination");
    let line = porcelain.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let (flag, reference, summary) = (fields.next()?, fields.next()?, fields.next()?);
        (reference == refspec).then_some((flag, summary))
    });
    match line {
        Some(("!", summary)) if summary.starts_with("[remote rejected]") => {
            Report::RemoteRejected(summary.to_owned())
        }
        // Rejected: the branch moved. Up to date: the branch already held this commit, which
        // the nonce makes impossible unless the remote misreports; refetch either way.
        Some(("!" | "=", summary)) => Report::Lost(summary.to_owned()),
        Some(_) | None => Report::Unclear,
    }
}

/// Builds a commit on `tip` with the claim file added or removed, using a private index.
fn commit_change(
    root: &Path,
    commit_environment: &[(&str, &str)],
    tip: Option<&str>,
    task: &str,
    holder: Option<&str>,
) -> Result<String, ClaimError> {
    let index = private_index(root, task)?;
    let index_text = index.to_string_lossy().into_owned();
    let environment = [("GIT_INDEX_FILE", index_text.as_str())];
    let result = build_commit(root, &environment, commit_environment, tip, task, holder);
    let removed = std::fs::remove_file(&index);
    // A failed build reports its own error; it may have failed before the index existed.
    let commit = result?;
    removed
        .map_err(|error| ClaimError::Git(format!("cannot remove {}: {error}", index.display())))?;
    Ok(commit)
}

fn build_commit(
    root: &Path,
    environment: &[(&str, &str)],
    commit_environment: &[(&str, &str)],
    tip: Option<&str>,
    task: &str,
    holder: Option<&str>,
) -> Result<String, ClaimError> {
    let read_tree: &[&str] =
        if let Some(tip) = tip { &["read-tree", tip] } else { &["read-tree", "--empty"] };
    let _ = git(root, read_tree, environment, None)?;
    let path = format!("claims/{task}");
    let message = if let Some(holder) = holder {
        let content = format!("{holder}\n");
        let blob = git(root, &["hash-object", "-w", "--stdin"], &[], Some(&content))?;
        let entry = format!("100644,{},{path}", blob.trim());
        let _ = git(root, &["update-index", "--add", "--cacheinfo", &entry], environment, None)?;
        format!("Claim {task}\n\n{holder}\n\n{}\n", nonce())
    } else {
        let _ = git(root, &["update-index", "--force-remove", &path], environment, None)?;
        format!("Release {task}\n\n{}\n", nonce())
    };
    let tree = git(root, &["write-tree"], environment, None)?;
    let mut arguments = vec!["commit-tree", tree.trim(), "-m", &message];
    if let Some(tip) = tip {
        arguments.extend(["-p", tip]);
    }
    let commit = git(root, &arguments, commit_environment, None)?;
    assert!(!commit.trim().is_empty(), "commit-tree prints the new commit");
    Ok(commit.trim().to_owned())
}

/// A line that sets this commit apart from every other claim or release, even one built on the
/// same tip by the same identity in the same second: the process, a per-process count and a
/// random value. Not the clock: whole seconds are what made commits collide (#32), and clippy
/// bans reading it (ADR-0005); the random value already covers what a finer clock would.
fn nonce() -> String {
    let count = COMMITS_BUILT.fetch_add(1, Ordering::Relaxed);
    assert!(count < u32::MAX, "a process builds fewer than 2^32 claim commits");
    // A fresh `RandomState` has keys from the operating system's random source.
    let random = RandomState::new().hash_one(count);
    let nonce = format!("Nonce: {}-{count}-{random:016x}", std::process::id());
    assert!(!nonce.contains('\n'), "the nonce is one line");
    nonce
}

/// A path for a throwaway index inside the repository's git directory.
fn private_index(root: &Path, task: &str) -> Result<PathBuf, ClaimError> {
    let git_directory = git(root, &["rev-parse", "--absolute-git-dir"], &[], None)?;
    let name = format!("nami-claim-{task}-{}.index", std::process::id());
    Ok(Path::new(git_directory.trim()).join(name))
}

/// Runs git in `root` and returns its stdout, or its stderr as the error.
pub(crate) fn git(
    root: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
    stdin: Option<&str>,
) -> Result<String, ClaimError> {
    let mut command = Command::new("git");
    let _ = command.args(arguments).current_dir(root).envs(environment.iter().copied());
    let _ = command.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let output = run(command, stdin).map_err(|error| ClaimError::Git(error.to_string()))?;
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .map_err(|_| ClaimError::Git("git printed invalid UTF-8".to_owned()));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(ClaimError::Git(format!("git {}: {}", arguments.join(" "), stderr.trim())))
}

fn run(mut command: Command, stdin: Option<&str>) -> std::io::Result<Output> {
    let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    if let Some(text) = stdin {
        let mut pipe = child.stdin.take().expect("stdin is piped");
        std::io::Write::write_all(&mut pipe, text.as_bytes())?;
    }
    child.wait_with_output()
}

#[cfg(test)]
mod tests {
    use super::{ClaimError, Report, commit_change, git, push_report};

    /// The bot's identity and a fixed clock: what makes two claims look the same (#32).
    const BOT: &[(&str, &str)] = &[
        ("GIT_AUTHOR_NAME", "nami-agent[bot]"),
        ("GIT_AUTHOR_EMAIL", "bot@example.invalid"),
        ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"),
        ("GIT_COMMITTER_NAME", "nami-agent[bot]"),
        ("GIT_COMMITTER_EMAIL", "bot@example.invalid"),
        ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z"),
    ];

    // Goal: two claims (or releases) of one task built on one tip by one identity in one second
    // are different commits, so neither push can read as the other's (#32). Method: build each
    // twice in a fresh repository, on the empty branch and on a tip.
    #[test]
    fn claim_commits_are_unique() {
        let root = std::env::temp_dir().join(format!("nami-claim-unique-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root).expect("a stale temporary directory is removable");
        }
        std::fs::create_dir_all(&root).expect("temporary directory is writable");
        let _ = git(&root, &["init", "--quiet"], &[], None).expect("git init works");
        let build = |tip: Option<&str>, holder: Option<&str>| {
            commit_change(&root, BOT, tip, "t", holder).expect("the commit builds")
        };
        let (first, second) = (build(None, Some("agent: bot")), build(None, Some("agent: bot")));
        assert_ne!(first, second, "claims on the empty branch differ");
        let tree =
            |commit: &str| git(&root, &["rev-parse", &format!("{commit}^{{tree}}")], &[], None);
        assert_eq!(tree(&first), tree(&second), "only the message differs");
        let (released, again) = (build(Some(&first), None), build(Some(&first), None));
        assert_ne!(released, again, "releases on one tip differ");
        let message = git(&root, &["log", "-1", "--format=%B", &first], &[], None);
        let message = message.expect("the commit is readable");
        assert!(message.starts_with("Claim t\n\nagent: bot\n\nNonce: "), "{message}");
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    // Goal: when the remote does not hold our commit, the push report decides between retrying
    // (the branch moved, or already held the commit) and an error: the remote refused the
    // update, or nothing explains it. Method: porcelain output as git 2.53 prints it.
    #[test]
    fn push_reports() {
        let refspec = "abc:refs/heads/claims";
        let line =
            |flag: &str, summary: &str| format!("To r.git\n{flag}\t{refspec}\t{summary}\nDone\n");
        let lost = |summary: &str| Report::Lost(summary.to_owned());
        for summary in ["[rejected] (stale info)", "[rejected] (fetch first)"] {
            assert_eq!(push_report(&line("!", summary), refspec), lost(summary), "{summary}");
        }
        assert_eq!(push_report(&line("=", "[up to date]"), refspec), lost("[up to date]"));
        let declined = "[remote rejected] (pre-receive hook declined)";
        let refused = Report::RemoteRejected(declined.to_owned());
        assert_eq!(push_report(&line("!", declined), refspec), refused);
        assert_eq!(push_report(&line("*", "[new branch]"), refspec), Report::Unclear);
        assert_eq!(push_report(&line("+", "1...2 (forced update)"), refspec), Report::Unclear);
        let other = "def:refs/heads/claims";
        assert_eq!(push_report(&line("!", "[rejected]"), other), Report::Unclear);
        assert_eq!(push_report("", refspec), Report::Unclear, "a push that died prints nothing");
    }

    // Goal: claim errors read as the CLI prints them.
    #[test]
    fn errors_display() {
        assert_eq!(
            ClaimError::AlreadyClaimed("agent: x".to_owned()).to_string(),
            "already claimed: agent: x"
        );
        assert_eq!(ClaimError::NotClaimed.to_string(), "not claimed");
        assert_eq!(ClaimError::Git("boom".to_owned()).to_string(), "git failed: boom");
    }
}
