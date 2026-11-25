//! Task claims on the `claims` branch (ADR-0011).
//!
//! A claim is the file `claims/<task-id>` on the remote branch `claims`. Claiming builds a commit
//! on top of the branch's current tip with git plumbing (a private index, no working-tree
//! changes) and pushes it. Git rejects a push that is not a fast-forward, so when two agents
//! claim at once exactly one push wins; the other refetches and finds the task taken.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The branch that holds the claims.
pub const BRANCH: &str = "claims";

/// The local ref the claims branch is fetched into.
const LOCAL_REF: &str = "refs/nami/claims";

/// Attempts before giving up when other claims keep landing first.
const PUSH_ATTEMPTS_MAX: u32 = 8;

/// One claimed task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The claimed task.
    pub task: String,
    /// The claim file: who holds it.
    pub holder: String,
    /// When it was claimed, as git describes it relative to now ("2 hours ago").
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
    let heads = git(root, &["ls-remote", "--heads", remote, BRANCH], &[], None)?;
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
    let names = git(root, &["ls-tree", "--name-only", &format!("{tip}:claims/")], &[], None);
    // A branch whose last claim was released has no `claims/` directory.
    let Ok(names) = names else {
        return Ok(Vec::new());
    };
    let mut claims = Vec::new();
    for task in names.lines().filter(|name| !name.is_empty()) {
        let path = format!("claims/{task}");
        let holder = git(root, &["show", &format!("{tip}:{path}")], &[], None)?;
        let age = git(root, &["log", "-1", "--format=%cr", tip, "--", &path], &[], None)?;
        claims.push(Claim {
            task: task.to_owned(),
            holder: holder.trim().to_owned(),
            age: age.trim().to_owned(),
        });
    }
    Ok(claims)
}

/// Claims `task` for `holder`, retrying while other claims land first.
///
/// # Errors
///
/// Fails if the task is already claimed, or if git fails.
pub fn claim(root: &Path, remote: &str, task: &str, holder: &str) -> Result<(), ClaimError> {
    assert!(!task.is_empty() && !task.contains('/'), "task ids are single path components");
    assert!(!holder.trim().is_empty(), "a claim names its holder");
    update(root, remote, task, Some(holder))
}

/// Releases the claim on `task`.
///
/// # Errors
///
/// Fails if the task is not claimed, or if git fails.
pub fn release(root: &Path, remote: &str, task: &str) -> Result<(), ClaimError> {
    assert!(!task.is_empty() && !task.contains('/'), "task ids are single path components");
    update(root, remote, task, None)
}

/// Adds (`Some(holder)`) or removes (`None`) the claim file and pushes the result.
fn update(root: &Path, remote: &str, task: &str, holder: Option<&str>) -> Result<(), ClaimError> {
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
        let commit = commit_change(root, tip.as_deref(), task, holder)?;
        let refspec = format!("{commit}:refs/heads/{BRANCH}");
        match git(root, &["push", "--quiet", remote, &refspec], &[], None) {
            Ok(_) => return Ok(()),
            // Another claim landed first: refetch and try again. Any other failure (no
            // network, no permission) will not go away by retrying.
            Err(ClaimError::Git(message)) if message.contains("rejected") => last_error = message,
            Err(other) => return Err(other),
        }
    }
    Err(ClaimError::Git(format!(
        "the claims branch kept moving; gave up after {PUSH_ATTEMPTS_MAX} attempts: {last_error}"
    )))
}

/// Builds a commit on `tip` with the claim file added or removed, using a private index.
fn commit_change(
    root: &Path,
    tip: Option<&str>,
    task: &str,
    holder: Option<&str>,
) -> Result<String, ClaimError> {
    let index = private_index(root, task)?;
    let index_text = index.to_string_lossy().into_owned();
    let environment = [("GIT_INDEX_FILE", index_text.as_str())];
    let result = build_commit(root, &environment, tip, task, holder);
    let _ = std::fs::remove_file(&index);
    result
}

fn build_commit(
    root: &Path,
    environment: &[(&str, &str)],
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
        format!("Claim {task}\n\n{holder}\n")
    } else {
        let _ = git(root, &["update-index", "--force-remove", &path], environment, None)?;
        format!("Release {task}\n")
    };
    let tree = git(root, &["write-tree"], environment, None)?;
    let mut arguments = vec!["commit-tree", tree.trim(), "-m", &message];
    if let Some(tip) = tip {
        arguments.extend(["-p", tip]);
    }
    let commit = git(root, &arguments, &[], None)?;
    assert!(!commit.trim().is_empty(), "commit-tree prints the new commit");
    Ok(commit.trim().to_owned())
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
    use super::ClaimError;

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
