//! Claude Code hooks (ADR-0019, `.claude/settings.json`).
//!
//! Hooks are defence in depth: they give agents fast feedback, while the server-side gates stay
//! authoritative (ADR-0009). Each hook reads the event JSON on stdin. Exit code 2 sends stderr
//! back to the agent; any other failure is reported to the human as a hook error.
//!
//! - `post-edit`: formats an edited Rust file, and runs on the edited file every tidy rule that
//!   needs only that file: Rust sources, manifests, ADRs and the style guide.
//! - `stop`: runs `just check dylint` before an agent or subagent may finish, unless the working
//!   state is the unchanged base or has already passed.
//! - `session-start`: puts `$HOME/.cargo/bin` on the session's `PATH` and reports tools whose
//!   version is not the pinned one.
//!
//! # Which checkout, and which xtask
//!
//! A subagent may work in its own git worktree, while Claude Code starts every hook command in
//! the session's project directory. So the hooks take the checkout from the payload: the git
//! toplevel of the edited file, or of the `cwd` at stop and session start. If that checkout is
//! not the one this binary was built from, the hook runs `cargo xtask hook` there instead, so
//! that new code is checked by the rules on its own branch. [`ROOT_VARIABLE`] passes the
//! checkout on and stops the hand-over from repeating. If that xtask cannot run, this one does
//! the work, and an edit gets feedback that says so.
//!
//! # The stop gate
//!
//! The working state is named by a fingerprint: the git tree of everything that is tracked, or
//! untracked and not ignored. Committing does not change it, so a commit cannot hide work from
//! the gate. The gate is skipped only for a state that is known to be good: the merge base with
//! `main`, or the state of the last passing run. A failing gate blocks the stop
//! [`STOP_BLOCKS_MAX`] times in a row per session; after that it lets the agent go and tells the
//! human. The state is kept in `target/stop-gate/` of the checkout, which git ignores, so each
//! worktree has its own.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// Exit code that makes Claude Code show stderr to the agent.
const EXIT_FEEDBACK: u8 = 2;

/// Upper bound on directories walked up when looking for the workspace root.
const ANCESTORS_MAX: u32 = 64;

/// Upper bound on characters read from one JSON string value.
const JSON_STRING_CHARACTERS_MAX: u32 = 65_536;

/// Lines of failing gate output passed back to the agent.
const FEEDBACK_LINES_MAX: usize = 60;

/// Failed stops in a row that the gate blocks per session. The next one gives up, so that a
/// check the agent cannot fix does not trap it. CI still runs every check.
pub const STOP_BLOCKS_MAX: u32 = 3;

/// The `just` recipes of the stop gate: the fast gate and the recursion lint (ADR-0006).
const GATE_RECIPES: [&str; 2] = ["check", "dylint"];

/// Where the stop gate keeps its state, below the checkout root.
const GATE_STATE_DIRECTORY: &str = "target/stop-gate";

/// The file in the state directory that holds the fingerprint of the last passing run.
const GATE_PASSED_FILE: &str = "passed";

/// Branches that mark where the work started, in order of preference.
const BASE_BRANCHES: [&str; 2] = ["main", "origin/main"];

/// Upper bound on the characters of a session id that go into a file name.
const SESSION_ID_CHARACTERS_MAX: usize = 128;

/// The line that puts cargo's tools on `PATH`. The shell expands it when it reads the file.
const CARGO_BIN_EXPORT: &str = r#"export PATH="$HOME/.cargo/bin:$PATH""#;

/// The environment variable that names the checkout a delegated hook works on. The hook sets it
/// when it hands an event to that checkout's own xtask, which then takes the checkout from it
/// and never hands the event on again.
pub const ROOT_VARIABLE: &str = "NAMI_HOOK_ROOT";

/// The variable Claude Code sets to the directory where the session started.
const PROJECT_VARIABLE: &str = "CLAUDE_PROJECT_DIR";

/// Variables that rustup sets for the process it runs. Passed on, they would make the delegated
/// cargo use this checkout's toolchain instead of the one the other checkout pins.
const TOOLCHAIN_VARIABLES: [&str; 2] = ["RUSTUP_TOOLCHAIN", "RUSTUP_TOOLCHAIN_SOURCE"];

/// A Claude Code hook event that `cargo xtask hook` handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `PostToolUse` for `Edit`, `Write` and `MultiEdit`.
    PostEdit,
    /// `Stop` and `SubagentStop`.
    Stop,
    /// `SessionStart`.
    SessionStart,
}

impl Event {
    /// The event named `name` on the command line, if it is one.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let event = match name {
            "post-edit" => Self::PostEdit,
            "stop" => Self::Stop,
            "session-start" => Self::SessionStart,
            _ => return None,
        };
        assert_eq!(event.name(), name, "the name round-trips");
        Some(event)
    }

    /// The name of the event on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::PostEdit => "post-edit",
            Self::Stop => "stop",
            Self::SessionStart => "session-start",
        }
    }
}

/// Runs the hook named `event`, reading its JSON payload from stdin.
///
/// The hook works on the checkout the payload is about (see [`hook_root`]), and it runs that
/// checkout's own xtask: if this process was built from another checkout, it hands the event to
/// `cargo xtask hook` in that checkout, so that a worktree is checked by its own rules.
pub fn run(event: &str) -> ExitCode {
    let Some(event) = Event::parse(event) else {
        eprintln!("hook: unknown event `{event}`; expected post-edit, stop or session-start");
        return ExitCode::FAILURE;
    };
    let mut payload = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut payload) {
        eprintln!("hook {}: cannot read stdin: {error}", event.name());
        return ExitCode::FAILURE;
    }
    let delegated = std::env::var_os(ROOT_VARIABLE).filter(|root| !root.is_empty());
    let mut is_fallback = false;
    let root = if let Some(root) = delegated {
        Some(PathBuf::from(root))
    } else {
        let project = std::env::var_os(PROJECT_VARIABLE).filter(|project| !project.is_empty());
        let root = hook_root(event, &payload, project.as_deref().map(Path::new));
        if let Some(root) = &root
            && !is_current_checkout(root)
        {
            match delegate(event, root, &payload) {
                Ok(exit) => return exit,
                Err(error) => {
                    eprintln!("hook {}: {error}; checking with this xtask instead", event.name());
                    is_fallback = true;
                }
            }
        }
        root
    };
    let exit = match event {
        Event::PostEdit => root.map_or(ExitCode::SUCCESS, |root| post_edit(&root, &payload)),
        Event::Stop => root.map_or(ExitCode::SUCCESS, |root| stop(&root, &payload)),
        Event::SessionStart => session_start(root.as_deref()),
    };
    // A checkout whose xtask does not build must hear about it after an edit; the stop gate
    // fails on it by itself, and at session start the human sees it.
    if is_fallback && event == Event::PostEdit { ExitCode::from(EXIT_FEEDBACK) } else { exit }
}

/// The checkout that a hook payload is about, found with `git rev-parse --show-toplevel`:
///
/// - after an edit, the one that holds the edited file;
/// - at stop and session start, the one around the payload's `cwd`, or, if the payload has no
///   `cwd`, around `project` (`CLAUDE_PROJECT_DIR`), or else around the current directory.
///
/// `None` if that is not a nami checkout, or if an edit names no file.
#[must_use]
pub fn hook_root(event: Event, payload: &str, project: Option<&Path>) -> Option<PathBuf> {
    let start = match event {
        Event::PostEdit => PathBuf::from(json_string_field(payload, "file_path")?),
        Event::Stop | Event::SessionStart => json_string_field(payload, "cwd")
            .map(PathBuf::from)
            .or_else(|| project.map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    let root = checkout_around(&start)?;
    assert!(root.is_absolute(), "git names the toplevel by an absolute path");
    Some(root)
}

/// True if this process runs in the checkout at `root`. The hook command starts in the checkout
/// that `cargo xtask` was built from, so this says whether this binary has `root`'s rules.
fn is_current_checkout(root: &Path) -> bool {
    let current = std::env::current_dir().ok().and_then(|directory| checkout_around(&directory));
    current.as_deref() == Some(root)
}

/// Hands the event to `cargo xtask hook` in the checkout at `root`, with the payload on stdin
/// and [`ROOT_VARIABLE`] set to `root`. Returns its exit code if it ran the hook (0, 1 or 2),
/// or what went wrong if it did not, for example because that xtask does not build.
fn delegate(event: Event, root: &Path, payload: &str) -> Result<ExitCode, String> {
    assert!(root.is_absolute(), "the checkout to delegate to is named by an absolute path");
    let command = format!("`cargo xtask hook {}` in {}", event.name(), root.display());
    let mut child = Command::new("cargo")
        .args(["xtask", "hook", event.name()])
        .current_dir(root)
        .env(ROOT_VARIABLE, root)
        .env_remove(TOOLCHAIN_VARIABLES[0])
        .env_remove(TOOLCHAIN_VARIABLES[1])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run {command}: {error}"))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // A child that exits without reading its payload is judged by its exit code below.
    let _written = stdin.write_all(payload.as_bytes());
    drop(stdin);
    let status = child.wait().map_err(|error| format!("cannot wait for {command}: {error}"))?;
    match status.code().map(u8::try_from) {
        Some(Ok(code @ 0..=EXIT_FEEDBACK)) => Ok(ExitCode::from(code)),
        _ => Err(format!("{command} did not run the hook ({status})")),
    }
}

fn post_edit(root: &Path, payload: &str) -> ExitCode {
    let Some(file) = json_string_field(payload, "file_path").map(PathBuf::from) else {
        return ExitCode::SUCCESS;
    };
    // A deleted file, a directory or a binary file has nothing to check.
    let Ok(original) = std::fs::read_to_string(&file) else {
        return ExitCode::SUCCESS;
    };
    // git names the root without symbolic links, so the file is compared in the same form.
    let (Ok(root), Ok(file)) = (fs::canonicalize(root), fs::canonicalize(&file)) else {
        return ExitCode::SUCCESS;
    };
    let Ok(relative) = file.strip_prefix(&root) else {
        eprintln!("hook post-edit: {} is not in {}", file.display(), root.display());
        return ExitCode::FAILURE;
    };
    let is_rust = file.extension().is_some_and(|extension| extension == "rs");
    let text = if is_rust {
        match format_in_place(&root, &file, &original) {
            Ok(formatted) => formatted,
            Err(exit) => return exit,
        }
    } else {
        original
    };
    let violations = match crate::tidy::check_single_file(&root, relative, &text) {
        Ok(violations) => violations,
        Err(error) => {
            eprintln!("hook post-edit: cannot check {}: {error}", file.display());
            return ExitCode::FAILURE;
        }
    };
    if violations.is_empty() {
        return ExitCode::SUCCESS;
    }
    for violation in &violations {
        eprintln!("{violation}");
    }
    ExitCode::from(EXIT_FEEDBACK)
}

/// Formats a Rust file and writes it back if the text changed. Returns the formatted text, or
/// the exit code to stop with: feedback for a syntax error, failure if the file cannot be
/// written.
fn format_in_place(root: &Path, file: &Path, original: &str) -> Result<String, ExitCode> {
    let text = match format_source(root, original) {
        Ok(formatted) => formatted,
        Err(error) => {
            eprintln!("rustfmt cannot format {}; fix the syntax error first:", file.display());
            eprintln!("{error}");
            return Err(ExitCode::from(EXIT_FEEDBACK));
        }
    };
    if text != original && std::fs::write(file, &text).is_err() {
        eprintln!("cannot write the formatted {}", file.display());
        return Err(ExitCode::FAILURE);
    }
    Ok(text)
}

/// The stop gate for the checkout at `root`. Its state lives in that checkout, so each worktree
/// has its own last pass and its own counters.
fn stop(root: &Path, payload: &str) -> ExitCode {
    let state = gate_state(root);
    let attempts = json_string_field(payload, "session_id")
        .map(|session_id| sanitise_session_id(&session_id))
        .filter(|session_id| !session_id.is_empty())
        .map(|session_id| state.join(format!("attempts-{session_id}")));
    let fingerprint = tree_fingerprint(root);
    let passed = fs::read_to_string(state.join(GATE_PASSED_FILE)).ok();
    let passed = passed.as_deref().map(str::trim);
    if gate_decision(fingerprint.as_deref(), base_tree(root).as_deref(), passed) == Gate::Skip {
        discard(attempts.as_deref());
        return ExitCode::SUCCESS;
    }
    let Ok(output) = Command::new("just").args(GATE_RECIPES).current_dir(root).output() else {
        eprintln!("hook stop: cannot run `just check dylint`; is `just` installed?");
        return ExitCode::FAILURE;
    };
    if output.status.success() {
        if let Some(fingerprint) = fingerprint
            && let Err(error) = fs::write(state.join(GATE_PASSED_FILE), fingerprint)
        {
            eprintln!("hook stop: cannot record the passing state: {error}");
        }
        discard(attempts.as_deref());
        return ExitCode::SUCCESS;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stop_hook_active = json_bool_field(payload, "stop_hook_active") == Some(true);
    stop_failed(attempts.as_deref(), stop_hook_active, &tail(&text, FEEDBACK_LINES_MAX))
}

/// Reports a failed gate: blocks the stop, or gives up once the session has been blocked
/// [`STOP_BLOCKS_MAX`] times in a row. `attempts` is the session's counter file, if the payload
/// names a session.
fn stop_failed(attempts: Option<&Path>, stop_hook_active: bool, output_tail: &str) -> ExitCode {
    let mut decision = failure_decision(attempts.map(failures_stored), stop_hook_active);
    if let (Failure::Block { attempt }, Some(file)) = (decision, attempts)
        && let Err(error) = fs::write(file, attempt.to_string())
    {
        // A counter that cannot be stored would block for ever; Claude Code's flag still ends it.
        eprintln!("hook stop: cannot write {}: {error}", file.display());
        decision = failure_decision(None, stop_hook_active);
    }
    match decision {
        Failure::Block { attempt } => {
            eprintln!(
                "`just check dylint` fails, so the work is not done yet. This is blocked stop \
                 {attempt} of {STOP_BLOCKS_MAX}. Fix these and try again:"
            );
            eprintln!("{output_tail}");
            ExitCode::from(EXIT_FEEDBACK)
        }
        Failure::GiveUp => {
            discard(attempts);
            eprintln!("STOP GATE STILL FAILING: `just check dylint` does not pass.");
            eprintln!("The work is NOT done, and CI will reject it.");
            eprintln!("This hook has stopped blocking only so that the agent is not trapped.");
            eprintln!("The last output of the gate:");
            eprintln!("{output_tail}");
            ExitCode::FAILURE
        }
    }
}

fn session_start(root: Option<&Path>) -> ExitCode {
    let status = match export_cargo_bin() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hook session-start: cannot extend the file CLAUDE_ENV_FILE names: {error}");
            ExitCode::FAILURE
        }
    };
    if let Some(root) = root {
        // Claude Code adds stdout to the agent's context. A finding informs; it never fails the
        // hook.
        for finding in crate::doctor::findings(root) {
            println!("{finding}");
        }
    }
    status
}

/// Appends [`CARGO_BIN_EXPORT`] to the file that `CLAUDE_ENV_FILE` names, unless the session's
/// `PATH` already lists the directory. Claude Code applies that file to the session's later
/// shell commands, where `just` and `cargo` are otherwise not found.
///
/// The session's `PATH` comes from `NAMI_SESSION_PATH`, which the hook command sets. This
/// process's own `PATH` cannot tell: the hook command and the rustup proxy both extend it.
fn export_cargo_bin() -> io::Result<()> {
    let env_file = std::env::var_os("CLAUDE_ENV_FILE").filter(|file| !file.is_empty());
    let (Some(env_file), Ok(home)) = (env_file, std::env::var("HOME")) else {
        return Ok(());
    };
    let session_path = std::env::var("NAMI_SESSION_PATH").ok();
    if path_lists_cargo_bin(session_path.as_deref(), &home) {
        return Ok(());
    }
    let existing = fs::read_to_string(&env_file).unwrap_or_default();
    let Some(addition) = env_file_addition(&existing) else {
        return Ok(());
    };
    fs::OpenOptions::new().create(true).append(true).open(&env_file)?.write_all(addition.as_bytes())
}

/// True if `session_path`, a `PATH` value, lists `<home>/.cargo/bin`. An unknown `PATH` does
/// not: a second entry is harmless, a missing one makes `just check` fail.
fn path_lists_cargo_bin(session_path: Option<&str>, home: &str) -> bool {
    let cargo_bin = Path::new(home).join(".cargo/bin");
    session_path.is_some_and(|path| std::env::split_paths(path).any(|entry| entry == cargo_bin))
}

/// The text to append to an env file that holds `existing`, or `None` if the export is there.
fn env_file_addition(existing: &str) -> Option<String> {
    if existing.lines().any(|line| line.trim() == CARGO_BIN_EXPORT) {
        return None;
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    Some(format!("{separator}{CARGO_BIN_EXPORT}\n"))
}

/// What the stop hook does with the gate for one working state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// The state is known to be good: let the agent stop without running anything.
    Skip,
    /// The state is new or unknown: run the gate.
    Run,
}

/// Decides whether the gate runs for the working state `fingerprint`.
///
/// `base` is the tree of the merge base with `main`, and `passed` the fingerprint of the last
/// passing run. The gate is skipped only if the state equals one of them. A state that git
/// cannot name runs the gate.
#[must_use]
pub fn gate_decision(fingerprint: Option<&str>, base: Option<&str>, passed: Option<&str>) -> Gate {
    let Some(fingerprint) = fingerprint else {
        return Gate::Run;
    };
    if base == Some(fingerprint) || passed == Some(fingerprint) { Gate::Skip } else { Gate::Run }
}

/// What the stop hook does when the gate fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    /// Block the stop and send the output to the agent. `attempt` counts from 1.
    Block { attempt: u32 },
    /// Let the agent stop, and tell the human that the work is not done.
    GiveUp,
}

/// Decides between blocking and giving up after a failed gate.
///
/// `failures_before` counts this session's earlier failed stops in a row. It is `None` if there
/// is no counter, because the payload has no session id. Then `stop_hook_active`, which says that
/// an earlier stop hook already blocked, is the only memory: block once, then give up.
fn failure_decision(failures_before: Option<u32>, stop_hook_active: bool) -> Failure {
    match failures_before {
        Some(failures) if failures < STOP_BLOCKS_MAX => Failure::Block { attempt: failures + 1 },
        None if !stop_hook_active => Failure::Block { attempt: STOP_BLOCKS_MAX },
        Some(_) | None => Failure::GiveUp,
    }
}

/// The number in a session's counter file. A missing or unreadable file counts as no failures.
fn failures_stored(attempts: &Path) -> u32 {
    fs::read_to_string(attempts).ok().and_then(|count| count.trim().parse().ok()).unwrap_or(0)
}

/// Makes a session id safe as part of a file name: every character outside `[A-Za-z0-9_-]`
/// becomes `_`.
fn sanitise_session_id(session_id: &str) -> String {
    let sanitise = |character: char| {
        let is_safe = character.is_ascii_alphanumeric() || character == '_' || character == '-';
        if is_safe { character } else { '_' }
    };
    session_id.chars().take(SESSION_ID_CHARACTERS_MAX).map(sanitise).collect()
}

/// The last `lines_max` lines of `text`.
fn tail(text: &str, lines_max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let skipped = lines.len().saturating_sub(lines_max);
    lines.iter().skip(skipped).copied().collect::<Vec<&str>>().join("\n")
}

/// Removes a state file, if there is one.
fn discard(file: Option<&Path>) {
    if let Some(file) = file
        && let Err(error) = fs::remove_file(file)
        && error.kind() != io::ErrorKind::NotFound
    {
        eprintln!("hook stop: cannot remove {}: {error}", file.display());
    }
}

/// The fingerprint of the working state of the checkout at `root`: the id of the git tree that
/// holds every file that is tracked, or untracked and not ignored. `None` if git cannot build it.
///
/// The tree is built in a scratch index below `target/stop-gate/`, so the checkout's own index
/// and its files are untouched. Each process has its own scratch index, because concurrent stop
/// hooks in one checkout must not mix their states.
#[must_use]
pub fn tree_fingerprint(root: &Path) -> Option<String> {
    let state = std::path::absolute(gate_state(root)).ok()?;
    fs::create_dir_all(&state).ok()?;
    let index = state.join(format!("index-{}", std::process::id()));
    let lock = state.join(format!("index-{}.lock", std::process::id()));
    // A killed process with the same id may have left its lock behind.
    discard(Some(&lock));
    let steps: [&[&str]; 2] = [&["read-tree", "HEAD"], &["add", "--all"]];
    let is_built = steps.iter().all(|step| git(root, Some(&index), step).is_some());
    let tree = if is_built { git(root, Some(&index), &["write-tree"]) } else { None };
    discard(Some(&index));
    tree.filter(|tree| is_object_id(tree))
}

/// The tree of the commit where the work in the checkout at `root` started: the merge base of
/// `HEAD` with `main`, or with `origin/main`. `None` if neither gives one.
#[must_use]
pub fn base_tree(root: &Path) -> Option<String> {
    let branch = git(root, None, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    base_candidates(branch.as_deref()).find_map(|base| {
        let commit = git(root, None, &["merge-base", "HEAD", base])?;
        let tree = format!("{commit}^{{tree}}");
        git(root, None, &["rev-parse", "--verify", "--quiet", &tree])
            .filter(|tree| is_object_id(tree))
    })
}

/// The branches to take the merge base with, given the branch that is checked out. A branch is
/// never its own base: on `main`, every commit would count as the unchanged start.
fn base_candidates(branch: Option<&str>) -> impl Iterator<Item = &'static str> {
    BASE_BRANCHES.into_iter().filter(move |base| Some(*base) != branch)
}

/// True for a full git object id: 40 hexadecimal digits, or 64 in a SHA-256 repository.
fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.chars().all(|character| character.is_ascii_hexdigit())
}

/// Runs git in `root` and returns its trimmed output, or `None` if it cannot run or fails.
/// With `index`, git uses that index file instead of the checkout's own.
fn git(root: &Path, index: Option<&Path>, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .envs(index.map(|index| ("GIT_INDEX_FILE", index)))
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    output.status.success().then(|| text.trim().to_owned())
}

/// Formats one file's source through stdin, so that rustfmt does not follow `mod` declarations
/// into sibling files, which may not exist yet. Runs in `root` to pick up `rustfmt.toml`.
fn format_source(root: &Path, source: &str) -> Result<String, String> {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run rustfmt: {error}"))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(source.as_bytes()).map_err(|error| error.to_string())?;
    drop(stdin);
    let output = child.wait_with_output().map_err(|error| error.to_string())?;
    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|error| error.to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// The directory where the stop gate keeps its state for the checkout at `root`.
#[must_use]
pub fn gate_state(root: &Path) -> PathBuf {
    root.join(GATE_STATE_DIRECTORY)
}

/// The git toplevel of the checkout or worktree that holds `path`, if it is a nami checkout:
/// it holds `rust-toolchain.toml` and `xtask/`. `path` may be a file, or not exist yet; the
/// search starts at its nearest existing directory.
#[must_use]
pub fn checkout_around(path: &Path) -> Option<PathBuf> {
    let path = std::path::absolute(path).ok()?;
    let mut candidate = Some(path.as_path());
    let mut directory = None;
    for _ in 0..ANCESTORS_MAX {
        let existing = candidate?;
        if existing.is_dir() {
            directory = Some(existing);
            break;
        }
        candidate = existing.parent();
    }
    let toplevel = PathBuf::from(git(directory?, None, &["rev-parse", "--show-toplevel"])?);
    let is_nami = toplevel.join("rust-toolchain.toml").is_file() && toplevel.join("xtask").is_dir();
    is_nami.then_some(toplevel)
}

/// Returns the first string value stored under `key`, decoding JSON escapes.
///
/// Claude Code's payloads are flat enough that the first occurrence is the one we want
/// (`tool_input.file_path`, `cwd`). Returns `None` for anything unexpected.
#[must_use]
pub fn json_string_field(json: &str, key: &str) -> Option<String> {
    let value = json_value_after_key(json, key)?;
    let mut characters = value.strip_prefix('"')?.chars();
    let mut decoded = String::new();
    for _ in 0..JSON_STRING_CHARACTERS_MAX {
        match characters.next()? {
            '"' => return Some(decoded),
            '\\' => decoded.push(json_escape(&mut characters)?),
            character => decoded.push(character),
        }
    }
    None
}

/// Returns the boolean stored under `key`.
#[must_use]
pub fn json_bool_field(json: &str, key: &str) -> Option<bool> {
    let value = json_value_after_key(json, key)?;
    if value.starts_with("true") {
        Some(true)
    } else if value.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_value_after_key<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let quoted = format!("\"{key}\"");
    let start = json.find(&quoted)? + quoted.len();
    let rest = json.get(start..)?.trim_start().strip_prefix(':')?;
    Some(rest.trim_start())
}

fn json_escape(characters: &mut std::str::Chars<'_>) -> Option<char> {
    let decoded = match characters.next()? {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        'b' => '\u{8}',
        'f' => '\u{c}',
        'u' => {
            let hex: String = characters.by_ref().take(4).collect();
            char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?
        }
        other => other,
    };
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::{EXIT_FEEDBACK, Failure, Gate, STOP_BLOCKS_MAX, failure_decision, gate_decision};
    use super::{format_in_place, json_bool_field, json_string_field};
    use crate::tidy::scratch::Scratch;
    use std::process::ExitCode;

    /// `ExitCode` has no equality, so compare what it prints.
    fn is(code: ExitCode, expected: ExitCode) -> bool {
        format!("{code:?}") == format!("{expected:?}")
    }

    const UNFORMATTED: &str = "fn a( )->u32{1}\n";
    const FORMATTED: &str = "fn a() -> u32 {\n    1\n}\n";

    // Goal: an unformatted Rust file is rewritten in its formatted form, and the formatted text
    // is what the tidy rules then see.
    #[test]
    fn format_in_place_rewrites_an_unformatted_file() {
        let scratch = Scratch::new("hook-format-rewrite");
        scratch.write("lib.rs", UNFORMATTED);
        let file = scratch.root().join("lib.rs");
        let text = format_in_place(&crate::workspace_root(), &file, UNFORMATTED);
        assert_eq!(text.ok().as_deref(), Some(FORMATTED), "the formatted text is returned");
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert_eq!(on_disk, FORMATTED, "the file is rewritten");
    }

    // Goal: a file that is already formatted is not written at all, so the hook never touches a
    // file it has no reason to change. A path that cannot be written proves it: a write fails.
    #[test]
    fn format_in_place_leaves_a_formatted_file_alone() {
        let scratch = Scratch::new("hook-format-alone");
        let unwritable = scratch.root();
        let text = format_in_place(&crate::workspace_root(), unwritable, FORMATTED);
        assert_eq!(text.ok().as_deref(), Some(FORMATTED), "nothing to write, nothing fails");
    }

    // Goal: a syntax error is feedback for the agent (exit 2), and a file that cannot be written
    // is a failure of the hook itself (exit 1). Neither is silently passed.
    #[test]
    fn format_in_place_reports_syntax_errors_and_write_failures() {
        let scratch = Scratch::new("hook-format-errors");
        let root = crate::workspace_root();
        scratch.write("broken.rs", "fn a( {\n");
        let broken = format_in_place(&root, &scratch.root().join("broken.rs"), "fn a( {\n");
        let broken = broken.expect_err("a syntax error cannot be formatted");
        assert!(is(broken, ExitCode::from(EXIT_FEEDBACK)), "feedback, got {broken:?}");
        let unwritable = format_in_place(&root, scratch.root(), UNFORMATTED);
        let unwritable = unwritable.expect_err("a directory cannot be written as a file");
        assert!(is(unwritable, ExitCode::FAILURE), "a failure, got {unwritable:?}");
        assert!(!is(ExitCode::FAILURE, ExitCode::from(EXIT_FEEDBACK)), "the codes differ");
    }

    // Goal: the file path is extracted from a PostToolUse payload, with escapes decoded.
    #[test]
    fn string_fields_are_extracted() {
        let payload = concat!(
            r#"{"tool_name":"Edit","tool_input":"#,
            r#"{"file_path":"/a/b \"c\"\\d.rs","old_string":"x"}}"#
        );
        assert_eq!(json_string_field(payload, "file_path").as_deref(), Some("/a/b \"c\"\\d.rs"));
        assert_eq!(json_string_field(r#"{"cwd": "/w/é"}"#, "cwd").as_deref(), Some("/w/é"));
        assert_eq!(json_string_field(payload, "missing"), None);
    }

    // Goal: the stop-hook loop guard is read correctly, including whitespace around the colon.
    #[test]
    fn bool_fields_are_extracted() {
        assert_eq!(
            json_bool_field(r#"{"stop_hook_active" : true}"#, "stop_hook_active"),
            Some(true)
        );
        assert_eq!(
            json_bool_field(r#"{"stop_hook_active":false}"#, "stop_hook_active"),
            Some(false)
        );
        assert_eq!(json_bool_field(r#"{"stop_hook_active":"x"}"#, "stop_hook_active"), None);
    }

    // Goal: the checkout is found from any file or directory inside it, even one that does not
    // exist yet, and nothing is found outside a checkout. Each event takes it from its own
    // field, and only a payload without `cwd` falls back to the project directory.
    #[test]
    fn checkout_is_found_from_the_payload() {
        use super::{Event, checkout_around, hook_root};
        let root = std::fs::canonicalize(crate::workspace_root()).unwrap();
        let file = root.join("crates/base/src/lib.rs");
        assert_eq!(checkout_around(&file), Some(root.clone()));
        assert_eq!(checkout_around(&root.join("crates/base/src/new/new.rs")), Some(root.clone()));
        assert_eq!(checkout_around(std::path::Path::new("/")), None);

        let elsewhere = Some(std::path::Path::new("/"));
        let cwd = format!(r#"{{"session_id":"s","cwd":"{}"}}"#, file.display());
        assert_eq!(hook_root(Event::Stop, &cwd, elsewhere), Some(root.clone()), "the cwd wins");
        assert_eq!(hook_root(Event::SessionStart, &cwd, None), Some(root.clone()));
        assert_eq!(hook_root(Event::Stop, r#"{"cwd":"/"}"#, Some(&root)), None, "no fallback");
        assert_eq!(hook_root(Event::Stop, "{}", Some(&root)), Some(root.clone()), "fallback");
        assert_eq!(hook_root(Event::Stop, "{}", elsewhere), None, "the fallback is not nami");

        let edit = format!(r#"{{"cwd":"/","tool_input":{{"file_path":"{}"}}}}"#, file.display());
        assert_eq!(hook_root(Event::PostEdit, &edit, None), Some(root), "the file, not the cwd");
        assert_eq!(hook_root(Event::PostEdit, r#"{"cwd":"/"}"#, None), None, "no file");
    }

    // Goal: every event name round-trips, and anything else is not an event.
    #[test]
    fn events_are_parsed() {
        use super::Event;
        for event in [Event::PostEdit, Event::Stop, Event::SessionStart] {
            assert_eq!(Event::parse(event.name()), Some(event));
        }
        assert_eq!(Event::parse("subagent-stop"), None);
        assert_eq!(Event::parse(""), None);
    }

    // Goal: malformed or unterminated strings yield None instead of a partial value.
    #[test]
    fn malformed_strings_are_rejected() {
        assert_eq!(json_string_field(r#"{"file_path":"/a/b"#, "file_path"), None);
        assert_eq!(json_string_field(r#"{"file_path":42}"#, "file_path"), None);
    }

    // Goal: the gate is skipped only for a state that equals the base or the last pass. Method:
    // every combination of known and unknown inputs, including a state git cannot name.
    #[test]
    fn gate_runs_unless_the_state_is_known_good() {
        let (state, other) = (Some("a1"), Some("b2"));
        assert_eq!(gate_decision(state, state, None), Gate::Skip, "the unchanged base");
        assert_eq!(gate_decision(state, None, state), Gate::Skip, "the last passing state");
        assert_eq!(gate_decision(state, other, state), Gate::Skip, "passed, away from the base");
        assert_eq!(gate_decision(state, state, other), Gate::Skip, "back at the base");
        assert_eq!(gate_decision(state, other, other), Gate::Run, "a new state");
        assert_eq!(gate_decision(state, None, None), Gate::Run, "nothing to compare with");
        assert_eq!(gate_decision(state, other, None), Gate::Run, "changed, never passed");
        assert_eq!(gate_decision(None, None, None), Gate::Run, "no fingerprint");
        assert_eq!(gate_decision(None, state, state), Gate::Run, "no fingerprint, known others");
    }

    // Goal: a session is blocked exactly STOP_BLOCKS_MAX times in a row, whatever
    // `stop_hook_active` says, and the hook gives up on the next failure.
    #[test]
    fn failures_block_then_give_up() {
        for stop_hook_active in [false, true] {
            for failures in 0..STOP_BLOCKS_MAX {
                assert_eq!(
                    failure_decision(Some(failures), stop_hook_active),
                    Failure::Block { attempt: failures + 1 }
                );
            }
            assert_eq!(failure_decision(Some(STOP_BLOCKS_MAX), stop_hook_active), Failure::GiveUp);
            assert_eq!(failure_decision(Some(u32::MAX), stop_hook_active), Failure::GiveUp);
        }
        assert_eq!(STOP_BLOCKS_MAX, 3, "the documented number of blocked stops");
    }

    // Goal: without a session id there is no counter, so `stop_hook_active` decides: the first
    // failure blocks and says it is the last block, the second gives up.
    #[test]
    fn failures_without_a_session_follow_the_flag() {
        assert_eq!(failure_decision(None, false), Failure::Block { attempt: STOP_BLOCKS_MAX });
        assert_eq!(failure_decision(None, true), Failure::GiveUp);
    }

    // Goal: a stored count is read back, anything unreadable counts as no failures, and discard
    // removes the file. Method: observe first and assert after the clean-up, so that a failure
    // leaves no directory behind.
    #[test]
    fn stored_failures_are_read() {
        let directory = std::env::temp_dir().join(format!("nami-attempts-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("attempts-s");
        let missing = super::failures_stored(&file);
        std::fs::write(&file, "2\n").unwrap();
        let stored = super::failures_stored(&file);
        std::fs::write(&file, "many").unwrap();
        let not_a_number = super::failures_stored(&file);
        super::discard(Some(&file));
        let is_discarded = !file.exists();
        super::discard(Some(&file));
        super::discard(None);
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!((missing, stored, not_a_number), (0, 2, 0), "no file, a count, not a number");
        assert!(is_discarded, "discard removes the file");
    }

    // Goal: a session id becomes a safe file name: only `[A-Za-z0-9_-]`, bounded in length, and
    // a normal id is unchanged.
    #[test]
    fn session_ids_are_sanitised() {
        let uuid = "064968f0-a0a4-4f49-97d5-ecf023fde24d";
        assert_eq!(super::sanitise_session_id(uuid), uuid);
        assert_eq!(super::sanitise_session_id("aZ09_-"), "aZ09_-");
        assert_eq!(super::sanitise_session_id("../a b\\c.é"), "___a_b_c__");
        assert_eq!(super::sanitise_session_id(""), "");
        assert_eq!(super::sanitise_session_id(&"x".repeat(500)).len(), 128);
    }

    // Goal: only full hexadecimal object ids count as fingerprints.
    #[test]
    fn object_ids_are_recognised() {
        assert!(super::is_object_id(&"0aF9".repeat(10)), "SHA-1");
        assert!(super::is_object_id(&"0aF9".repeat(16)), "SHA-256");
        assert!(!super::is_object_id(&"0aF9".repeat(9)), "too short");
        assert!(!super::is_object_id(&"0aF9".repeat(11)), "between the two lengths");
        assert!(!super::is_object_id(&"0aG9".repeat(10)), "not hexadecimal");
        assert!(!super::is_object_id(""), "empty");
    }

    // Goal: the base is `main`, then `origin/main`, but never the branch that is checked out.
    #[test]
    fn base_candidates_exclude_the_current_branch() {
        let candidates = |branch| super::base_candidates(branch).collect::<Vec<&str>>();
        assert_eq!(candidates(None), ["main", "origin/main"]);
        assert_eq!(candidates(Some("work")), ["main", "origin/main"]);
        assert_eq!(candidates(Some("main")), ["origin/main"]);
    }

    // Goal: the tail keeps the last lines, and all of them if there are few.
    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(super::tail("a\nb\nc\n", 2), "b\nc");
        assert_eq!(super::tail("a\nb\nc\n", 3), "a\nb\nc");
        assert_eq!(super::tail("a\nb", 9), "a\nb");
        assert_eq!(super::tail("", 9), "");
    }

    // Goal: `$HOME/.cargo/bin` is found on a `PATH` only as a whole entry; an unknown `PATH`
    // does not list it.
    #[test]
    fn cargo_bin_is_found_on_path() {
        let lists = |path| super::path_lists_cargo_bin(path, "/home/u");
        assert!(lists(Some("/usr/bin:/home/u/.cargo/bin:/bin")), "in the middle");
        assert!(lists(Some("/home/u/.cargo/bin/")), "with a trailing slash");
        assert!(!lists(Some("/usr/bin:/bin")), "absent");
        assert!(!lists(Some("/home/u/.cargo/bin2:/home/other/.cargo/bin")), "other directories");
        assert!(!lists(Some("")), "empty");
        assert!(!lists(None), "unknown");
    }

    // Goal: the export line is added once, on a line of its own.
    #[test]
    fn export_line_is_added_once() {
        let export = super::CARGO_BIN_EXPORT;
        assert_eq!(export, "export PATH=\"$HOME/.cargo/bin:$PATH\"");
        assert_eq!(super::env_file_addition(""), Some(format!("{export}\n")));
        assert_eq!(super::env_file_addition("export A=1\n"), Some(format!("{export}\n")));
        assert_eq!(super::env_file_addition("export A=1"), Some(format!("\n{export}\n")));
        assert_eq!(super::env_file_addition(&format!("export A=1\n{export}\n")), None);
        assert_eq!(super::env_file_addition(&format!("  {export}")), None);
    }
}
