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
use xtask::hook::{Event, hook_root, tree_fingerprint};
use xtask::hook::{Gate, ROOT_VARIABLE, base_tree, checkout_around, gate_decision, gate_state};
use xtask::issue::github::{Comment, GitHub, Issue, LabelEvent};
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

/// A stand-in for `cargo`: it records where it runs, the checkout it is handed and the toolchain
/// overrides it inherits, then runs the xtask under test. While `broken-xtask` exists it fails
/// like an xtask that does not build; while `missing-toolchain` exists, like rustup without the
/// pinned toolchain; while `exit-without-reading` exists, it exits with the code that file
/// holds, without reading its payload.
const FAKE_CARGO: &str = r#"#!/bin/sh
mkdir -p target
echo "$PWD|$NAMI_HOOK_ROOT|$RUSTUP_TOOLCHAIN|$RUSTUP_TOOLCHAIN_SOURCE|$*" >> target/cargo-calls
if [ -e broken-xtask ]; then
    echo "error: could not compile xtask" >&2
    exit 101
fi
if [ -e missing-toolchain ]; then
    echo "error: toolchain 'pinned' is not installed" >&2
    exit 1
fi
if [ -e exit-without-reading ]; then
    exit "$(cat exit-without-reading)"
fi
shift
exec "$NAMI_TEST_XTASK" "$@"
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
        fs::create_dir_all(scratch.directory.join("bin"))
            .expect("the scratch directory is writable");
        for (tool_name, script) in [("just", FAKE_JUST), ("cargo", FAKE_CARGO)] {
            let tool = scratch.directory.join("bin").join(tool_name);
            fs::write(&tool, script).expect("the scratch directory is writable");
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("it is ours");
        }
        scratch
    }

    /// A second worktree of the checkout, on a new branch `name` that starts at `HEAD`.
    fn worktree(&self, name: &str) -> PathBuf {
        let path = self.directory.join(name);
        let path_text = path.to_str().expect("the path is UTF-8");
        self.git(&["worktree", "add", "--quiet", "-b", name, path_text]);
        fs::canonicalize(path).expect("the worktree exists")
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

    /// Runs `xtask hook <event>` in the checkout, as `.claude/settings.json` does in the project
    /// directory, with `payload` on stdin and the fake `just` and `cargo` first on `PATH`.
    fn hook(&self, event: &str, payload: &str, environment: &[(&str, &Path)]) -> Output {
        self.hook_in(&self.root(), event, payload, environment)
    }

    /// Runs `xtask hook <event>` like [`Scratch::hook`], but in `directory`.
    fn hook_in(
        &self,
        directory: &Path,
        event: &str,
        payload: &str,
        environment: &[(&str, &Path)],
    ) -> Output {
        let path = std::env::var("PATH").expect("PATH is set");
        let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["hook", event])
            .current_dir(directory)
            .env("PATH", format!("{}:{path}", self.directory.join("bin").display()))
            .env("NAMI_TEST_XTASK", env!("CARGO_BIN_EXE_xtask"))
            .env_remove(ROOT_VARIABLE)
            .env_remove("CLAUDE_PROJECT_DIR")
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

/// The calls the fake `cargo` recorded in `checkout`:
/// `<directory>|<root>|<toolchain>|<toolchain source>|<args>`.
fn cargo_calls(checkout: &Path) -> Vec<String> {
    let calls = fs::read_to_string(checkout.join("target/cargo-calls")).unwrap_or_default();
    calls.lines().map(str::to_owned).collect()
}

// Goal: a subagent in its own worktree is gated in that worktree, by that worktree's xtask,
// with state of its own, although the hook command runs in the project directory. Method: the
// hook runs in the main checkout with a Stop payload whose `cwd` is a second worktree; the fake
// `cargo` shows the hand-over, and the fake `just` shows where the gate ran.
#[test]
fn stop_gates_the_worktree_in_the_payload() {
    let scratch = Scratch::new("stop-worktree");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let worktree = scratch.worktree("work");
    fs::create_dir_all(worktree.join("src")).unwrap();
    fs::write(worktree.join("src/new.rs"), "fn new() {}\n").unwrap();
    fs::write(worktree.join("violation.txt"), "x\n").unwrap();
    let payload = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, worktree.join("src").display());
    let toolchain = [
        ("RUSTUP_TOOLCHAIN", Path::new("the-project-toolchain")),
        ("RUSTUP_TOOLCHAIN_SOURCE", Path::new("env")),
    ];
    let blocked = scratch.hook("stop", &payload, &toolchain);
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    assert_eq!(blocked.status.code(), Some(2), "the worktree's violation blocks: {stderr}");
    assert!(stderr.contains("blocked stop 1 of 3"), "{stderr}");
    assert!(stderr.contains("violation.txt must not exist"), "{stderr}");
    let handed = format!("{0}|{0}|||xtask hook stop", worktree.display());
    assert_eq!(
        cargo_calls(&worktree),
        std::slice::from_ref(&handed),
        "the worktree's own xtask ran"
    );
    assert_eq!(cargo_calls(&project), Vec::<String>::new(), "the project's xtask handed over");
    let just_calls = fs::read_to_string(worktree.join("target/just-calls")).unwrap();
    assert_eq!(just_calls, "check dylint\n", "the gate ran in the worktree");
    assert!(!project.join("target/just-calls").exists(), "and not in the project");
    assert!(gate_state(&worktree).join("attempts-s1").exists(), "the count is the worktree's");

    // A delegated stop that gives up is not taken for a failed hand-over and gated again.
    for attempt in 2..=3 {
        let again = scratch.hook("stop", &payload, &[]);
        assert_eq!(again.status.code(), Some(2), "blocked again");
        let again = String::from_utf8_lossy(&again.stderr);
        assert!(again.contains(&format!("blocked stop {attempt} of 3")), "{again}");
    }
    let gave_up = scratch.hook("stop", &payload, &[]);
    let gave_up_stderr = String::from_utf8_lossy(&gave_up.stderr);
    assert_eq!(gave_up.status.code(), Some(1), "the fourth stop gives up: {gave_up_stderr}");
    assert!(gave_up_stderr.starts_with("STOP GATE STILL FAILING"), "{gave_up_stderr}");
    assert!(!gave_up_stderr.contains("did not run the hook"), "it ran: {gave_up_stderr}");
    assert!(!gate_state(&worktree).join("attempts-s1").exists(), "no fallback counted again");
    let gate_runs = fs::read_to_string(worktree.join("target/just-calls")).unwrap();
    assert_eq!(gate_runs.lines().count(), 4, "one gate run per stop");

    fs::remove_file(worktree.join("violation.txt")).unwrap();
    assert_eq!(scratch.hook("stop", &payload, &[]).status.code(), Some(0), "fixed");
    let passed = fs::read_to_string(gate_state(&worktree).join("passed")).unwrap();
    assert_eq!(Some(passed.clone()), tree_fingerprint(&worktree), "the worktree's pass");
    assert!(!gate_state(&worktree).join("attempts-s1").exists(), "a pass clears the count");
    assert!(!gate_state(&project).join("passed").exists(), "the project has passed nothing");

    let in_project = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, project.display());
    scratch.write("violation.txt", "x\n");
    assert_eq!(scratch.hook("stop", &in_project, &[]).status.code(), Some(2), "the project");
    assert_eq!(scratch.gate_runs(), 1, "is gated in itself");
    assert_eq!(cargo_calls(&project), Vec::<String>::new(), "by the xtask that runs there");
    let kept = fs::read_to_string(gate_state(&worktree).join("passed")).unwrap();
    assert_eq!(kept, passed, "and the worktree's pass is untouched");
    assert_eq!(cargo_calls(&worktree), vec![handed; 5], "one hand-over per worktree stop");
}

// Goal: an edit is checked in the worktree that holds the file, whatever the `cwd` says; a
// worktree whose xtask does not build still gets the check and is told about it; session start
// reports on the `cwd`'s worktree. Method: as above, with PostToolUse and SessionStart payloads.
#[test]
fn edits_and_session_starts_use_their_own_worktree() {
    let scratch = Scratch::new("edit-worktree");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let worktree = scratch.worktree("work");
    let draft = worktree.join("tests/snapshots/a.snap.new");
    fs::create_dir_all(draft.parent().expect("a file has a parent")).unwrap();
    fs::write(&draft, "draft\n").unwrap();
    let payload = format!(
        r#"{{"cwd":"{}","tool_input":{{"file_path":"{}"}}}}"#,
        project.display(),
        draft.display()
    );
    let edited = scratch.hook("post-edit", &payload, &[]);
    let stderr = String::from_utf8_lossy(&edited.stderr);
    assert_eq!(edited.status.code(), Some(2), "the draft is a violation: {stderr}");
    assert!(stderr.contains("tests/snapshots/a.snap.new:0: error[snapshot-draft]"), "{stderr}");
    let handed = format!("{0}|{0}|||xtask hook post-edit", worktree.display());
    assert_eq!(cargo_calls(&worktree), [handed], "checked by the worktree's xtask");

    fs::write(worktree.join("broken-xtask"), "").unwrap();
    let fallback = scratch.hook("post-edit", &payload, &[]);
    let told = String::from_utf8_lossy(&fallback.stderr);
    assert_eq!(fallback.status.code(), Some(2), "a broken xtask is feedback: {told}");
    assert!(told.contains("could not compile xtask"), "cargo's error is shown: {told}");
    assert!(told.contains("did not run the hook"), "{told}");
    assert!(told.contains("the toolchain that"), "{told}");
    assert!(told.contains("pins was not used"), "the fallback names the toolchain: {told}");
    assert!(told.contains("error[snapshot-draft]"), "and the file is still checked: {told}");
    fs::remove_file(&draft).unwrap();
    let clean = scratch.hook("post-edit", &payload.replace(".snap.new", ".snap"), &[]);
    assert_eq!(clean.status.code(), Some(2), "a broken xtask is feedback even for a clean file");
    fs::remove_file(worktree.join("broken-xtask")).unwrap();

    let start = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, worktree.display());
    let started = scratch.hook("session-start", &start, &[]);
    assert_eq!(started.status.code(), Some(0), "findings do not fail the hook");
    let started_in = cargo_calls(&worktree).last().cloned().unwrap_or_default();
    assert!(started_in.ends_with("|xtask hook session-start"), "handed over: {started_in}");
    let stdout = String::from_utf8_lossy(&started.stdout);
    assert!(stdout.contains("error[doctor]: "), "the worktree's findings are shown: {stdout}");
    assert_eq!(cargo_calls(&project), Vec::<String>::new(), "the project never ran cargo");
}

/// A second worktree of `scratch` on the branch `work`, with committed-to-be work in `src/` and
/// a planted `violation.txt`, and the Stop payload of a session whose `cwd` is in it.
fn worktree_with_violation(scratch: &Scratch) -> (PathBuf, String) {
    let worktree = scratch.worktree("work");
    fs::create_dir_all(worktree.join("src")).expect("the worktree is writable");
    fs::write(worktree.join("src/new.rs"), "fn new() {}\n").expect("the worktree is writable");
    fs::write(worktree.join("violation.txt"), "x\n").expect("the worktree is writable");
    let payload = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, worktree.display());
    (worktree, payload)
}

// Goal: a hand-over that exits 1 without running the hook, as rustup does when the pinned
// toolchain is missing, never lets a failing stop through: exit 1 would not block. Method: the
// fake `cargo` exits 1; the hook gates the worktree itself, blocks three times and then gives
// up like any gate, and it says that the worktree's toolchain was not used.
#[test]
fn a_hand_over_that_exits_1_falls_back_to_the_gate() {
    let scratch = Scratch::new("stop-exit-1");
    let (worktree, payload) = worktree_with_violation(&scratch);
    fs::write(worktree.join("missing-toolchain"), "").unwrap();
    for attempt in 1..=3 {
        let blocked = scratch.hook("stop", &payload, &[]);
        let stderr = String::from_utf8_lossy(&blocked.stderr);
        assert_eq!(blocked.status.code(), Some(2), "the violation blocks: {stderr}");
        assert!(stderr.contains("toolchain 'pinned' is not installed"), "rustup's error: {stderr}");
        assert!(stderr.contains("did not run the hook (exit status: 1)"), "{stderr}");
        assert!(stderr.contains("pins was not used"), "the fallback is named: {stderr}");
        assert!(stderr.contains(&format!("blocked stop {attempt} of 3")), "{stderr}");
        assert!(stderr.contains("violation.txt must not exist"), "the gate ran: {stderr}");
    }
    let gave_up = scratch.hook("stop", &payload, &[]);
    let stderr = String::from_utf8_lossy(&gave_up.stderr);
    assert_eq!(gave_up.status.code(), Some(1), "then it gives up, loudly: {stderr}");
    assert!(stderr.contains("STOP GATE STILL FAILING"), "{stderr}");
    let gate_runs = fs::read_to_string(worktree.join("target/just-calls")).unwrap();
    assert_eq!(gate_runs.lines().count(), 4, "the gate ran in the worktree every time");
    assert_eq!(cargo_calls(&worktree).len(), 4, "after a hand-over was tried every time");
}

// Goal: only a worktree of the project's own repository is handed over; another clone, such as
// a fork's review checkout, is checked by the project's xtask as before, so that the hooks never
// run code that the project does not hold. Method: a clone with a planted violation and an
// unreviewed snapshot; the fake `cargo` would record any hand-over.
#[test]
fn another_clone_is_checked_by_the_project_xtask() {
    let scratch = Scratch::new("foreign-clone");
    let clone_path = scratch.directory.join("clone");
    scratch.git(&["clone", "--quiet", ".", clone_path.to_str().expect("the path is UTF-8")]);
    let clone = fs::canonicalize(&clone_path).expect("the clone exists");
    git(&clone, &["switch", "--quiet", "--create", "work"]);
    fs::write(clone.join("violation.txt"), "x\n").unwrap();
    let payload = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, clone.display());
    let stopped = scratch.hook("stop", &payload, &[]);
    let stderr = String::from_utf8_lossy(&stopped.stderr);
    assert_eq!(stopped.status.code(), Some(2), "the clone is gated: {stderr}");
    assert!(!stderr.contains("did not run the hook"), "no hand-over was tried: {stderr}");
    let gate_runs = fs::read_to_string(clone.join("target/just-calls")).unwrap();
    assert_eq!(gate_runs, "check dylint\n", "the gate ran in the clone");

    let draft = clone.join("a.snap.new");
    fs::write(&draft, "draft\n").unwrap();
    let edit = format!(r#"{{"tool_input":{{"file_path":"{}"}}}}"#, draft.display());
    let edited = scratch.hook("post-edit", &edit, &[]);
    let edit_stderr = String::from_utf8_lossy(&edited.stderr);
    assert_eq!(edited.status.code(), Some(2), "the clone's file is checked: {edit_stderr}");
    assert!(edit_stderr.contains("a.snap.new:0: error[snapshot-draft]"), "{edit_stderr}");
    assert_eq!(cargo_calls(&clone), Vec::<String>::new(), "the clone's xtask never ran");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    assert_eq!(cargo_calls(&project), Vec::<String>::new(), "and nothing was built");
}

// Goal: a delegated hook works on the checkout that `NAMI_HOOK_ROOT` names, not on the one the
// payload names, and refuses a root it cannot trust. Method: run the hook in the worktree with
// the root set and a payload about the clean project: honoured, the worktree's violation
// blocks; ignored, the hook would hand over to the project, whose gate passes.
#[test]
fn a_delegated_hook_honours_and_checks_its_root() {
    let scratch = Scratch::new("delegated-root");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let (worktree, _payload) = worktree_with_violation(&scratch);
    let about_project = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, project.display());
    let root = [(ROOT_VARIABLE, worktree.as_path())];
    let delegated = scratch.hook_in(&worktree, "stop", &about_project, &root);
    let stderr = String::from_utf8_lossy(&delegated.stderr);
    assert_eq!(delegated.status.code(), Some(2), "the named worktree is gated: {stderr}");
    assert!(stderr.contains("violation.txt must not exist"), "{stderr}");
    assert!(worktree.join("target/just-calls").exists(), "the gate ran in the worktree");
    assert!(!project.join("target/just-calls").exists(), "and not in the project");
    assert_eq!(cargo_calls(&project), Vec::<String>::new(), "nothing was handed on");

    let refused = |name: &Path, expected: &str| {
        let output = scratch.hook_in(&worktree, "stop", &about_project, &[(ROOT_VARIABLE, name)]);
        let refusal = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(output.status.code(), Some(1), "a root that is not trusted fails: {refusal}");
        assert!(refusal.contains(expected), "{expected}: {refusal}");
    };
    refused(Path::new("work"), "is not an absolute path");
    refused(&worktree.join("src"), "is not the toplevel of a nami checkout");
    refused(&project, "is not the checkout this xtask runs in");
    let gate_runs = fs::read_to_string(worktree.join("target/just-calls")).unwrap();
    assert_eq!(gate_runs.lines().count(), 1, "a refused root runs no gate");
}

// Goal: a git repository nested in a checkout, such as a test's scratch repository, belongs to
// the checkout around it, as it did before the hooks used git's toplevel.
#[test]
fn a_nested_repository_belongs_to_the_enclosing_checkout() {
    let scratch = Scratch::new("nested");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let nested = project.join("target/tmp/nested");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "--quiet"]);
    assert_eq!(git_output(&nested, &["rev-parse", "--show-toplevel"]), nested.to_str().unwrap());
    let file = nested.join("src/lib.rs");
    assert_eq!(checkout_around(&file), Some(project.clone()), "the enclosing checkout");
    let edit = format!(r#"{{"tool_input":{{"file_path":"{}"}}}}"#, file.display());
    assert_eq!(hook_root(Event::PostEdit, &edit, None), Some(project));
    assert_eq!(checkout_around(&scratch.directory), None, "outside every checkout: none");
}

// Goal: a hand-over counts only if the other hook ran: 0 and 2 for every event, and the
// give-up code 3 only from a stop, which then exits 1 without a second gate. Anything else
// falls back. A payload that the other side did not read whole is trusted only from a hook
// that passed or blocked. Method: the fake `cargo` exits with a given code without reading;
// a large payload cannot fit in the pipe, so its write breaks.
#[test]
fn only_a_hook_that_ran_counts_as_a_hand_over() {
    let scratch = Scratch::new("exit-codes");
    let (worktree, stop) = worktree_with_violation(&scratch);
    let draft = worktree.join("a.snap.new");
    fs::write(&draft, "draft\n").unwrap();
    let edit = format!(r#"{{"tool_input":{{"file_path":"{}"}}}}"#, draft.display());
    let padding = "x".repeat(1 << 20);
    let large_edit = edit.replace("}}", &format!(r#"}},"padding":"{padding}"}}"#));
    let large_stop = stop.replace('}', &format!(r#","padding":"{padding}"}}"#));
    let fallback = "pins was not used";
    // The exit code and whether the hook fell back, and its stderr for the messages.
    let run = |code: &str, event: &str, payload: &str| {
        fs::write(worktree.join("exit-without-reading"), code).unwrap();
        let output = scratch.hook(event, payload, &[]);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        ((output.status.code(), stderr.contains(fallback)), stderr)
    };
    for payload in [edit.as_str(), large_edit.as_str()] {
        let (passed, passed_stderr) = run("0", "post-edit", payload);
        assert_eq!(passed, (Some(0), false), "a pass is passed on: {passed_stderr}");
        let (blocked, blocked_stderr) = run("2", "post-edit", payload);
        assert_eq!(blocked, (Some(2), false), "a block is passed on: {blocked_stderr}");
    }
    for code in ["1", "3", "101"] {
        let (failed, stderr) = run(code, "post-edit", &edit);
        assert_eq!(failed, (Some(2), true), "exit {code} falls back: {stderr}");
        assert!(stderr.contains("error[snapshot-draft]"), "and the file is checked: {stderr}");
    }
    let (gave_up, gave_up_stderr) = run("3", "stop", &stop);
    assert_eq!(gave_up, (Some(1), false), "a stop that gave up exits 1: {gave_up_stderr}");
    assert!(!worktree.join("target/just-calls").exists(), "and is not gated again");
    let (unread, unread_stderr) = run("3", "stop", &large_stop);
    assert_eq!(unread, (Some(2), true), "an unread payload falls back: {unread_stderr}");
    assert!(unread_stderr.contains("Broken pipe"), "{unread_stderr}");
    assert!(unread_stderr.contains("blocked stop 1 of 3"), "it is gated: {unread_stderr}");
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

/// GitHub with the issues #12 by the bot and #13 by an outsider.
struct Issues;

impl GitHub for Issues {
    fn issue(&self, number: u32) -> Result<Issue, String> {
        let author = match number {
            12 => "nami-agent[bot]",
            13 => "outsider",
            _ => return Err(format!("no issue #{number}")),
        };
        Ok(Issue {
            number,
            title: "A gap".to_owned(),
            author: author.to_owned(),
            is_open: true,
            is_pull_request: false,
            labels: vec!["guardrail-gap".to_owned()],
            body: String::new(),
        })
    }
    fn open_issues(&self) -> Result<Vec<Issue>, String> {
        Err("not used".to_owned())
    }
    fn comments(&self, _number: u32) -> Result<Vec<Comment>, String> {
        Err("not used".to_owned())
    }
    fn label_events(&self, _number: u32) -> Result<Vec<LabelEvent>, String> {
        Err("not used".to_owned())
    }
    fn create(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<u32, String> {
        Err("not used".to_owned())
    }
}

// Goal: `pr-body` checks a description against the real changes, the code owners on the base
// and the issues on GitHub, and a check that cannot run reports a problem instead of passing.
#[test]
fn pr_body_is_checked_against_the_checkout() {
    let scratch = scratch_with_protected_changes("pr-body");
    let root = scratch.root();
    let on_main = ["--base", "main"];
    let check = |options: &[&str], body: Option<&str>, branch: &str| {
        body_problems(&root, options, body, branch, &Issues)
    };
    let body = "## Protected files\n- xtask/: hooks\n- crates/a/Cargo.toml: a crate\n\n\
                ## Follow-up issues\n- #12\n";
    assert_eq!(check(&on_main, Some(body), "work"), Vec::<String>::new(), "complete");
    let outsider = check(&on_main, Some(&body.replace("#12", "#13, #14")), "work");
    assert_eq!(outsider.len(), 2, "{outsider:?}");
    assert!(outsider[0].contains("#13, listed") && outsider[0].contains("neither the bot"));
    assert!(outsider[1].contains("#14, listed") && outsider[1].contains("no issue #14"));
    let unclosed = check(&on_main, Some(body), "task/issue-12");
    assert_eq!(unclosed.len(), 1, "{unclosed:?}");
    assert!(unclosed[0].contains("add the line `Fixes #12`"), "{unclosed:?}");
    let closed = format!("Fixes #12.\n\n{body}");
    assert_eq!(check(&on_main, Some(&closed), "task/issue-12"), Vec::<String>::new());

    let follow_ups_only = Some("## Follow-up issues\nNone\n");
    let unlisted = check(&on_main, follow_ups_only, "work");
    assert_eq!(unlisted.len(), 1, "protected paths changed: {unlisted:?}");
    assert!(unlisted[0].contains("the section `## Protected files` is missing"), "{unlisted:?}");
    let on_head = check(&["--base", "HEAD"], follow_ups_only, "work");
    assert_eq!(on_head, Vec::<String>::new(), "no protected change, nothing to list");

    let unset = check(&on_main, None, "work");
    assert_eq!(unset.len(), 1, "{unset:?}");
    assert!(unset[0].starts_with("error[pr-body]: the environment variable PR_BODY is not set; "));
    let no_base = check(&[], Some(body), "work");
    assert_eq!(no_base.len(), 1, "{no_base:?}");
    assert!(no_base[0].starts_with("error[pr-body]: `git diff --name-only origin/main...HEAD`"));
    let options = check(&["--bass", "main"], Some(body), "work");
    assert_eq!(options.len(), 1, "{options:?}");
    assert!(options[0].starts_with("error[pr-body]: cannot read the options `--bass main`; "));

    // The code owners come from the base, not from the PR's tree.
    scratch.git(&["switch", "--quiet", "main"]);
    scratch.git(&["rm", "--quiet", ".github/CODEOWNERS"]);
    scratch.commit("no code owners on main");
    scratch.git(&["switch", "--quiet", "work"]);
    let no_owners = check(&on_main, Some(body), "work");
    assert_eq!(no_owners.len(), 1, "{no_owners:?}");
    assert!(no_owners[0].contains("cannot read .github/CODEOWNERS on main"), "{no_owners:?}");
    let none = check(&on_main, follow_ups_only, "work");
    assert!(none.iter().all(|problem| !problem.contains("CODEOWNERS on")), "{none:?}");
}
