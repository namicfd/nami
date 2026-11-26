//! Claims on the `claims` branch, against real git repositories (ADR-0011).

use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::task::claims::{self, ClaimError};

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git").args(arguments).current_dir(directory).output();
    let output = output.expect("git runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "git {arguments:?} failed: {stderr}");
}

/// One bot identity and one fixed clock, as every session of the bot has (`.claude/settings.json`
/// sets the identity; a whole-second clock makes claims in the same second look the same).
const BOT: &[(&str, &str)] = &[
    ("GIT_AUTHOR_NAME", "nami-agent[bot]"),
    ("GIT_AUTHOR_EMAIL", "bot@example.invalid"),
    ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"),
    ("GIT_COMMITTER_NAME", "nami-agent[bot]"),
    ("GIT_COMMITTER_EMAIL", "bot@example.invalid"),
    ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z"),
];

/// A bare remote and `clones` clones of it, in a fresh temporary directory.
fn setup(name: &str, clones: u32) -> (PathBuf, Vec<PathBuf>) {
    let root = std::env::temp_dir().join(format!("nami-claims-{name}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).expect("a stale temporary directory is removable");
    }
    std::fs::create_dir_all(&root).expect("temporary directory is writable");
    git(&root, &["init", "--quiet", "--bare", "remote.git"]);
    let mut directories = Vec::new();
    for index in 0..clones {
        let clone = format!("clone-{index}");
        git(&root, &["clone", "--quiet", "remote.git", &clone]);
        let directory = root.join(&clone);
        git(&directory, &["config", "user.name", &clone]);
        git(&directory, &["config", "user.email", "agent@example.invalid"]);
        git(&directory, &["config", "commit.gpgsign", "false"]);
        directories.push(directory);
    }
    assert_eq!(directories.len(), usize::try_from(clones).expect("small"), "every clone exists");
    (root, directories)
}

fn claimed(directory: &Path) -> Vec<String> {
    let tip = claims::fetch(directory, "origin").expect("fetch works");
    let list = claims::list(directory, tip.as_deref()).expect("list works");
    list.into_iter().map(|claim| format!("{}={}", claim.task, claim.holder)).collect()
}

// Goal: claim, conflicting claim, release and reclaim behave as specified across two clones.
// Method: two clones of one bare remote take turns; the second always sees the first's pushes.
#[test]
fn claims_are_exclusive_and_releasable() {
    let (root, clones) = setup("sequence", 2);
    let (first, second) = (&clones[0], &clones[1]);
    assert_eq!(claimed(first), Vec::<String>::new(), "no claims branch yet");
    claims::claim(first, "origin", BOT, "t1", "agent: one").expect("the first claim succeeds");
    let conflict = claims::claim(second, "origin", BOT, "t1", "agent: two");
    assert_eq!(conflict, Err(ClaimError::AlreadyClaimed("agent: one".to_owned())));
    claims::claim(second, "origin", BOT, "t2", "agent: two").expect("another task is free");
    assert_eq!(claimed(first), ["t1=agent: one", "t2=agent: two"]);
    claims::release(first, "origin", BOT, "t1").expect("the holder releases");
    assert_eq!(claims::release(second, "origin", BOT, "t1"), Err(ClaimError::NotClaimed));
    claims::release(second, "origin", BOT, "t2").expect("releasing the last claim works");
    assert_eq!(claimed(first), Vec::<String>::new(), "the branch is empty again");
    claims::claim(second, "origin", BOT, "t1", "agent: two")
        .expect("a released task can be reclaimed");
    assert_eq!(claimed(first), ["t1=agent: two"]);
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: when clones race for one task, exactly one wins; when they race for different tasks,
// all win. Method: one thread per clone, all claiming at once as the same bot at the same
// second, so two claims built on one tip would be the same commit if nothing set them apart
// (#32); the outcome does not depend on the interleaving, because losers refetch and retry.
#[test]
fn racing_claims_have_one_winner() {
    let (root, clones) = setup("race", 4);
    let same: Vec<Result<(), ClaimError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = clones
            .iter()
            .map(|clone| {
                scope.spawn(move || claims::claim(clone, "origin", BOT, "shared", "agent"))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("thread finishes")).collect()
    });
    let winners = same.iter().filter(|result| result.is_ok()).count();
    assert_eq!(winners, 1, "exactly one claim wins: {same:?}");
    let losers_saw_claim =
        same.iter().filter(|result| matches!(result, Err(ClaimError::AlreadyClaimed(_)))).count();
    assert_eq!(losers_saw_claim, 3, "the others find the task taken: {same:?}");

    let distinct: Vec<Result<(), ClaimError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = clones
            .iter()
            .enumerate()
            .map(|(index, clone)| {
                let task = format!("own-{index}");
                scope.spawn(move || claims::claim(clone, "origin", BOT, &task, "a"))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("thread finishes")).collect()
    });
    assert!(distinct.iter().all(Result::is_ok), "different tasks all succeed: {distinct:?}");
    assert_eq!(claimed(&clones[0]).len(), 5, "shared plus four own claims");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: two claims for one task, built on the same tip by the same bot at the same second,
// have one winner (#32). Method: the second clone fetches from a remote that never sees the
// first claim but pushes to the real one, so it always builds its claim on the empty branch
// the first claim already replaced: the race, without depending on timing.
#[test]
fn claims_built_on_one_tip_have_one_winner() {
    let (root, clones) = setup("one-tip", 2);
    let (first, second) = (&clones[0], &clones[1]);
    git(&root, &["init", "--quiet", "--bare", "stale.git"]);
    let (stale, real) = (root.join("stale.git"), root.join("remote.git"));
    git(second, &["remote", "set-url", "origin", &stale.to_string_lossy()]);
    git(second, &["remote", "set-url", "--push", "origin", &real.to_string_lossy()]);
    claims::claim(first, "origin", BOT, "t1", "agent: bot").expect("the first claim succeeds");
    let late = claims::claim(second, "origin", BOT, "t1", "agent: bot");
    let lost = matches!(&late, Err(ClaimError::Git(message)) if message.contains("stale info"));
    assert!(lost, "a claim built on a replaced tip loses its lease, never wins: {late:?}");
    assert_eq!(claimed(first), ["t1=agent: bot"], "the first claim stands");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

/// Installs `script` as the remote's hook `name`.
fn install_hook(root: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("remote.git/hooks").join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("the hook is written");
    let executable = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&path, executable).expect("the hook is executable");
    assert!(path.is_file(), "the hook exists");
}

// Goal: a claim the remote accepted is held, even when git push then fails (exit 1, `*` in its
// output): the remote's tip decides, not the exit code. Method: a post-receive hook kills
// receive-pack after the branch moved.
#[test]
fn a_landed_claim_holds_when_the_push_dies_after_receiving() {
    let (root, clones) = setup("post-receive", 1);
    install_hook(&root, "post-receive", "kill -9 $PPID");
    claims::claim(&clones[0], "origin", BOT, "t1", "agent: bot").expect("the claim landed");
    assert_eq!(claimed(&clones[0]), ["t1=agent: bot"], "the remote holds the claim");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: as above, when git push prints nothing at all and exits 128. Method: a
// reference-transaction hook kills receive-pack once the ref update is committed.
#[test]
fn a_landed_claim_holds_when_the_push_dies_silently() {
    let (root, clones) = setup("reference-transaction", 1);
    install_hook(
        &root,
        "reference-transaction",
        r#"if [ "$1" = committed ]; then kill -9 $PPID; fi"#,
    );
    claims::claim(&clones[0], "origin", BOT, "t1", "agent: bot").expect("the claim landed");
    assert_eq!(claimed(&clones[0]), ["t1=agent: bot"], "the remote holds the claim");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: a remote that keeps rejecting the push is retried a bounded number of times, then an
// error that names both causes: a refusal (hooks, protection, permissions) or contention.
// Method: a pre-receive hook that counts its runs and always declines.
#[test]
fn a_claim_push_the_remote_keeps_rejecting_is_an_error_after_bounded_retries() {
    let (root, clones) = setup("pre-receive", 1);
    install_hook(&root, "pre-receive", "echo run >> pre-receive-runs; exit 1");
    let refused = claims::claim(&clones[0], "origin", BOT, "t1", "agent: bot");
    let message = match &refused {
        Err(ClaimError::Git(message)) => message.clone(),
        other => panic!("a rejected claim is a git error: {other:?}"),
    };
    for part in ["may be refusing", "hooks, branch protection", "kept landing", "hook declined"] {
        assert!(message.contains(part), "the error names `{part}`: {message}");
    }
    let runs = std::fs::read_to_string(root.join("remote.git/pre-receive-runs"));
    let runs = u32::try_from(runs.expect("the hook ran").lines().count()).expect("few runs");
    assert_eq!(runs, claims::PUSH_ATTEMPTS_MAX, "every attempt pushed, and no more");
    assert_eq!(claimed(&clones[0]), Vec::<String>::new(), "nothing was claimed");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: a remote rejection while the branch moved is retried, and the claim wins on the new
// tip. Method: a pre-receive hook that, the first time, creates the branch itself (as a racing
// claim would) and declines; afterwards it accepts.
#[test]
fn a_remote_rejection_while_the_branch_moved_is_retried() {
    let (root, clones) = setup("moved", 1);
    let script = "[ -e raced ] && exit 0\ntouch raced\n\
                  unset GIT_QUARANTINE_PATH GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES\n\
                  tree=$(git mktree </dev/null)\n\
                  other=$(git -c user.name=o -c user.email=o@x commit-tree $tree -m other)\n\
                  git update-ref refs/heads/claims $other\nexit 1";
    install_hook(&root, "pre-receive", script);
    claims::claim(&clones[0], "origin", BOT, "t1", "agent: bot").expect("the retry wins");
    assert!(root.join("remote.git/raced").exists(), "the first push lost the race");
    assert_eq!(claimed(&clones[0]), ["t1=agent: bot"], "our claim is the only one held");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

/// Shell: wait for the file `$1` (up to 30 s), else fail loudly.
const WAIT_FOR: &str = r#"wait_for() {
  i=0
  while [ ! -e "$1" ]; do
    i=$((i + 1))
    if [ "$i" -gt 3000 ]; then echo "timed out waiting for $1" >&2; exit 1; fi
    sleep 0.01
  done
}"#;

/// Waits (up to 30 s) until `done` holds, else fails the test.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..3000 {
        if done() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out waiting until {what}");
}

/// How many pushes the remote's counting pre-receive hook has seen.
fn pushes(root: &Path) -> usize {
    let pushes = std::fs::read_to_string(root.join("remote.git/pushes"));
    pushes.map_or(0, |pushes| pushes.lines().count())
}

// Goal: a claim of a free task wins although another claim holds the remote's ref lock, the
// remote rejects its push, and the tip has not moved when it refetches. Method, driven by
// markers rather than time:
// 1. Claim A's update waits in `prepared`, holding the ref lock, until `release`.
// 2. Claim B's first push (push 2) is rejected at the lock; it refetches the unmoved tip.
// 3. B's second push (push 3) waits in pre-receive until `b-go`.
// 4. The test then creates `release`, joins A (committed), creates `b-go` and joins B.
// B's second push was built on the empty branch, so it is rejected; its third wins.
#[test]
fn a_claim_outlasts_another_claim_holding_the_ref_lock() {
    let (root, clones) = setup("contention", 2);
    let hold = format!(
        "{WAIT_FOR}\nif [ \"$1\" = prepared ] && [ ! -e holding ]; then\n  touch holding\n  \
         wait_for release\nfi"
    );
    install_hook(&root, "reference-transaction", &hold);
    let count = format!(
        "{WAIT_FOR}\necho push >> pushes\n\
         if [ \"$(wc -l < pushes)\" -eq 3 ]; then wait_for b-go; fi"
    );
    install_hook(&root, "pre-receive", &count);
    let remote = root.join("remote.git");
    std::thread::scope(|scope| {
        let first = scope.spawn(|| claims::claim(&clones[0], "origin", BOT, "t1", "agent: one"));
        wait_until("A holds the ref lock", || remote.join("holding").exists());
        let second = scope.spawn(|| claims::claim(&clones[1], "origin", BOT, "t2", "agent: two"));
        wait_until("B was rejected and pushed again", || pushes(&root) >= 3);
        std::fs::write(remote.join("release"), "").expect("the marker is written");
        let first = first.join().expect("claim A finishes");
        assert_eq!(first, Ok(()), "the claim holding the lock wins");
        std::fs::write(remote.join("b-go"), "").expect("the marker is written");
        let second = second.join().expect("claim B finishes");
        assert_eq!(second, Ok(()), "the claim that was rejected wins too");
    });
    assert_eq!(pushes(&root), 4, "A once; B rejected twice, then landed");
    assert_eq!(claimed(&clones[0]), ["t1=agent: one", "t2=agent: two"], "both claims are held");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: a claim wins when another claim lands on top of it before it reads the branch back:
// the remote holds our commit as an ancestor, not as its tip. Method: a post-receive hook
// that, once, commits on top of what was pushed.
#[test]
fn a_claim_wins_when_another_lands_on_top_before_the_refetch() {
    let (root, clones) = setup("stacked", 1);
    let script = r#"read old new ref
[ -e stacked ] && exit 0
touch stacked
other=$(git -c user.name=o -c user.email=o@x commit-tree "$new^{tree}" -p "$new" -m other)
git update-ref refs/heads/claims "$other""#;
    install_hook(&root, "post-receive", script);
    claims::claim(&clones[0], "origin", BOT, "t1", "agent: bot").expect("our claim is held");
    assert!(root.join("remote.git/stacked").exists(), "another commit landed on ours");
    assert_eq!(claimed(&clones[0]), ["t1=agent: bot"], "the remote holds our claim");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: when the push may have landed but the branch cannot be read back, the error says the
// claim may be held, how to check, and what git push said. Method: a post-receive hook moves
// the remote away, so the refetch finds no repository.
#[test]
fn a_claim_that_cannot_be_read_back_says_it_may_be_held() {
    let (root, clones) = setup("unreadable", 1);
    install_hook(&root, "post-receive", "echo landed >&2; mv ../remote.git ../moved.git");
    let unknown = claims::claim(&clones[0], "origin", BOT, "t1", "agent: bot");
    let message = match &unknown {
        Err(ClaimError::Git(message)) => message.clone(),
        other => panic!("an unreadable branch is a git error: {other:?}"),
    };
    for part in ["may be held", "cargo xtask task status", "landed"] {
        assert!(message.contains(part), "the error names `{part}`: {message}");
    }
    std::fs::remove_dir_all(&root).expect("cleanup");
}
