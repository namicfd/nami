//! Task claims on the `claims` branch (ADR-0011).
//!
//! A claim is the file `claims/<task-id>` on the remote branch `claims`. Claiming builds a commit
//! on top of the branch's current tip with git plumbing (a private index, no working-tree
//! changes) and pushes it with a lease on the tip it was built on. Git rejects that push if the
//! branch moved, so when two agents claim at once exactly one push wins; the other refetches and
//! finds the task taken.
//!
//! What git push prints or returns never decides a claim: after every push the branch is
//! refetched, and the claim is ours if and only if the remote's tip is our commit or descends
//! from it. Two sessions of the bot share one identity, and git times are whole seconds, so two
//! claims built on one tip in the same second would be the same commit (#32). So every claim
//! commit carries a nonce, which makes "the remote holds our commit" mean "our push landed".

use super::pulls::WRITE_TOKEN;
use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// The branch that holds the claims.
pub const BRANCH: &str = "claims";

/// The local ref the claims branch is fetched into.
const LOCAL_REF: &str = "refs/nami/claims";

/// Attempts before giving up when other claims keep landing first, or the remote keeps
/// rejecting the push.
pub const PUSH_ATTEMPTS_MAX: u32 = 8;

/// The pause before each attempt, in milliseconds: none before the first, then doubling, so
/// that a claim outlasts another push holding the remote's ref lock (about 2.4 s in all).
const PAUSES_MILLISECONDS: [u64; 8] = [0, 25, 50, 100, 200, 400, 800, 800];

/// Unique tokens this process has made (for commit nonces and index names): part of each.
static TOKENS_MADE: AtomicU32 = AtomicU32::new(0);

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
    let mut attempts = 0_u32;
    for pause in PAUSES_MILLISECONDS {
        std::thread::sleep(std::time::Duration::from_millis(pause));
        attempts += 1;
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
            // Another claim landed first, or the remote rejected the push: try again.
            Pushed::Lost(reason) => last_error = reason,
        }
    }
    assert_eq!(attempts, PUSH_ATTEMPTS_MAX, "one attempt per pause");
    Err(ClaimError::Git(format!(
        "gave up after {PUSH_ATTEMPTS_MAX} attempts. The remote may be refusing the claim push \
         (check its hooks, branch protection and permissions), other claims kept landing \
         first, or another push still holds the remote's ref lock; then retrying later may \
         work. The last attempt: {last_error}"
    )))
}

/// What a push of a claim commit did.
#[derive(Debug, PartialEq, Eq)]
enum Pushed {
    /// The remote's branch holds our commit: we won.
    Landed,
    /// The branch does not hold our commit, and git said why: the summary and git's stderr.
    Lost(String),
}

/// Pushes `commit` to the claims branch if the branch is still at `tip` (`None`: if it does not
/// exist yet). Whether it landed is decided by what the remote holds afterwards, not by git's
/// output or exit code: a push can land and still fail, when the remote dies after updating the
/// branch. If the branch cannot be read back (no network, a dead remote), that is an error that
/// says the claim may be held. If it can, but git reported nothing on our refspec, that is an
/// error too.
fn push(root: &Path, remote: &str, commit: &str, tip: Option<&str>) -> Result<Pushed, ClaimError> {
    // With a lease that matches, `--force-with-lease` is a forced push: the lease is the only
    // check, and git does not require a fast-forward. It is safe only because the commit's
    // parent is the lease's tip, so the forced update is a fast-forward all the same; the
    // assertion keeps it so. The lease names that tip (empty: the branch must not exist yet).
    let parents = git(root, &["log", "-1", "--format=%P", commit], &[], None)?;
    assert_eq!(
        parents.trim(),
        tip.unwrap_or_default(),
        "a claim commit's only parent is the tip its lease names (none on an empty branch), so \
         the forced push cannot drop another claim"
    );
    let lease = format!("--force-with-lease=refs/heads/{BRANCH}:{}", tip.unwrap_or_default());
    let refspec = format!("{commit}:refs/heads/{BRANCH}");
    let mut command = Command::new("git");
    let _ = command.args(["push", "--porcelain", &lease, remote, &refspec]);
    let _ = command.current_dir(root).stdin(Stdio::null());
    let output = run(command, None)
        .map_err(|error| ClaimError::Git(format!("cannot run git push {remote}: {error}")))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let held = fetch(root, remote).and_then(|now| match now {
        Some(now) => is_ancestor(root, commit, &now),
        None => Ok(false),
    });
    let held = held.map_err(|error| {
        ClaimError::Git(format!(
            "the claim push may have landed, so the claim may be held, but the claims branch \
             cannot be read back: {error}. Check with `cargo xtask task status`. git push said: \
             {stderr}"
        ))
    })?;
    if held {
        return Ok(Pushed::Landed);
    }
    match retry_reason(&String::from_utf8_lossy(&output.stdout), &refspec) {
        Some(summary) => Ok(Pushed::Lost(format!("{summary}: {stderr}"))),
        None => Err(ClaimError::Git(format!("git push {remote} {refspec} did not land: {stderr}"))),
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

/// Reads what `git push --porcelain` said about `refspec`, when the remote does not hold our
/// commit: the line is `<flag> TAB <refspec> TAB <summary>`. Returns the summary if retrying
/// can help: `!`, any rejection (git's reasons vary across versions, and a racing update that
/// holds the remote's ref lock reads like a refusal), or `=`, up to date, which the nonce makes
/// impossible unless the remote misreports. `None` if git did not report on `refspec`.
fn retry_reason(porcelain: &str, refspec: &str) -> Option<String> {
    assert!(refspec.contains(':'), "a refspec names its source and destination");
    let (flag, summary) = porcelain.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let (flag, reference, summary) = (fields.next()?, fields.next()?, fields.next()?);
        (reference == refspec).then_some((flag, summary))
    })?;
    matches!(flag, "!" | "=").then(|| summary.to_owned())
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
/// same tip by the same identity in the same second (#32).
fn nonce() -> String {
    let nonce = format!("Nonce: {}", unique_token());
    assert!(!nonce.contains('\n'), "the nonce is one line");
    nonce
}

/// A token no other call returns, in this process or another: the process, a per-process count
/// and a random value. Not the clock: whole seconds are what made commits collide (#32), and
/// clippy bans reading it (ADR-0005); the random value already covers what a finer clock would.
fn unique_token() -> String {
    let count = TOKENS_MADE.fetch_add(1, Ordering::Relaxed);
    assert!(count < u32::MAX, "a process makes fewer than 2^32 tokens");
    // A fresh `RandomState` has keys from the operating system's random source.
    let random = RandomState::new().hash_one(count);
    let token = format!("{}-{count}-{random:016x}", std::process::id());
    let is_plain =
        token.chars().all(|character| character.is_ascii_alphanumeric() || character == '-');
    assert!(is_plain, "a token is safe in a file name and a commit message: {token}");
    token
}

/// A path for a throwaway index inside the repository's git directory, unique to this call, so
/// that two claims never share an index, even from two threads of one process.
fn private_index(root: &Path, task: &str) -> Result<PathBuf, ClaimError> {
    let git_directory = git(root, &["rev-parse", "--absolute-git-dir"], &[], None)?;
    let name = format!("nami-claim-{task}-{}.index", unique_token());
    Ok(Path::new(git_directory.trim()).join(name))
}

/// The git command `git <arguments>` in `root`, with `environment` added. The app's token for
/// writes ([`WRITE_TOKEN`]) is removed from its environment unless `environment` names it:
/// only the push of `bot/passes` does (ADR-0023, rule 2).
pub(crate) fn git_command(
    root: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> Command {
    let mut command = Command::new("git");
    let _ = command.args(arguments).current_dir(root).env_remove(WRITE_TOKEN);
    let _ = command.envs(environment.iter().copied());
    let given = environment.iter().any(|(name, _)| *name == WRITE_TOKEN);
    let sees = command.get_envs().any(|(name, value)| name == WRITE_TOKEN && value.is_some());
    assert_eq!(sees, given, "git sees the token for writes only when it is given");
    command
}

/// Runs git in `root` and returns its stdout, or its stderr as the error.
pub(crate) fn git(
    root: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
    stdin: Option<&str>,
) -> Result<String, ClaimError> {
    let mut command = git_command(root, arguments, environment);
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
    use super::{
        ClaimError, WRITE_TOKEN, commit_change, git, git_command, private_index, retry_reason,
    };

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
        let index = |_| private_index(&root, "t").expect("the git directory is found");
        assert_ne!(index(1), index(2), "two claims in one process never share an index");
        let (released, again) = (build(Some(&first), None), build(Some(&first), None));
        assert_ne!(released, again, "releases on one tip differ");
        let message = git(&root, &["log", "-1", "--format=%B", &first], &[], None);
        let message = message.expect("the commit is readable");
        assert!(message.starts_with("Claim t\n\nagent: bot\n\nNonce: "), "{message}");
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    // Goal: when the remote does not hold our commit, every rejection and "up to date" is
    // retried, and a push git did not report on is not. Method: porcelain output as git 2.53
    // prints it.
    #[test]
    fn retry_reasons() {
        let refspec = "abc:refs/heads/claims";
        let line =
            |flag: &str, summary: &str| format!("To r.git\n{flag}\t{refspec}\t{summary}\nDone\n");
        for summary in [
            "[rejected] (stale info)",
            "[rejected] (fetch first)",
            "[remote rejected] (reference already exists)",
            "[remote rejected] (pre-receive hook declined)",
        ] {
            assert_eq!(retry_reason(&line("!", summary), refspec).as_deref(), Some(summary));
        }
        let same = retry_reason(&line("=", "[up to date]"), refspec);
        assert_eq!(same.as_deref(), Some("[up to date]"));
        assert_eq!(retry_reason(&line("*", "[new branch]"), refspec), None);
        assert_eq!(retry_reason(&line("+", "1...2 (forced update)"), refspec), None);
        assert_eq!(retry_reason(&line("!", "[rejected]"), "def:refs/heads/claims"), None);
        assert_eq!(retry_reason("", refspec), None, "a push that died prints nothing");
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

    // Goal: no git command sees the app's token for writes, unless it is handed to that one
    // command, as the push of bot/passes does (ADR-0023, rule 2).
    #[test]
    fn git_sees_the_write_token_only_when_given() {
        let root = std::path::Path::new(".");
        let token_of = |environment: &[(&str, &str)]| {
            let command = git_command(root, &["status"], environment);
            let found = command.get_envs().find(|(name, _)| *name == WRITE_TOKEN);
            found.map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };
        assert_eq!(token_of(&[]), Some(None), "removed");
        assert_eq!(token_of(&[("GIT_INDEX_FILE", "x")]), Some(None), "removed");
        assert_eq!(token_of(&[(WRITE_TOKEN, "t")]), Some(Some("t".to_owned())), "given");
    }
}
