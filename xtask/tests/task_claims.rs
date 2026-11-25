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
    claims::claim(first, "origin", "t1", "agent: one").expect("the first claim succeeds");
    let conflict = claims::claim(second, "origin", "t1", "agent: two");
    assert_eq!(conflict, Err(ClaimError::AlreadyClaimed("agent: one".to_owned())));
    claims::claim(second, "origin", "t2", "agent: two").expect("another task is free");
    assert_eq!(claimed(first), ["t1=agent: one", "t2=agent: two"]);
    claims::release(first, "origin", "t1").expect("the holder releases");
    assert_eq!(claims::release(second, "origin", "t1"), Err(ClaimError::NotClaimed));
    claims::release(second, "origin", "t2").expect("releasing the last claim works");
    assert_eq!(claimed(first), Vec::<String>::new(), "the branch is empty again");
    claims::claim(second, "origin", "t1", "agent: two").expect("a released task can be reclaimed");
    assert_eq!(claimed(first), ["t1=agent: two"]);
    std::fs::remove_dir_all(&root).expect("cleanup");
}

// Goal: when clones race for one task, exactly one wins; when they race for different tasks,
// all win. Method: one thread per clone, all claiming at once; the outcome does not depend on
// the interleaving, because losers refetch and retry.
#[test]
fn racing_claims_have_one_winner() {
    let (root, clones) = setup("race", 4);
    let same: Vec<Result<(), ClaimError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = clones
            .iter()
            .map(|clone| scope.spawn(move || claims::claim(clone, "origin", "shared", "agent")))
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
                scope.spawn(move || claims::claim(clone, "origin", &format!("own-{index}"), "a"))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("thread finishes")).collect()
    });
    assert!(distinct.iter().all(Result::is_ok), "different tasks all succeed: {distinct:?}");
    assert_eq!(claimed(&clones[0]).len(), 5, "shared plus four own claims");
    std::fs::remove_dir_all(&root).expect("cleanup");
}
