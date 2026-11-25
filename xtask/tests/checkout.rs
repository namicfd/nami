//! The stop gate, the session-start hook and `protected`, run against real git checkouts.
//!
//! Each test builds a throwaway checkout that looks like nami to the hooks: it has a
//! `rust-toolchain.toml` and an `xtask/` directory, and git ignores its `target/`.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use xtask::hook::{Gate, base_tree, gate_decision, tree_fingerprint};
use xtask::pr::{body_problems, changed_protected};

/// A stand-in for `just`: it records its arguments and fails while `violation.txt` exists.
const FAKE_JUST: &str = r#"#!/bin/sh
mkdir -p target
echo "$*" >> target/just-calls
if [ -e violation.txt ]; then
    echo "error: violation.txt must not exist"
    exit 1
fi
"#;

/// Runs git in `directory` with a fixed identity and without the user's hooks or signing.
fn git_output(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=nami-test", "-c", "user.email=nami-test@example.invalid"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("git starts");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "git {arguments:?} failed: {stderr}");
    String::from_utf8(output.stdout).expect("git prints UTF-8").trim().to_owned()
}

fn git(directory: &Path, arguments: &[&str]) {
    let _stdout = git_output(directory, arguments);
}

/// What one run of the stop hook did.
struct Stopped {
    code: Option<i32>,
    stderr: String,
}

/// A throwaway directory with a checkout on `main` and a fake `just`. Removed on drop.
struct Scratch {
    directory: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir().join(format!("nami-{name}-{}", std::process::id()));
        if directory.exists() {
            fs::remove_dir_all(&directory).expect("a stale scratch directory is removable");
        }
        let scratch = Self { directory };
        scratch.write(".gitignore", "/target/\n");
        scratch.write("rust-toolchain.toml", "[toolchain]\n");
        scratch.write("xtask/README", "tooling\n");
        scratch.git(&["init", "--quiet", "--initial-branch=main"]);
        scratch.commit("base");
        let just = scratch.directory.join("bin/just");
        fs::create_dir_all(scratch.directory.join("bin"))
            .expect("the scratch directory is writable");
        fs::write(&just, FAKE_JUST).expect("the scratch directory is writable");
        fs::set_permissions(&just, fs::Permissions::from_mode(0o755)).expect("just is ours");
        scratch
    }

    fn root(&self) -> PathBuf {
        self.directory.join("checkout")
    }

    fn write(&self, path: &str, text: &str) {
        let file = self.root().join(path);
        let parent = file.parent().expect("a file has a parent");
        fs::create_dir_all(parent).expect("the scratch directory is writable");
        fs::write(&file, text).expect("the scratch directory is writable");
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.root().join(path)).unwrap_or_default()
    }

    fn git(&self, arguments: &[&str]) {
        git(&self.root(), arguments);
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--message", message]);
    }

    fn head_tree(&self) -> String {
        git_output(&self.root(), &["rev-parse", "HEAD^{tree}"])
    }

    /// Runs `xtask hook <event>` with `payload` on stdin and the fake `just` first on `PATH`.
    fn hook(&self, event: &str, payload: &str, environment: &[(&str, &Path)]) -> Output {
        let path = std::env::var("PATH").expect("PATH is set");
        let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["hook", event])
            .env("PATH", format!("{}:{path}", self.directory.join("bin").display()))
            .envs(environment.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("xtask starts");
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(payload.as_bytes()).expect("xtask reads its payload");
        drop(stdin);
        child.wait_with_output().expect("xtask finishes")
    }

    /// Stops the agent of `session` in the checkout.
    fn stop(&self, session: Option<&str>, stop_hook_active: bool) -> Stopped {
        let session = session.map_or_default(|id| format!(r#""session_id":"{id}","#));
        let root = self.root();
        let payload = format!(
            r#"{{{session}"cwd":"{}","stop_hook_active":{stop_hook_active}}}"#,
            root.display()
        );
        let output = self.hook("stop", &payload, &[]);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Stopped { code: output.status.code(), stderr }
    }

    /// How often the gate ran: the fake `just` writes one line per call.
    fn gate_runs(&self) -> usize {
        self.read("target/just-calls").lines().count()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.directory) {
            eprintln!("cannot remove {}: {error}", self.directory.display());
        }
    }
}

// Goal: the fingerprint names the working state, whether it is committed or not. Method: in a
// real checkout, compare it with the tree of `HEAD` when clean, after ignored and untracked
// files appear, and after a commit, when it must equal the value from before the commit.
#[test]
fn fingerprint_names_the_working_state() {
    let scratch = Scratch::new("fingerprint");
    let root = scratch.root();
    let clean = tree_fingerprint(&root).expect("git names a clean state");
    assert_eq!(clean, scratch.head_tree(), "a clean checkout is the tree of HEAD");
    scratch.write("target/debug/build.log", "ignored\n");
    assert_eq!(tree_fingerprint(&root), Some(clean.clone()), "ignored files do not count");

    scratch.write("src/new.rs", "fn new() {}\n");
    let stored = tree_fingerprint(&root).expect("git names a changed state");
    assert_ne!(stored, clean, "an untracked file changes the state");
    let status = git_output(&root, &["status", "--porcelain"]);
    assert_eq!(status, "?? src/", "the checkout's own index is untouched");

    scratch.commit("add new.rs");
    assert_eq!(tree_fingerprint(&root), Some(stored.clone()), "a commit keeps the state");
    assert_eq!(stored, scratch.head_tree(), "and the state is now the tree of HEAD");

    scratch.write("xtask/README", "changed\n");
    let modified = tree_fingerprint(&root).expect("git names a modified state");
    assert_ne!(modified, stored, "a modified tracked file changes the state");
    fs::remove_file(root.join("xtask/README")).unwrap();
    let deleted = tree_fingerprint(&root).expect("git names a state with a deletion");
    assert_ne!(deleted, modified, "a deleted tracked file changes the state");
    assert_ne!(deleted, stored, "and it is not the committed state either");

    let leftovers = fs::read_dir(root.join("target/stop-gate")).unwrap().count();
    assert_eq!(leftovers, 0, "the scratch index is removed after use");
    assert_eq!(tree_fingerprint(&scratch.directory), None, "outside a repository: no state");
}

// Goal: the run-or-skip decision on real states. Method: walk a checkout through the workflow:
// the unchanged base is skipped; committed work runs the gate, which is the case the old hook
// missed; the same state is skipped once it has passed; new work runs the gate again.
#[test]
fn committed_work_runs_the_gate_until_it_passes() {
    let scratch = Scratch::new("decision");
    let root = scratch.root();
    let decide = |passed: Option<&str>| {
        gate_decision(tree_fingerprint(&root).as_deref(), base_tree(&root).as_deref(), passed)
    };
    assert_eq!(base_tree(&root), None, "`main` is not its own base");
    assert_eq!(decide(None), Gate::Run, "so work on `main` is checked");

    scratch.git(&["switch", "--quiet", "--create", "work"]);
    assert_eq!(base_tree(&root), Some(scratch.head_tree()), "the base is where `work` started");
    assert_eq!(decide(None), Gate::Skip, "nothing changed since `main`");

    scratch.write("src/new.rs", "fn new() {}\n");
    scratch.commit("work");
    assert_eq!(git_output(&root, &["status", "--porcelain"]), "", "the work is committed");
    assert_ne!(base_tree(&root), Some(scratch.head_tree()), "the base stays behind");
    assert_eq!(decide(None), Gate::Run, "committed work is checked");

    let passed = tree_fingerprint(&root);
    assert_eq!(decide(passed.as_deref()), Gate::Skip, "a passing state is not checked again");
    scratch.write("src/more.rs", "fn more() {}\n");
    assert_eq!(decide(passed.as_deref()), Gate::Run, "new work is checked");
}

// Goal: without a local `main` the base is `origin/main`, and on `main` it is `origin/main`
// too, so that commits made directly on `main` are checked. Method: a clone of a checkout
// whose `HEAD` is a work branch has only `origin/main` at first.
#[test]
fn base_falls_back_to_origin_main() {
    let scratch = Scratch::new("base");
    let main_tree = scratch.head_tree();
    scratch.git(&["switch", "--quiet", "--create", "work"]);
    scratch.write("src/new.rs", "fn new() {}\n");
    scratch.commit("work");
    let clone = scratch.directory.join("clone");
    scratch.git(&["clone", "--quiet", ".", clone.to_str().expect("the path is UTF-8")]);
    let branches = git_output(&clone, &["branch", "--format=%(refname:short)"]);
    assert_eq!(branches, "work", "the clone has no local `main`");
    assert_eq!(base_tree(&clone), Some(main_tree.clone()), "the base comes from `origin/main`");

    git(&clone, &["switch", "--quiet", "main"]);
    assert_eq!(base_tree(&clone), Some(main_tree.clone()), "on `main`: still `origin/main`");
    fs::write(clone.join("direct.rs"), "fn direct() {}\n").unwrap();
    git(&clone, &["add", "--all"]);
    git(&clone, &["commit", "--quiet", "--message", "directly on main"]);
    assert_eq!(base_tree(&clone), Some(main_tree), "a commit on `main` does not move the base");
    let fingerprint = tree_fingerprint(&clone);
    let decision = gate_decision(fingerprint.as_deref(), base_tree(&clone).as_deref(), None);
    assert_eq!(decision, Gate::Run, "so a commit on `main` is checked");
}

// Goal: the stop hook is a real gate, end to end. Method: pipe Stop payloads into the xtask
// binary, in a checkout whose fake `just` fails while `violation.txt` exists. The unchanged base
// may stop. A committed violation blocks three times, whatever `stop_hook_active` says; the
// fourth stop gives up with exit code 1; then the count starts again.
#[test]
fn stop_blocks_a_committed_violation_then_gives_up() {
    let scratch = Scratch::new("stop-blocks");
    scratch.git(&["switch", "--quiet", "--create", "work"]);
    assert_eq!(scratch.stop(Some("s1"), false).code, Some(0), "the unchanged base may stop");
    assert_eq!(scratch.gate_runs(), 0, "and the gate does not run for it");

    scratch.write("violation.txt", "x\n");
    scratch.commit("violation");
    for attempt in 1..=3 {
        let Stopped { code, stderr } = scratch.stop(Some("s1"), attempt > 1);
        assert_eq!(code, Some(2), "a failing gate blocks: {stderr}");
        assert!(stderr.contains(&format!("blocked stop {attempt} of 3")), "{stderr}");
        assert!(stderr.contains("violation.txt must not exist"), "the gate's output: {stderr}");
    }
    let other = scratch.stop(Some("s2"), false);
    assert_eq!(other.code, Some(2), "another session is blocked too: {}", other.stderr);
    assert!(other.stderr.contains("blocked stop 1 of 3"), "and counts for itself");

    let counter = scratch.root().join("target/stop-gate/attempts-s1");
    assert_eq!(fs::read_to_string(&counter).unwrap(), "3", "three failures are on record");
    let fourth = scratch.stop(Some("s1"), true);
    assert_eq!(fourth.code, Some(1), "the fourth failure gives up: {}", fourth.stderr);
    assert!(fourth.stderr.starts_with("STOP GATE STILL FAILING"), "{}", fourth.stderr);
    assert!(fourth.stderr.contains("The work is NOT done, and CI will reject it."), "loud");
    assert!(fourth.stderr.contains("violation.txt must not exist"), "the gate's output");
    assert!(!counter.exists(), "giving up clears the counter");

    assert_eq!(scratch.gate_runs(), 5, "every one of these stops ran the gate");
    assert_eq!(scratch.read("target/just-calls").lines().next(), Some("check dylint"));
    let fifth = scratch.stop(Some("s1"), true);
    assert_eq!(fifth.code, Some(2), "the next failure blocks again: {}", fifth.stderr);
    assert!(fifth.stderr.contains("blocked stop 1 of 3"), "and the count starts again");
}

// Goal: a passing state is recorded and not checked again, and a pass clears the session's
// counter. Method: fail once with an uncommitted violation, fix and commit, then stop three
// times: the first runs the gate, the second does not, the third runs it for new work.
#[test]
fn stop_does_not_recheck_a_passing_state() {
    let scratch = Scratch::new("stop-passes");
    let root = scratch.root();
    scratch.git(&["switch", "--quiet", "--create", "work"]);
    scratch.write("violation.txt", "x\n");
    assert_eq!(scratch.stop(Some("s1"), false).code, Some(2), "a violation blocks");
    assert!(root.join("target/stop-gate/attempts-s1").exists(), "the failure is on record");

    fs::remove_file(root.join("violation.txt")).unwrap();
    scratch.write("src/new.rs", "fn new() {}\n");
    scratch.commit("work");
    let fixed = scratch.stop(Some("s1"), true);
    assert_eq!((fixed.code, fixed.stderr.as_str()), (Some(0), ""), "the fixed state passes");
    assert_eq!(scratch.gate_runs(), 2, "after the gate ran for it");
    assert!(!root.join("target/stop-gate/attempts-s1").exists(), "a pass clears the counter");
    assert_eq!(Some(scratch.read("target/stop-gate/passed")), tree_fingerprint(&root));

    assert_eq!(scratch.stop(Some("s1"), false).code, Some(0), "the same state may stop");
    assert_eq!(scratch.stop(Some("s2"), false).code, Some(0), "in any session");
    assert_eq!(scratch.gate_runs(), 2, "and it is not checked again");
    assert_eq!(git_output(&root, &["status", "--porcelain"]), "", "the gate's state is ignored");

    scratch.write("src/more.rs", "fn more() {}\n");
    assert_eq!(scratch.stop(Some("s1"), false).code, Some(0), "new work passes");
    assert_eq!(scratch.gate_runs(), 3, "but only after the gate ran for it");
}

// Goal: without a session id there is no counter, and `stop_hook_active` is the fallback: the
// first failing stop blocks, the second gives up. Outside a nami checkout the hook does nothing.
#[test]
fn stop_without_a_session_follows_stop_hook_active() {
    let scratch = Scratch::new("stop-no-session");
    scratch.git(&["switch", "--quiet", "--create", "work"]);
    scratch.write("violation.txt", "x\n");
    let first = scratch.stop(None, false);
    assert_eq!(first.code, Some(2), "the first failure blocks: {}", first.stderr);
    assert!(first.stderr.contains("blocked stop 3 of 3"), "and says it is the last block");
    let second = scratch.stop(None, true);
    assert_eq!(second.code, Some(1), "the second failure gives up: {}", second.stderr);
    assert!(second.stderr.starts_with("STOP GATE STILL FAILING"), "{}", second.stderr);
    let state: Vec<_> = fs::read_dir(scratch.root().join("target/stop-gate")).unwrap().collect();
    assert!(state.is_empty(), "no counter and no pass are stored: {state:?}");

    let outside = scratch.hook("stop", r#"{"session_id":"s1","cwd":"/"}"#, &[]);
    assert_eq!(outside.status.code(), Some(0), "not a nami checkout: nothing to gate");
    assert_eq!(scratch.gate_runs(), 2, "and the gate does not run for it");
}

// Goal: the session-start hook puts cargo's tools on the session's `PATH` once, and only if
// they are missing; doctor findings go to stdout and do not fail the hook. Method: run the
// binary with a scratch env file. The scratch checkout pins no tools, so the doctor has findings.
#[test]
fn session_start_extends_path_and_reports_findings() {
    let scratch = Scratch::new("session-start");
    let env_file = scratch.directory.join("env");
    let home = std::env::var("HOME").expect("HOME is set");
    let payload = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, scratch.root().display());
    let start = |session_path: &str| {
        let environment = [
            ("CLAUDE_ENV_FILE", env_file.as_path()),
            ("NAMI_SESSION_PATH", Path::new(session_path)),
        ];
        scratch.hook("session-start", &payload, &environment)
    };

    let output = start(&format!("/usr/bin:{home}/.cargo/bin:/bin"));
    assert_eq!(output.status.code(), Some(0), "findings do not fail the hook");
    assert!(!env_file.exists(), "a `PATH` that lists cargo's tools is left alone");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let findings: Vec<&str> = stdout.lines().collect();
    assert_eq!(findings.len(), 2, "no pinned tools and no pinned nightly: {stdout}");
    let no_tools = "error[doctor]: cannot read the pinned tools from .github/actions/setup/";
    assert!(findings[0].starts_with(no_tools), "{stdout}");
    let no_nightly = "error[doctor]: lints/rust-toolchain.toml names no toolchain; ";
    assert!(findings[1].starts_with(no_nightly), "{stdout}");

    let export = "export PATH=\"$HOME/.cargo/bin:$PATH\"\n";
    assert_eq!(start("/usr/bin:/bin").status.code(), Some(0), "the hook succeeds");
    assert_eq!(fs::read_to_string(&env_file).unwrap(), export, "the export is appended");
    assert_eq!(start("/usr/bin:/bin").status.code(), Some(0), "the hook succeeds again");
    assert_eq!(fs::read_to_string(&env_file).unwrap(), export, "but only once");
}

/// A checkout whose branch `work` changes protected and unprotected files and moves a file out
/// of a protected directory, while `main` moves on with a protected change of its own.
fn scratch_with_protected_changes(name: &str) -> Scratch {
    let scratch = Scratch::new(name);
    scratch.write(".github/CODEOWNERS", "/xtask/ @owner\n**/Cargo.toml @owner\n");
    scratch.write("xtask/old.rs", "fn old() {}\n");
    scratch.commit("codeowners");
    scratch.git(&["switch", "--quiet", "--create", "work"]);
    scratch.write("xtask/hook.rs", "fn hook() {}\n");
    scratch.write("src/lib.rs", "fn lib() {}\n");
    scratch.write("crates/a/Cargo.toml", "[package]\n");
    scratch.git(&["mv", "xtask/old.rs", "src/old.rs"]);
    scratch.commit("work");
    scratch.git(&["switch", "--quiet", "main"]);
    scratch.write("xtask/main-only.rs", "fn main_only() {}\n");
    scratch.commit("main moves on");
    scratch.git(&["switch", "--quiet", "work"]);
    scratch
}

// Goal: `protected` compares `HEAD` with the merge base and applies CODEOWNERS; a base, an
// option or a CODEOWNERS file that it cannot use is an error, never an empty list.
#[test]
fn protected_changes_are_found_since_the_merge_base() {
    let scratch = scratch_with_protected_changes("protected");
    let root = scratch.root();
    let expected = ["crates/a/Cargo.toml", "xtask/hook.rs", "xtask/old.rs"];
    let on_main = ["--base", "main"];
    assert_eq!(changed_protected(&root, &on_main), Ok(expected.map(str::to_owned).to_vec()));
    let on_head = changed_protected(&root, &["--base", "HEAD"]);
    assert_eq!(on_head, Ok(Vec::new()), "nothing differs from itself");

    let no_default = changed_protected(&root, &[]).expect_err("there is no origin/main here");
    assert!(no_default.contains("origin/main...HEAD` failed"), "the default: {no_default}");
    assert!(no_default.contains("; fetch the base"), "the error says how to fix it");
    let no_base = changed_protected(&root, &["--base", "no-such-ref"]).expect_err("no such ref");
    assert!(no_base.contains("no-such-ref...HEAD` failed"), "{no_base}");
    let option = changed_protected(&root, &["--base", "--output=x"]).expect_err("not a ref");
    assert!(option.starts_with("cannot read the options `--base --output=x`"), "{option}");
    assert!(!root.join("x").exists(), "and git never saw it as an option");

    scratch.write(".github/CODEOWNERS", "*.rs @owner\n");
    let unsupported = changed_protected(&root, &on_main).expect_err("an unsupported form");
    assert!(unsupported.contains("unsupported pattern `*.rs`"), "{unsupported}");
    fs::remove_file(root.join(".github/CODEOWNERS")).unwrap();
    let unreadable = changed_protected(&root, &on_main).expect_err("CODEOWNERS must exist");
    assert!(unreadable.starts_with("cannot read .github/CODEOWNERS"), "{unreadable}");
}

// Goal: `pr-body` checks a description against the real changes and the real tree, and a check
// that cannot run reports a problem instead of passing.
#[test]
fn pr_body_is_checked_against_the_checkout() {
    let scratch = scratch_with_protected_changes("pr-body");
    let root = scratch.root();
    let on_main = ["--base", "main"];
    let body = "## Protected files\n- xtask/: hooks\n- crates/a/Cargo.toml: a crate\n\n\
                ## Guardrail gaps\n- docs/learnings/stop-gate.md\n";
    let no_learning = body_problems(&root, &on_main, Some(body));
    assert_eq!(no_learning.len(), 1, "the learning is not in the tree: {no_learning:?}");
    assert!(no_learning[0].contains("`docs/learnings/stop-gate.md`, named"), "{no_learning:?}");
    scratch.write("docs/learnings/stop-gate.md", "# The stop gate\n");
    assert_eq!(body_problems(&root, &on_main, Some(body)), Vec::<String>::new(), "complete");

    let gaps_only = Some("## Guardrail gaps\nNone\n");
    let unlisted = body_problems(&root, &on_main, gaps_only);
    assert_eq!(unlisted.len(), 1, "protected paths changed: {unlisted:?}");
    assert!(unlisted[0].contains("the section `## Protected files` is missing"), "{unlisted:?}");
    let on_head = body_problems(&root, &["--base", "HEAD"], gaps_only);
    assert_eq!(on_head, Vec::<String>::new(), "no protected change, nothing to list");

    let unset = body_problems(&root, &on_main, None);
    assert_eq!(unset.len(), 1, "{unset:?}");
    assert!(unset[0].starts_with("error[pr-body]: the environment variable PR_BODY is not set; "));
    let no_base = body_problems(&root, &[], Some(body));
    assert_eq!(no_base.len(), 1, "{no_base:?}");
    assert!(no_base[0].starts_with("error[pr-body]: `git diff --name-only origin/main...HEAD`"));
    let options = body_problems(&root, &["--bass", "main"], Some(body));
    assert_eq!(options.len(), 1, "{options:?}");
    assert!(options[0].starts_with("error[pr-body]: cannot read the options `--bass main`; "));
}
