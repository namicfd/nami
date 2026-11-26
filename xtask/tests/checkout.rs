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
use xtask::hook::{Gate, base_tree, gate_decision, gate_state, tree_fingerprint};
use xtask::issue::github::{Comment, GitHub, Issue, LabelEvent};
use xtask::pr::{body_problems, changed_protected};

/// A stand-in for `just`: it records its arguments and the toolchain overrides it inherits, and
/// fails while `violation.txt` exists.
const FAKE_JUST: &str = r#"#!/bin/sh
mkdir -p target
echo "$*" >> target/just-calls
echo "$RUSTUP_TOOLCHAIN|$RUSTUP_TOOLCHAIN_SOURCE" >> target/just-environment
if [ -e violation.txt ]; then
    echo "error: violation.txt must not exist"
    exit 1
fi
"#;

/// A stand-in for `cargo`: it records where it runs, the toolchain overrides it inherits and its
/// arguments, then runs the xtask under test, as `cargo xtask` would. While `broken-xtask`
/// exists it fails like an xtask that does not build; while `missing-toolchain` exists, like
/// rustup without the pinned toolchain; while `old-xtask` exists, like an xtask from before
/// `tidy --file`. While `worktree-flags-everything` or `worktree-passes-everything` exists, it
/// stands for a worktree whose own rules disagree with the project's: every file breaks a rule,
/// or none does.
const FAKE_CARGO: &str = r#"#!/bin/sh
mkdir -p target
echo "$PWD|$RUSTUP_TOOLCHAIN|$RUSTUP_TOOLCHAIN_SOURCE|$*" >> target/cargo-calls
if [ -e broken-xtask ]; then
    echo "error: could not compile xtask" >&2
    exit 101
fi
if [ -e missing-toolchain ]; then
    echo "error: toolchain 'pinned' is not installed" >&2
    exit 1
fi
if [ -e old-xtask ]; then
    echo "usage: cargo xtask tidy [--root <dir> [--policy <dir>]] | rules [--write]" >&2
    exit 1
fi
if [ -e worktree-flags-everything ]; then
    echo "$4:0: error[worktree-only]: a rule that only this worktree has" >&2
    exit 2
fi
if [ -e worktree-passes-everything ]; then
    exit 0
fi
shift
exec "$NAMI_TEST_XTASK" "$@"
"#;

/// A stand-in for `rustfmt`: it records where it runs, the toolchain overrides it inherits and
/// its arguments, and "formats" stdin by putting a line in front that names the toolchain it ran
/// with: the inherited override, or the one the checkout pins if there is none. While
/// `rustfmt-fails` exists, it fails like rustup without the pinned toolchain, unless an
/// override names another toolchain.
const FAKE_RUSTFMT: &str = r#"#!/bin/sh
mkdir -p target
echo "$PWD|$RUSTUP_TOOLCHAIN|$RUSTUP_TOOLCHAIN_SOURCE|$*" >> target/rustfmt-calls
if [ -e rustfmt-fails ] && [ -z "$RUSTUP_TOOLCHAIN" ]; then
    echo "error: toolchain 'pinned' is not installed" >&2
    exit 1
fi
echo "// formatted by ${RUSTUP_TOOLCHAIN:-the pinned toolchain}"
cat
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
        let tools = [("just", FAKE_JUST), ("cargo", FAKE_CARGO), ("rustfmt", FAKE_RUSTFMT)];
        for (tool_name, script) in tools {
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

    /// A second worktree of the checkout with a detached `HEAD` at `HEAD`, as `review-pr` checks
    /// out a fork's pull request.
    fn detached_worktree(&self, name: &str) -> PathBuf {
        let path = self.directory.join(name);
        let path_text = path.to_str().expect("the path is UTF-8");
        self.git(&["worktree", "add", "--quiet", "--detach", path_text, "HEAD"]);
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["hook", event])
            .current_dir(self.root())
            .env("PATH", self.path())
            .env("NAMI_TEST_XTASK", env!("CARGO_BIN_EXE_xtask"))
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

    /// The `PATH` with the fake `just` and `cargo` first.
    fn path(&self) -> String {
        let path = std::env::var("PATH").expect("PATH is set");
        format!("{}:{path}", self.directory.join("bin").display())
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
/// `<directory>|<toolchain>|<toolchain source>|<arguments>`.
fn cargo_calls(checkout: &Path) -> Vec<String> {
    let calls = fs::read_to_string(checkout.join("target/cargo-calls")).unwrap_or_default();
    calls.lines().map(str::to_owned).collect()
}

/// The lines a file in `checkout` holds, or none if it does not exist.
fn lines_of(checkout: &Path, file: &str) -> Vec<String> {
    let text = fs::read_to_string(checkout.join(file)).unwrap_or_default();
    text.lines().map(str::to_owned).collect()
}

/// A second worktree of `scratch` on the branch `work`, with new work in `src/` and a planted
/// `violation.txt`, and the Stop payload of a session whose `cwd` is in it.
fn worktree_with_violation(scratch: &Scratch) -> (PathBuf, String) {
    let worktree = scratch.worktree("work");
    fs::create_dir_all(worktree.join("src")).expect("the worktree is writable");
    fs::write(worktree.join("src/new.rs"), "fn new() {}\n").expect("the worktree is writable");
    fs::write(worktree.join("violation.txt"), "x\n").expect("the worktree is writable");
    let payload = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, worktree.join("src").display());
    (worktree, payload)
}

/// The `PostToolUse` payload of an edit of `file`, made from the project directory.
fn edit_payload(project: &Path, file: &Path) -> String {
    format!(
        r#"{{"cwd":"{}","tool_input":{{"file_path":"{}"}}}}"#,
        project.display(),
        file.display()
    )
}

// Goal: a subagent in its own worktree is gated in that worktree, with the toolchain it pins and
// state of its own, by the project's hook alone: the hook command runs in the project
// directory, and the worktree's code runs only as the gate. Method: Stop payloads whose `cwd`
// is in a second worktree, with a fake `just` that records where and how it ran.
#[test]
fn stop_gates_the_worktree_in_the_payload() {
    let scratch = Scratch::new("stop-worktree");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let (worktree, payload) = worktree_with_violation(&scratch);
    let toolchain = [
        ("RUSTUP_TOOLCHAIN", Path::new("the-project-toolchain")),
        ("RUSTUP_TOOLCHAIN_SOURCE", Path::new("env")),
    ];
    for attempt in 1..=3 {
        let blocked = scratch.hook("stop", &payload, &toolchain);
        let stderr = String::from_utf8_lossy(&blocked.stderr);
        assert_eq!(blocked.status.code(), Some(2), "the worktree's violation blocks: {stderr}");
        assert!(stderr.contains(&format!("blocked stop {attempt} of 3")), "{stderr}");
        assert!(stderr.contains("violation.txt must not exist"), "{stderr}");
        assert!(gate_state(&worktree).join("attempts-s1").exists(), "the count is the worktree's");
    }
    let gave_up = scratch.hook("stop", &payload, &toolchain);
    let stderr = String::from_utf8_lossy(&gave_up.stderr);
    assert_eq!(gave_up.status.code(), Some(1), "the fourth stop gives up: {stderr}");
    assert!(stderr.starts_with("STOP GATE STILL FAILING"), "{stderr}");
    assert!(!gate_state(&worktree).join("attempts-s1").exists(), "giving up clears the count");
    assert_eq!(lines_of(&worktree, "target/just-calls"), vec!["check dylint"; 4], "every stop");
    assert_eq!(lines_of(&worktree, "target/just-environment"), vec!["|"; 4], "its own toolchain");
    assert_eq!(cargo_calls(&worktree), Vec::<String>::new(), "no other code of the worktree ran");
    assert!(!project.join("target/just-calls").exists(), "the project was not gated");

    fs::remove_file(worktree.join("violation.txt")).unwrap();
    assert_eq!(scratch.hook("stop", &payload, &[]).status.code(), Some(0), "fixed");
    let passed = fs::read_to_string(gate_state(&worktree).join("passed")).unwrap();
    assert_eq!(Some(passed.clone()), tree_fingerprint(&worktree), "the worktree's pass");
    assert!(!gate_state(&project).join("passed").exists(), "the project has passed nothing");

    let in_project = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, project.display());
    scratch.write("violation.txt", "x\n");
    assert_eq!(scratch.hook("stop", &in_project, &[]).status.code(), Some(2), "the project");
    assert_eq!(scratch.gate_runs(), 1, "is gated in itself");
    let kept = fs::read_to_string(gate_state(&worktree).join("passed")).unwrap();
    assert_eq!(kept, passed, "and the worktree's pass is untouched");
}

// Goal: a gate that cannot start is a failed gate: it blocks within the limit and then gives up
// loudly, and is never a pass. Method: the fake `just` loses its execute permission.
#[test]
fn a_gate_that_cannot_start_is_a_failed_gate() {
    let scratch = Scratch::new("stop-no-just");
    let (worktree, payload) = worktree_with_violation(&scratch);
    fs::remove_file(worktree.join("violation.txt")).unwrap();
    let just = scratch.directory.join("bin/just");
    fs::set_permissions(&just, fs::Permissions::from_mode(0o644)).expect("it is ours");
    // No other `just` may be found further down `PATH`.
    let system_path = std::env::var_os("PATH").expect("PATH is set");
    let others = std::env::split_paths(&system_path).filter(|entry| !entry.join("just").exists());
    let entries = std::iter::once(scratch.directory.join("bin")).chain(others);
    let path = PathBuf::from(std::env::join_paths(entries).expect("the entries join"));
    let without_just = [("PATH", path.as_path())];
    for attempt in 1..=3 {
        let blocked = scratch.hook("stop", &payload, &without_just);
        let stderr = String::from_utf8_lossy(&blocked.stderr);
        assert_eq!(blocked.status.code(), Some(2), "a gate that cannot start blocks: {stderr}");
        assert!(stderr.contains("`just check dylint` cannot start: "), "{stderr}");
        assert!(stderr.contains(&format!("blocked stop {attempt} of 3")), "{stderr}");
    }
    let gave_up = scratch.hook("stop", &payload, &without_just);
    assert_eq!(gave_up.status.code(), Some(1), "then it gives up, loudly");
    assert!(String::from_utf8_lossy(&gave_up.stderr).starts_with("STOP GATE STILL FAILING"));
    assert!(!gate_state(&worktree).join("passed").exists(), "and nothing passed");
}

// Goal: an edit in another worktree of the project's repository is checked by that worktree's
// own `cargo xtask tidy --file`, with the toolchain it pins, whatever the `cwd` says; session
// start reports on the `cwd`'s worktree. Method: PostToolUse and SessionStart payloads; the fake
// `cargo` records the check and runs the xtask under test for it.
#[test]
fn edits_in_a_worktree_use_its_own_check() {
    let scratch = Scratch::new("edit-worktree");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let worktree = scratch.worktree("work");
    let draft = worktree.join("tests/snapshots/a.snap.new");
    fs::create_dir_all(draft.parent().expect("a file has a parent")).unwrap();
    fs::write(&draft, "draft\n").unwrap();
    let toolchain = [
        ("RUSTUP_TOOLCHAIN", Path::new("the-project-toolchain")),
        ("RUSTUP_TOOLCHAIN_SOURCE", Path::new("env")),
    ];
    let edited = scratch.hook("post-edit", &edit_payload(&project, &draft), &toolchain);
    let stderr = String::from_utf8_lossy(&edited.stderr);
    assert_eq!(edited.status.code(), Some(2), "the draft is a violation: {stderr}");
    assert!(stderr.contains("tests/snapshots/a.snap.new:0: error[snapshot-draft]"), "{stderr}");
    assert!(!stderr.contains("could not run"), "the worktree's check ran: {stderr}");
    let checked = format!("{}|||xtask tidy --file {}", worktree.display(), draft.display());
    assert_eq!(cargo_calls(&worktree), [checked], "by the worktree's xtask and toolchain");

    let notes = worktree.join("notes.md");
    fs::write(&notes, "notes\n").unwrap();
    let clean = scratch.hook("post-edit", &edit_payload(&project, &notes), &[]);
    let clean_stderr = String::from_utf8_lossy(&clean.stderr);
    assert_eq!((clean.status.code(), clean_stderr.as_ref()), (Some(0), ""), "a clean file passes");
    assert_eq!(cargo_calls(&worktree).len(), 2, "after the worktree checked it");

    let start = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, worktree.display());
    let started = scratch.hook("session-start", &start, &[]);
    assert_eq!(started.status.code(), Some(0), "findings do not fail the hook");
    let stdout = String::from_utf8_lossy(&started.stdout);
    assert!(stdout.contains("error[doctor]: "), "the worktree's findings are shown: {stdout}");
    assert_eq!(cargo_calls(&project), Vec::<String>::new(), "the project never ran cargo");
}

// Goal: the worktree's own verdict decides for its own files, both ways (#27): a violation that
// only the worktree's rules see blocks the edit, and a file that only the project's rules would
// flag passes. Method: the fake `cargo` stands for a worktree whose rules disagree with the
// project's, on a clean file and on an unreviewed snapshot that the project's rules flag.
#[test]
fn the_worktree_verdict_wins_for_its_own_files() {
    let scratch = Scratch::new("edit-verdict");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let worktree = scratch.worktree("work");
    let notes = worktree.join("notes.md");
    fs::write(&notes, "notes\n").unwrap();
    let draft = worktree.join("a.snap.new");
    fs::write(&draft, "draft\n").unwrap();

    fs::write(worktree.join("worktree-flags-everything"), "").unwrap();
    let flagged = scratch.hook("post-edit", &edit_payload(&project, &notes), &[]);
    let flagged_stderr = String::from_utf8_lossy(&flagged.stderr);
    assert_eq!(flagged.status.code(), Some(2), "the worktree's rule blocks: {flagged_stderr}");
    let expected = format!("{}:0: error[worktree-only]", notes.display());
    assert!(flagged_stderr.contains(&expected), "its message is shown: {flagged_stderr}");
    assert!(!flagged_stderr.contains("could not run"), "the check ran: {flagged_stderr}");
    fs::remove_file(worktree.join("worktree-flags-everything")).unwrap();

    fs::write(worktree.join("worktree-passes-everything"), "").unwrap();
    let passed = scratch.hook("post-edit", &edit_payload(&project, &draft), &[]);
    let passed_stderr = String::from_utf8_lossy(&passed.stderr);
    let outcome = (passed.status.code(), passed_stderr.as_ref());
    assert_eq!(outcome, (Some(0), ""), "the worktree's rules pass it, so it passes");
    fs::remove_file(worktree.join("worktree-passes-everything")).unwrap();
    assert_eq!(cargo_calls(&worktree).len(), 2, "both verdicts came from the worktree");
}

// Goal: a directory inside the project that merely looks like a checkout, untracked with its
// own `rust-toolchain.toml` and `xtask/`, is not run as a worktree: git names the project as
// its toplevel. Its files are checked by the project's rules, and none of its code runs.
#[test]
fn a_nami_looking_directory_inside_the_project_is_not_a_worktree() {
    let scratch = Scratch::new("inner");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    scratch.write("vendor/inner/rust-toolchain.toml", "[toolchain]\n");
    scratch.write("vendor/inner/xtask/README", "tooling\n");
    scratch.write("vendor/inner/a.snap.new", "draft\n");
    let inner = project.join("vendor/inner");
    let edited = scratch.hook("post-edit", &edit_payload(&project, &inner.join("a.snap.new")), &[]);
    let stderr = String::from_utf8_lossy(&edited.stderr);
    assert_eq!(edited.status.code(), Some(2), "the file is checked: {stderr}");
    assert!(stderr.contains("a.snap.new:0: error[snapshot-draft]"), "by the project: {stderr}");
    assert!(!stderr.contains("could not run"), "no worktree check was tried: {stderr}");
    assert_eq!(cargo_calls(&inner), Vec::<String>::new(), "none of its code ran");
}

// Goal: an edited Rust file in a worktree is formatted by the rustfmt of the toolchain that
// worktree pins, with this process's toolchain override removed; if that rustfmt cannot run,
// this process's own formats the file and says so. Method: a fake `rustfmt` that puts a line
// in front naming the toolchain it ran with, and fails while `rustfmt-fails` exists unless an
// override names another toolchain.
#[test]
fn rust_files_in_a_worktree_are_formatted_by_its_toolchain() {
    let scratch = Scratch::new("edit-rustfmt");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let worktree = scratch.worktree("work");
    let source = worktree.join("src/lib.rs");
    fs::create_dir_all(worktree.join("src")).unwrap();
    fs::write(&source, "fn a() {}\n").unwrap();
    let toolchain = [
        ("RUSTUP_TOOLCHAIN", Path::new("the-project-toolchain")),
        ("RUSTUP_TOOLCHAIN_SOURCE", Path::new("env")),
    ];
    let edited = scratch.hook("post-edit", &edit_payload(&project, &source), &toolchain);
    let stderr = String::from_utf8_lossy(&edited.stderr);
    assert_eq!(edited.status.code(), Some(0), "the formatted file is clean: {stderr}");
    let formatted = fs::read_to_string(&source).unwrap();
    assert_eq!(formatted, "// formatted by the pinned toolchain\nfn a() {}\n", "the worktree's");
    let pinned = format!("{}|||--edition 2024 --emit stdout", worktree.display());
    let calls = lines_of(&worktree, "target/rustfmt-calls");
    assert_eq!(calls, std::slice::from_ref(&pinned), "with the overrides removed");

    fs::write(worktree.join("rustfmt-fails"), "").unwrap();
    fs::write(&source, "fn b() {}\n").unwrap();
    let fell_back = scratch.hook("post-edit", &edit_payload(&project, &source), &toolchain);
    let fell_back_stderr = String::from_utf8_lossy(&fell_back.stderr);
    let code = fell_back.status.code();
    assert_eq!(code, Some(0), "the fallback formats it: {fell_back_stderr}");
    let note = format!("note: the rustfmt that {} pins did not run (exit 1: ", worktree.display());
    assert!(fell_back_stderr.contains(&note), "and says so: {fell_back_stderr}");
    let reformatted = fs::read_to_string(&source).unwrap();
    let expected = "// formatted by the-project-toolchain\nfn b() {}\n";
    assert_eq!(reformatted, expected, "by this process's rustfmt");
    let tail = "the-project-toolchain|env|--edition 2024 --emit stdout";
    let fallback = format!("{}|{tail}", worktree.display());
    let all_calls = lines_of(&worktree, "target/rustfmt-calls");
    assert_eq!(all_calls, [pinned.clone(), pinned, fallback], "the worktree's first, then its own");
}

// Goal: a worktree check that cannot run, because the toolchain it pins is missing, its xtask
// does not build or is too old to know `--file`, falls back to the project's rules and says so:
// the file is checked either way, and only a real violation blocks.
#[test]
fn a_worktree_check_that_cannot_run_falls_back_to_the_project() {
    let scratch = Scratch::new("edit-fallback");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let worktree = scratch.worktree("work");
    let draft = worktree.join("a.snap.new");
    fs::write(&draft, "draft\n").unwrap();
    let notes = worktree.join("notes.md");
    fs::write(&notes, "notes\n").unwrap();
    let cases = [
        ("missing-toolchain", "exited 1: error: toolchain 'pinned' is not installed"),
        ("broken-xtask", "exited 101: error: could not compile xtask"),
        ("old-xtask", "exited 1: usage: cargo xtask tidy [--root <dir>"),
    ];
    for (marker, reason) in cases {
        fs::write(worktree.join(marker), "").unwrap();
        let edited = scratch.hook("post-edit", &edit_payload(&project, &draft), &[]);
        let stderr = String::from_utf8_lossy(&edited.stderr);
        assert_eq!(edited.status.code(), Some(2), "{marker}: the draft still blocks: {stderr}");
        assert!(stderr.contains(reason), "{marker}: why it could not run: {stderr}");
        assert!(stderr.contains("checked with the rules of the project instead"), "{stderr}");
        assert!(stderr.contains("a.snap.new:0: error[snapshot-draft]"), "{marker}: {stderr}");
        let clean = scratch.hook("post-edit", &edit_payload(&project, &notes), &[]);
        let clean_stderr = String::from_utf8_lossy(&clean.stderr);
        assert_eq!(clean.status.code(), Some(0), "{marker}: a clean file passes: {clean_stderr}");
        assert!(clean_stderr.contains("could not run"), "{marker}: and says how it was checked");
        fs::remove_file(worktree.join(marker)).unwrap();
    }
    assert_eq!(cargo_calls(&worktree).len(), 6, "the worktree's check was tried every time");
}

// Goal: a worktree of the project's own repository with a detached `HEAD`, as `review-pr`
// checks out a fork's pull request, runs none of its own code after an edit: it shares the
// repository, but its `HEAD` is not a local branch, so the project's rules check its files.
#[test]
fn a_detached_worktree_gets_the_project_rules() {
    let scratch = Scratch::new("edit-detached");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let detached = scratch.detached_worktree("review");
    let draft = detached.join("a.snap.new");
    fs::write(&draft, "draft\n").unwrap();
    let source = detached.join("src/lib.rs");
    fs::create_dir_all(detached.join("src")).unwrap();
    fs::write(&source, "fn a() {}\n").unwrap();
    let toolchain = [("RUSTUP_TOOLCHAIN", Path::new("the-project-toolchain"))];
    let edited = scratch.hook("post-edit", &edit_payload(&project, &draft), &toolchain);
    let stderr = String::from_utf8_lossy(&edited.stderr);
    assert_eq!(edited.status.code(), Some(2), "the draft is checked: {stderr}");
    assert!(stderr.contains("a.snap.new:0: error[snapshot-draft]"), "by the project: {stderr}");
    assert!(!stderr.contains("could not run"), "no worktree check was tried: {stderr}");
    let formatted = scratch.hook("post-edit", &edit_payload(&project, &source), &toolchain);
    assert_eq!(formatted.status.code(), Some(0), "the Rust file is formatted and clean");
    let text = fs::read_to_string(&source).unwrap();
    assert_eq!(text, "// formatted by the-project-toolchain\nfn a() {}\n", "by the project");
    assert_eq!(cargo_calls(&detached), Vec::<String>::new(), "none of its code ran");
}

// Goal: an edit in the project checkout itself runs no worktree check: this process's rules
// and toolchain are the project's own. Method: a Rust file and an unreviewed snapshot edited in
// the project, with the fake `rustfmt` and `cargo` recording any call.
#[test]
fn an_edit_in_the_project_runs_no_worktree_check() {
    let scratch = Scratch::new("edit-project");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    scratch.write("a.snap.new", "draft\n");
    scratch.write("src/lib.rs", "fn a() {}\n");
    let toolchain = [
        ("RUSTUP_TOOLCHAIN", Path::new("the-project-toolchain")),
        ("RUSTUP_TOOLCHAIN_SOURCE", Path::new("env")),
    ];
    let draft = project.join("a.snap.new");
    let edited = scratch.hook("post-edit", &edit_payload(&project, &draft), &toolchain);
    let stderr = String::from_utf8_lossy(&edited.stderr);
    assert_eq!(edited.status.code(), Some(2), "the draft is checked: {stderr}");
    assert!(stderr.contains("a.snap.new:0: error[snapshot-draft]"), "{stderr}");
    let source = project.join("src/lib.rs");
    let formatted = scratch.hook("post-edit", &edit_payload(&project, &source), &toolchain);
    assert_eq!(formatted.status.code(), Some(0), "the Rust file is formatted and clean");
    let calls = lines_of(&project, "target/rustfmt-calls");
    let own =
        format!("{}|the-project-toolchain|env|--edition 2024 --emit stdout", project.display());
    assert_eq!(calls, [own], "by this process's rustfmt, with its toolchain kept");
    assert_eq!(cargo_calls(&project), Vec::<String>::new(), "and no `cargo xtask tidy --file`");
}

// Goal: only a worktree of the project's own repository whose `HEAD` is a local branch runs
// its own checks after an edit. Another clone is checked with the project's rules, as is a
// worktree whose repository git cannot name; neither is ever skipped. (A fork's pull request
// checked out for review shares this repository and is told apart by its detached `HEAD`: see
// `a_detached_worktree_gets_the_project_rules`.) Method: a clone, and a fake `git` that always
// fails, first on `PATH`.
#[test]
fn a_foreign_or_unnamed_checkout_gets_the_project_rules() {
    let scratch = Scratch::new("foreign");
    let project = fs::canonicalize(scratch.root()).expect("the checkout exists");
    let clone_path = scratch.directory.join("clone");
    scratch.git(&["clone", "--quiet", ".", clone_path.to_str().expect("the path is UTF-8")]);
    let clone = fs::canonicalize(&clone_path).expect("the clone exists");
    let draft = clone.join("a.snap.new");
    fs::write(&draft, "draft\n").unwrap();
    let edited = scratch.hook("post-edit", &edit_payload(&project, &draft), &[]);
    let stderr = String::from_utf8_lossy(&edited.stderr);
    assert_eq!(edited.status.code(), Some(2), "the clone's file is checked: {stderr}");
    assert!(stderr.contains("a.snap.new:0: error[snapshot-draft]"), "{stderr}");
    assert!(!stderr.contains("could not run"), "by the project, as intended: {stderr}");
    assert_eq!(cargo_calls(&clone), Vec::<String>::new(), "the clone's xtask never ran");
    git(&clone, &["switch", "--quiet", "--create", "work"]);
    fs::write(clone.join("violation.txt"), "x\n").unwrap();
    let in_clone = format!(r#"{{"session_id":"s1","cwd":"{}"}}"#, clone.display());
    assert_eq!(scratch.hook("stop", &in_clone, &[]).status.code(), Some(2), "and gated");

    let (worktree, payload) = worktree_with_violation(&scratch);
    let no_git = scratch.directory.join("no-git");
    fs::create_dir_all(&no_git).unwrap();
    fs::write(no_git.join("git"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(no_git.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = PathBuf::from(format!("{}:{}", no_git.display(), scratch.path()));
    let without_git = [("PATH", path.as_path())];
    let worktree_draft = worktree.join("a.snap.new");
    fs::write(&worktree_draft, "draft\n").unwrap();
    let edit = edit_payload(&project, &worktree_draft);
    let unnamed = scratch.hook("post-edit", &edit, &without_git);
    let unnamed_stderr = String::from_utf8_lossy(&unnamed.stderr);
    assert_eq!(unnamed.status.code(), Some(2), "without git, still checked: {unnamed_stderr}");
    assert!(unnamed_stderr.contains("a.snap.new:0: error[snapshot-draft]"), "{unnamed_stderr}");
    assert_eq!(cargo_calls(&worktree), Vec::<String>::new(), "as a foreign tree");
    let stopped = scratch.hook("stop", &payload, &without_git);
    let stop_stderr = String::from_utf8_lossy(&stopped.stderr);
    assert_eq!(stopped.status.code(), Some(2), "without git, the stop is gated: {stop_stderr}");
    assert!(stop_stderr.contains("violation.txt must not exist"), "{stop_stderr}");
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
