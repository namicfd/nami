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
