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
//! # Which checkout, and whose rules
//!
//! A subagent may work in its own git worktree, while Claude Code starts every hook command in
//! the session's project directory, so this process runs the project's xtask. The hook takes the
//! checkout from the payload instead: the nearest directory above the edited file, or above the
//! `cwd` at stop and session start, that holds `rust-toolchain.toml` and `xtask/`. Finding it
//! needs no git.
//!
//! This process owns the hook: the payload, the checkout, the stop gate's state, its limit and
//! the exit code. The checkout's own code runs only as a check, a plain command with a deadline
//! whose process group is killed when the deadline passes:
//!
//! - At stop, the gate is `just check dylint` in whatever nami checkout is around the `cwd`, as
//!   on `main`: that runs the checkout's own `justfile` and code, wherever it came from. A gate
//!   that fails, cannot start or runs out of time is a failed gate, never a pass.
//! - After an edit in another worktree of this repository, the file is formatted by the rustfmt
//!   of the toolchain that worktree pins and checked by its `cargo xtask tidy --file`, so that
//!   new rules on a branch apply to it. Such a worktree is one whose git toplevel is the
//!   checkout, whose git common directory is this repository's, and whose `HEAD` is a local
//!   branch: a detached `HEAD`, such as a fork's pull request checked out for review, is not.
//!   If its formatting or check cannot run, for example because the toolchain it pins is
//!   missing or its xtask is too old to know `--file`, this process formats or checks the file
//!   itself and says so. Any other checkout, and any that git cannot name, is formatted and
//!   checked by this process: an edit builds no code that is not on a local branch of this
//!   repository.
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
//!
//! The fingerprint is taken before the gate runs, and a pass is recorded under it. A file that
//! changes while the gate runs, for example by a background process, may not have been seen by
//! the gate, yet the recorded pass names the state from before. Any later change makes a new
//! fingerprint and runs the gate again; only a state that returns exactly to the recorded one
//! is skipped. CI checks every pushed state regardless (ADR-0009).

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::Duration;

/// Exit code that makes Claude Code show stderr to the agent.
const EXIT_FEEDBACK: u8 = 2;

/// Seconds that the worktree's own formatting and check of an edited file may take together,
/// each run's reap after a kill included (hook timeout: 120). What one run leaves, the next
/// gets. A first build of its xtask can take longer; then this process checks the file itself.
const EDIT_BUDGET_SECONDS_MAX: u32 = 80;

/// Seconds that each hook keeps, beyond the budget of its checks, for its own work: the fallback
/// check in this process after an edit, recording the outcome at stop.
#[cfg(test)]
const OWN_WORK_SECONDS_MIN: u32 = 30;

/// Seconds the stop gate may take, its reap after a kill included (hook timeout: 600). A cold
/// gate took 114 seconds.
const GATE_SECONDS_MAX: u32 = 480;

/// Seconds a killed check may take to end before the hook stops waiting for it.
const REAP_SECONDS_MAX: u32 = 5;

/// Milliseconds between two polls of a check for its end.
const POLL_INTERVAL_MILLISECONDS: u32 = 20;

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

/// The variable Claude Code sets to the directory where the session started.
const PROJECT_VARIABLE: &str = "CLAUDE_PROJECT_DIR";

/// Variables that rustup sets for the process it runs. Removed for a command in a checkout, so
/// that the toolchain that checkout pins applies, not the one this process runs with.
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

/// Runs the hook named `event`, reading its JSON payload from stdin, on the checkout the
/// payload is about (see [`hook_root`]).
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
    let project = std::env::var_os(PROJECT_VARIABLE).filter(|project| !project.is_empty());
    let root = hook_root(event, &payload, project.as_deref().map(Path::new));
    match event {
        Event::PostEdit => root.map_or(ExitCode::SUCCESS, |root| post_edit(&root, &payload)),
        Event::Stop => {
            if let Some(root) = root {
                stop(&root, &payload)
            } else {
                // Not a failure: a session may stop anywhere. But the human should know.
                eprintln!(
                    "hook stop: no nami checkout was found around the session's directory, so \
                     this stop was not gated"
                );
                ExitCode::SUCCESS
            }
        }
        Event::SessionStart => session_start(root.as_deref()),
    }
}

/// The checkout that a hook payload is about (see [`checkout_around`]):
///
/// - after an edit, the one that holds the edited file;
/// - at stop and session start, the one around the payload's `cwd`, or, if the payload has no
///   `cwd`, around `project` (`CLAUDE_PROJECT_DIR`), or else around the current directory.
///
/// `None` if that is not in a nami checkout, or if an edit names no file.
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
    assert!(root.is_absolute(), "the checkout is named by an absolute path");
    Some(root)
}

/// True if `root` is another worktree of the repository that this process runs in, on a local
/// branch, so that its own checks may run: git names `root` as its own toplevel, the same common
/// directory for both, and a branch for its `HEAD`. A checkout that git cannot name, a
/// nami-looking directory inside another checkout, a detached `HEAD` (such as a fork's pull
/// request that `git worktree add --detach` checked out for review, which shares this common
/// directory) and this process's own checkout are not.
fn is_other_worktree(root: &Path) -> bool {
    let current = std::env::current_dir().ok().and_then(|directory| checkout_around(&directory));
    let Some(current) = current else {
        return false;
    };
    let canonical = |path: &Path| fs::canonicalize(path).ok();
    if canonical(&current).is_none() || canonical(&current) == canonical(root) {
        return false;
    }
    let toplevel = git(root, None, &["rev-parse", "--show-toplevel"]);
    if toplevel.and_then(|toplevel| canonical(Path::new(&toplevel))) != canonical(root) {
        return false;
    }
    if git(root, None, &["symbolic-ref", "--quiet", "HEAD"]).is_none() {
        return false;
    }
    let common = |checkout: &Path| {
        let arguments = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
        git(checkout, None, &arguments).and_then(|directory| canonical(Path::new(&directory)))
    };
    let current_common = common(&current);
    current_common.is_some() && current_common == common(root)
}

/// What a check printed, stream by stream.
#[derive(Debug, PartialEq, Eq)]
struct Printed {
    stdout: String,
    stderr: String,
}

impl Printed {
    /// Both streams, stdout first.
    fn both(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// What became of a check that ran in a checkout.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// It exited with this code and printed this.
    Exited(i32, Printed),
    /// It could not start, was killed at its deadline, or ended by a signal.
    CouldNotRun(String),
}

/// Runs `command` in its own process group, with stdin from `input` (or nothing), stdout and
/// stderr in files next to `log`, and waits for it until its deadline in `budget`, in polls of
/// [`POLL_INTERVAL_MILLISECONDS`]. The deadline counts polls, not wall-clock time (ADR-0005).
fn run_bounded(
    command: &mut Command,
    input: Option<&Path>,
    log: &Path,
    budget: &mut Budget,
) -> Outcome {
    assert!(log.is_absolute(), "the logs of a run are named by an absolute path");
    assert!(input.is_none_or(Path::is_absolute), "the input of a run is an absolute path");
    if budget.deadline() == 0 {
        return Outcome::CouldNotRun("was not started: its time budget is spent".to_owned());
    }
    let (out_log, err_log) = (log.with_extension("out"), log.with_extension("err"));
    let stdin = match input.map(fs::File::open).transpose() {
        Ok(file) => file.map_or_else(Stdio::null, Stdio::from),
        Err(error) => return Outcome::CouldNotRun(format!("cannot read its input: {error}")),
    };
    let files = fs::File::create(&out_log).and_then(|out| Ok((out, fs::File::create(&err_log)?)));
    let (stdout, stderr) = match files {
        Ok(files) => files,
        Err(error) => {
            return Outcome::CouldNotRun(format!("cannot create {}: {error}", log.display()));
        }
    };
    let command = command.stdin(stdin).stdout(stdout).stderr(stderr);
    let spawned = in_own_process_group(command).spawn();
    let status = spawned.map(|mut child| wait_until(&mut child, budget));
    let read = |file: &Path| match fs::read(file) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => format!("(cannot read the output in {}: {error})", file.display()),
    };
    let printed = Printed { stdout: read(&out_log), stderr: read(&err_log) };
    discard(Some(&out_log));
    discard(Some(&err_log));
    match status {
        Err(error) => Outcome::CouldNotRun(format!("cannot start: {error}")),
        Ok(Ok(status)) => match status.code() {
            Some(code) => Outcome::Exited(code, printed),
            None => Outcome::CouldNotRun(format!("ended by {status}\n{}", printed.both())),
        },
        Ok(Err(error)) => Outcome::CouldNotRun(format!("{error}\n{}", printed.both())),
    }
}

/// Polls of [`POLL_INTERVAL_MILLISECONDS`] in `seconds`.
fn polls_in(seconds: u32) -> u32 {
    let polls = seconds * 1000 / POLL_INTERVAL_MILLISECONDS;
    assert!(polls > 0, "a deadline gets at least one poll");
    polls
}

/// Polls that a run, or a series of runs that share it, may still take, each run's reap after a
/// kill included. A run's deadline is what is left minus one reap, so a series of runs can never
/// take more polls than its budget.
///
/// A budget counts polls, not wall-clock time (ADR-0005). Each poll sleeps
/// [`POLL_INTERVAL_MILLISECONDS`], and the sleeps' overrun, the time `try_wait` takes, the git
/// calls around the runs and the fallback check all come out of the headroom that the hook keeps
/// below Claude Code's timeout: 40 seconds after an edit, 120 at stop.
#[derive(Debug)]
struct Budget {
    polls_left: u32,
}

impl Budget {
    /// A budget of `seconds`.
    fn of_seconds(seconds: u32) -> Self {
        Self { polls_left: polls_in(seconds) }
    }

    /// The polls a run may wait before it is killed: what is left minus one reap, or none.
    fn deadline(&self) -> u32 {
        self.polls_left.saturating_sub(polls_in(REAP_SECONDS_MAX))
    }

    /// Takes one poll from the budget.
    fn spend(&mut self) {
        assert!(self.polls_left > 0, "a run never polls beyond its budget");
        self.polls_left -= 1;
    }
}

/// Waits for `child` until its deadline in `budget`, then kills it and its process group and
/// waits at most one reap for it to end. Every poll is taken from `budget`.
fn wait_until(child: &mut Child, budget: &mut Budget) -> Result<ExitStatus, String> {
    let interval = Duration::from_millis(u64::from(POLL_INTERVAL_MILLISECONDS));
    let deadline = budget.deadline();
    assert!(deadline > 0, "a run is started only with a deadline left in its budget");
    let polls_before = budget.polls_left;
    let result = wait_within(child, budget, deadline, interval);
    assert!(budget.polls_left < polls_before, "every wait takes at least one poll");
    result
}

/// [`wait_until`] without its checks: polls `child` up to `deadline` times, then kills it.
fn wait_within(
    child: &mut Child,
    budget: &mut Budget,
    deadline: u32,
    interval: Duration,
) -> Result<ExitStatus, String> {
    for _ in 0..deadline {
        budget.spend();
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => std::thread::sleep(interval),
            Err(error) => {
                // A child that cannot be waited for is not left running.
                let killed = match kill_process_group(child) {
                    Ok(()) => "its process group was killed".to_owned(),
                    Err(kill_error) => kill_error,
                };
                return Err(format!("cannot be waited for: {error}; {killed}"));
            }
        }
    }
    if let Ok(Some(status)) = child.try_wait() {
        return Ok(status);
    }
    let group = kill_process_group(child);
    // The reap is bounded too, so that a child the kill did not reach cannot hang the hook.
    let mut reaped = Err("it did not end after the kill".to_owned());
    for _ in 0..polls_in(REAP_SECONDS_MAX).min(budget.polls_left) {
        budget.spend();
        match child.try_wait() {
            Ok(Some(status)) => {
                reaped = Ok(status);
                break;
            }
            Ok(None) => std::thread::sleep(interval),
            Err(error) => {
                reaped = Err(format!("cannot be waited for: {error}"));
                break;
            }
        }
    }
    let killed = match (reaped, group) {
        (Ok(status), Ok(())) => status.to_string(),
        (Ok(status), Err(error)) => format!("{status}; {error}"),
        (Err(error), _) => error,
    };
    let seconds = deadline * POLL_INTERVAL_MILLISECONDS / 1000;
    Err(format!("did not finish within {seconds} seconds and was killed ({killed})"))
}

/// Starts the child in a process group of its own, so that a kill reaches its children too.
/// The ratchet's null-mutant run uses it as well.
pub(crate) fn in_own_process_group(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    let command = std::os::unix::process::CommandExt::process_group(command, 0);
    command
}

/// Kills `child`, and on Unix every process in its group: cargo's build or the gate's `just`.
/// The child itself is killed in any case. Returns what went wrong with the group, if anything.
pub(crate) fn kill_process_group(child: &mut Child) -> Result<(), String> {
    #[cfg(unix)]
    let group = {
        let group = format!("-{}", child.id());
        match Command::new("kill").args(["-KILL", "--", &group]).stderr(Stdio::null()).status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("cannot kill the process group {group}: {status}")),
            Err(error) => Err(format!("cannot kill the process group {group}: {error}")),
        }
    };
    #[cfg(not(unix))]
    let group = Ok(());
    // The child is not reaped yet, so it can always be signalled, even after the group kill.
    child.kill().map_err(|error| format!("cannot kill the process {}: {error}", child.id()))?;
    group
}

/// A file in the gate's state directory of `root` for one run's output, named for this process.
fn output_log(root: &Path, purpose: &str) -> PathBuf {
    let state = gate_state(root);
    if let Err(error) = fs::create_dir_all(&state) {
        eprintln!("hook: cannot create {}: {error}", state.display());
    }
    state.join(format!("{purpose}-{}.log", std::process::id()))
}

fn post_edit(root: &Path, payload: &str) -> ExitCode {
    // One budget for all of the worktree's own work, so that the fallback in `check_edit`
    // always has time before the hook's timeout.
    let work = is_other_worktree(root).then(|| WorktreeWork {
        budget: Budget::of_seconds(EDIT_BUDGET_SECONDS_MAX),
        rustfmt: "rustfmt",
        cargo: "cargo",
    });
    check_edit(root, payload, work)
}

/// The work that a worktree's own code does after an edit in it: the programs it runs, found on
/// `PATH`, and the one budget that they share.
struct WorktreeWork<'a> {
    budget: Budget,
    rustfmt: &'a str,
    cargo: &'a str,
}

/// Formats and checks the file that a `PostToolUse` payload names, in the checkout at `root`:
/// with `work`, by that worktree's own rustfmt and rules first, else by this process.
fn check_edit(root: &Path, payload: &str, mut work: Option<WorktreeWork<'_>>) -> ExitCode {
    let Some(file) = json_string_field(payload, "file_path").map(PathBuf::from) else {
        return ExitCode::SUCCESS;
    };
    // A deleted file, a directory or a binary file has nothing to check.
    let Ok(original) = std::fs::read_to_string(&file) else {
        return ExitCode::SUCCESS;
    };
    let Ok(file) = std::path::absolute(&file) else {
        return ExitCode::SUCCESS;
    };
    let Ok(relative) = file.strip_prefix(root) else {
        eprintln!("hook post-edit: {} is not in {}", file.display(), root.display());
        return ExitCode::FAILURE;
    };
    let is_rust = file.extension().is_some_and(|extension| extension == "rs");
    let text = if is_rust {
        match format_in_place(root, &file, &original, work.as_mut()) {
            Ok(formatted) => formatted,
            Err(exit) => return exit,
        }
    } else {
        original
    };
    if let Some(work) = work.as_mut() {
        match own_file_check(root, &file, work) {
            Ok(exit) => return exit,
            Err(reason) => eprintln!(
                "hook post-edit: the check of {} could not run ({reason}); checked with the \
                 rules of the project instead, not those of that worktree",
                root.display()
            ),
        }
    }
    let violations = match crate::tidy::check_single_file(root, relative, &text) {
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

/// Checks `file` with the rules of the worktree at `root`: `cargo xtask tidy --file` in it,
/// with the toolchain it pins. Returns the exit code to stop with if the check ran: it passed,
/// or it found violations, which go to the agent. Otherwise returns why it could not run.
fn own_file_check(
    root: &Path,
    file: &Path,
    work: &mut WorktreeWork<'_>,
) -> Result<ExitCode, String> {
    assert!(file.is_absolute(), "the worktree's check is given an absolute path");
    let mut command = Command::new(work.cargo);
    let command = command
        .args(["xtask", "tidy", "--file"])
        .arg(file)
        .current_dir(root)
        .env_remove(TOOLCHAIN_VARIABLES[0])
        .env_remove(TOOLCHAIN_VARIABLES[1]);
    let log = output_log(root, "edit-check");
    let violations = i32::from(crate::tidy::cli::EXIT_VIOLATIONS);
    match run_bounded(command, None, &log, &mut work.budget) {
        Outcome::Exited(0, _) => Ok(ExitCode::SUCCESS),
        Outcome::Exited(code, printed) if code == violations => {
            eprint!("{}", printed.both());
            Ok(ExitCode::from(EXIT_FEEDBACK))
        }
        Outcome::Exited(code, printed) => {
            let output = tail(&printed.both(), 10);
            Err(format!("`cargo xtask tidy --file` exited {code}: {output}"))
        }
        Outcome::CouldNotRun(reason) => Err(format!("`cargo xtask tidy --file` {reason}")),
    }
}

/// Formats a Rust file and writes it back if the text changed. With `checkout_work`, the
/// rustfmt of the toolchain that `root` pins formats it within that work's budget, or, if that
/// cannot run, this process's own. Returns the formatted text, or the exit code to stop with:
/// feedback for a syntax error, failure if the file cannot be written.
fn format_in_place(
    root: &Path,
    file: &Path,
    original: &str,
    checkout_work: Option<&mut WorktreeWork<'_>>,
) -> Result<String, ExitCode> {
    let formatted = match checkout_work {
        Some(work) => match format_with_checkout_toolchain(root, original, work) {
            Err(error) => format_source(root, original).inspect(|_| {
                let first = error.lines().next().unwrap_or_default();
                eprintln!("note: the rustfmt that {} pins did not run ({first})", root.display());
            }),
            formatted => formatted,
        },
        None => format_source(root, original),
    };
    let text = match formatted {
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
/// has its own last pass and its own counters, and only this process writes it.
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
    let output = match run_gate(root) {
        Ok(()) => {
            if let Some(fingerprint) = fingerprint
                && let Err(error) = fs::write(state.join(GATE_PASSED_FILE), fingerprint)
            {
                eprintln!("hook stop: cannot record the passing state: {error}");
            }
            discard(attempts.as_deref());
            return ExitCode::SUCCESS;
        }
        Err(output) => output,
    };
    let stop_hook_active = json_bool_field(payload, "stop_hook_active") == Some(true);
    stop_failed(attempts.as_deref(), stop_hook_active, &tail(&output, FEEDBACK_LINES_MAX))
}

/// Runs `just check dylint` in `root` with the toolchain it pins, within [`GATE_SECONDS_MAX`].
/// Returns the output of a gate that did not pass: one that failed, could not start, or ran
/// out of time. Each of them is a failed gate.
fn run_gate(root: &Path) -> Result<(), String> {
    let mut command = Command::new("just");
    let command = command
        .args(GATE_RECIPES)
        .current_dir(root)
        .env_remove(TOOLCHAIN_VARIABLES[0])
        .env_remove(TOOLCHAIN_VARIABLES[1]);
    let mut budget = Budget::of_seconds(GATE_SECONDS_MAX);
    match run_bounded(command, None, &output_log(root, "gate"), &mut budget) {
        Outcome::Exited(0, _) => Ok(()),
        Outcome::Exited(_, printed) => Err(printed.both()),
        Outcome::CouldNotRun(reason) => Err(format!("`just check dylint` {reason}")),
    }
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
/// into sibling files, which may not exist yet. Runs in `root` to pick up `rustfmt.toml`, with
/// this process's toolchain.
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

/// Formats one file's source like [`format_source`], but with the rustfmt of the toolchain that
/// `root` pins, as a bounded check: within what is left of the work's budget, its process group
/// killed at the deadline. Returns what went wrong if it did not format the source, so that the
/// caller can fall back to this process's rustfmt.
fn format_with_checkout_toolchain(
    root: &Path,
    source: &str,
    work: &mut WorktreeWork<'_>,
) -> Result<String, String> {
    let input = output_log(root, "format-input");
    let written = fs::write(&input, source);
    written.map_err(|error| format!("cannot write {}: {error}", input.display()))?;
    let mut command = Command::new(work.rustfmt);
    let command = command
        .args(["--edition", "2024", "--emit", "stdout"])
        .current_dir(root)
        .env_remove(TOOLCHAIN_VARIABLES[0])
        .env_remove(TOOLCHAIN_VARIABLES[1]);
    let log = output_log(root, "format");
    let outcome = run_bounded(command, Some(&input), &log, &mut work.budget);
    discard(Some(&input));
    match outcome {
        Outcome::Exited(0, printed) => Ok(printed.stdout),
        Outcome::Exited(code, printed) => Err(format!("exit {code}: {}", printed.stderr)),
        Outcome::CouldNotRun(reason) => Err(reason),
    }
}

/// The directory where the stop gate keeps its state for the checkout at `root`.
#[must_use]
pub fn gate_state(root: &Path) -> PathBuf {
    root.join(GATE_STATE_DIRECTORY)
}

/// The nami checkout or worktree that holds `path`: the nearest directory at or above it that
/// holds `rust-toolchain.toml` and `xtask/`. `path` may be a file, or not exist yet. Needs no
/// git, so a checkout is found even where git cannot run, and a git repository nested in a
/// checkout belongs to the checkout around it.
#[must_use]
pub fn checkout_around(path: &Path) -> Option<PathBuf> {
    let path = std::path::absolute(path).ok()?;
    let mut candidate = Some(path.as_path());
    for _ in 0..ANCESTORS_MAX {
        let directory = candidate?;
        if directory.join("rust-toolchain.toml").is_file() && directory.join("xtask").is_dir() {
            return Some(directory.to_path_buf());
        }
        candidate = directory.parent();
    }
    None
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

    /// The worktree's own work after an edit, as the hook does it.
    fn edit_work() -> super::WorktreeWork<'static> {
        let budget = super::Budget::of_seconds(super::EDIT_BUDGET_SECONDS_MAX);
        super::WorktreeWork { budget, rustfmt: "rustfmt", cargo: "cargo" }
    }

    // Goal: an unformatted Rust file is rewritten in its formatted form, and the formatted text
    // is what the tidy rules then see.
    #[test]
    fn format_in_place_rewrites_an_unformatted_file() {
        let scratch = Scratch::new("hook-format-rewrite");
        scratch.write("lib.rs", UNFORMATTED);
        let file = scratch.root().join("lib.rs");
        let text = format_in_place(&crate::workspace_root(), &file, UNFORMATTED, None);
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
        let text = format_in_place(
            &crate::workspace_root(),
            unwritable,
            FORMATTED,
            Some(&mut edit_work()),
        );
        assert_eq!(text.ok().as_deref(), Some(FORMATTED), "nothing to write, nothing fails");
    }

    // Goal: a syntax error is feedback for the agent (exit 2), and a file that cannot be written
    // is a failure of the hook itself (exit 1). Neither is silently passed.
    #[test]
    fn format_in_place_reports_syntax_errors_and_write_failures() {
        let scratch = Scratch::new("hook-format-errors");
        let root = crate::workspace_root();
        scratch.write("broken.rs", "fn a( {\n");
        let broken = format_in_place(
            &root,
            &scratch.root().join("broken.rs"),
            "fn a( {\n",
            Some(&mut edit_work()),
        );
        let broken = broken.expect_err("a syntax error cannot be formatted");
        assert!(is(broken, ExitCode::from(EXIT_FEEDBACK)), "feedback, got {broken:?}");
        let unwritable = format_in_place(&root, scratch.root(), UNFORMATTED, None);
        let unwritable = unwritable.expect_err("a directory cannot be written as a file");
        assert!(is(unwritable, ExitCode::FAILURE), "a failure, got {unwritable:?}");
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
    // exist yet, without git, and a git repository nested in it belongs to it; nothing is found
    // outside a checkout. Each event takes it from its own field, and only a payload without
    // `cwd` falls back to the project directory.
    #[test]
    fn checkout_is_found_from_the_payload() {
        use super::{Event, checkout_around, hook_root};
        let scratch = Scratch::new("hook-checkout");
        scratch.write("rust-toolchain.toml", "[toolchain]\n");
        scratch.write("xtask/README", "tooling\n");
        scratch.write("crates/base/src/lib.rs", "\n");
        scratch.write("target/tmp/nested/.git/HEAD", "ref: refs/heads/main\n");
        let root = scratch.root().to_path_buf();
        let file = root.join("crates/base/src/lib.rs");
        assert_eq!(checkout_around(&file), Some(root.clone()));
        assert_eq!(checkout_around(&root.join("crates/base/src/new/new.rs")), Some(root.clone()));
        let nested = root.join("target/tmp/nested/src/lib.rs");
        assert_eq!(checkout_around(&nested), Some(root.clone()), "a nested repository");
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

    /// `cat` reading a pipe that the test holds open: it cannot end until it is killed.
    fn blocked_cat(command: &mut std::process::Command) -> std::process::Child {
        command.stdin(std::process::Stdio::piped()).spawn().unwrap()
    }

    /// A budget whose deadline is `polls`, with one reap on top.
    fn budget_with_deadline(polls: u32) -> super::Budget {
        super::Budget { polls_left: polls + super::polls_in(super::REAP_SECONDS_MAX) }
    }

    // Goal: a check that outlives its deadline is killed with its whole process group, and one
    // that ends in time is waited for. Method: event-driven, no clock: two `cat`s in one group
    // block on pipes the test holds open, so only a kill can end them, and the test reaps both.
    #[test]
    fn checks_are_killed_with_their_group_at_the_deadline() {
        use super::{in_own_process_group, wait_until};
        let mut leader = blocked_cat(in_own_process_group(&mut std::process::Command::new("cat")));
        let group = i32::try_from(leader.id()).unwrap();
        let mut member_command = std::process::Command::new("cat");
        let member_command =
            std::os::unix::process::CommandExt::process_group(&mut member_command, group);
        let mut member = blocked_cat(member_command);
        let mut budget = budget_with_deadline(50);
        let error = wait_until(&mut leader, &mut budget).expect_err("only a kill ends the leader");
        assert!(error.starts_with("did not finish within 1 seconds and was killed"), "{error}");
        assert!(error.ends_with("(signal: 9 (SIGKILL))"), "by SIGKILL, group and all: {error}");
        assert_eq!(budget.deadline(), 0, "the deadline is spent");
        let member_status = member.wait().unwrap();
        let signal = std::os::unix::process::ExitStatusExt::signal(&member_status);
        assert_eq!(signal, Some(9), "the group kill reached the member: {member_status}");
        let mut quick = std::process::Command::new("true").spawn().unwrap();
        let mut ample = super::Budget::of_seconds(super::GATE_SECONDS_MAX);
        let status = wait_until(&mut quick, &mut ample).expect("`true` ends by itself");
        assert!(status.success(), "its status is kept: {status}");
    }

    // Goal: a child that leads no process group of its own is still killed, and the failed
    // group kill is reported, not passed over.
    #[test]
    fn a_failed_group_kill_is_reported_and_the_child_still_dies() {
        let mut child = blocked_cat(&mut std::process::Command::new("cat"));
        let error = super::kill_process_group(&mut child).expect_err("it leads no group");
        assert!(error.starts_with("cannot kill the process group -"), "{error}");
        let status = child.wait().unwrap();
        assert!(!status.success(), "the child was killed: {status}");
    }

    // Goal: a bounded run reads its input from a file, reports the exit code with stdout and
    // stderr apart, and a check that cannot start, is ended by a signal or outlives its deadline
    // as one that could not run, never as an exit code. Method: no case depends on the clock.
    // A case that ends by itself gets the gate's whole budget; the one that must be killed
    // blocks on a FIFO that the test made and never writes, so it cannot end by itself.
    #[test]
    fn bounded_runs_tell_an_exit_from_a_check_that_could_not_run() {
        use super::{Budget, GATE_SECONDS_MAX, Outcome, Printed, run_bounded};
        let scratch = Scratch::new("hook-bounded");
        let log = scratch.root().join("run.log");
        let shell = |script: &str, input: Option<&std::path::Path>, budget: &mut Budget| {
            let mut command = std::process::Command::new("sh");
            let command = command.args(["-c", script]).current_dir(scratch.root());
            run_bounded(command, input, &log, budget)
        };
        let ample = || Budget::of_seconds(GATE_SECONDS_MAX);
        let exited = shell("echo out; echo err >&2; exit 7", None, &mut ample());
        let printed = Printed { stdout: "out\n".to_owned(), stderr: "err\n".to_owned() };
        assert_eq!(exited, Outcome::Exited(7, printed), "both streams, apart");
        assert!(!log.with_extension("out").exists(), "the logs are removed after the run");
        assert!(!log.with_extension("err").exists(), "both of them");
        scratch.write("input.txt", "from the file\n");
        let input = scratch.root().join("input.txt");
        let read = shell("cat", Some(&input), &mut ample());
        let Outcome::Exited(0, read) = read else { panic!("`cat` of the input fails: {read:?}") };
        assert_eq!(read.stdout, "from the file\n", "stdin comes from the input file");
        let empty = shell("cat", None, &mut ample());
        let Outcome::Exited(0, empty) = empty else { panic!("`cat` of nothing fails: {empty:?}") };
        assert_eq!(empty.stdout, "", "and without one, stdin is empty");
        let mut nonexistent = std::process::Command::new("/nonexistent");
        let missing = run_bounded(&mut nonexistent, None, &log, &mut ample());
        let Outcome::CouldNotRun(unstarted) = missing else { panic!("it ran: {missing:?}") };
        assert!(unstarted.starts_with("cannot start: "), "{unstarted}");
        let signalled = shell("echo before; kill -TERM $$", None, &mut ample());
        let Outcome::CouldNotRun(ended) = signalled else { panic!("it exited: {signalled:?}") };
        assert!(ended.starts_with("ended by signal: 15"), "{ended}");
        assert!(ended.ends_with("before\n"), "with its output: {ended}");
        let mut fifo = std::process::Command::new("mkfifo");
        let made = fifo.arg(scratch.root().join("f")).status().unwrap();
        assert!(made.success(), "the FIFO is made before the check starts");
        let blocked = shell("read x < f", None, &mut budget_with_deadline(5));
        let Outcome::CouldNotRun(killed) = blocked else { panic!("it ended: {blocked:?}") };
        assert!(killed.starts_with("did not finish within 0 seconds and was killed"), "{killed}");
    }

    // Goal: after an edit, the worktree's formatting and its check share one budget: a rustfmt
    // that uses all of it up leaves none for the check, which is not started, and this process
    // still formats and checks the file. Method: event-driven, no clock. The worktree's rustfmt
    // blocks on a FIFO that nothing writes, so only its deadline ends it; its cargo would leave
    // a marker if it ran.
    #[test]
    fn the_worktree_edit_work_shares_one_budget() {
        use super::{WorktreeWork, check_edit};
        let scratch = Scratch::new("hook-shared-budget");
        let workspace = crate::workspace_root();
        let toolchain = std::fs::read_to_string(workspace.join("rust-toolchain.toml")).unwrap();
        scratch.write("rust-toolchain.toml", &toolchain);
        scratch.write("xtask/README", "tooling\n");
        // Unformatted, and with a debt marker that tidy reports. The marker is put together
        // here so that this file does not carry it.
        let debt = ["TO", "DO"].concat();
        scratch.write("src/lib.rs", &format!("fn a( ) {{}} // {debt} later\n"));
        let root = scratch.root();
        let mut fifo = std::process::Command::new("mkfifo");
        assert!(fifo.arg(root.join("f")).status().unwrap().success(), "the FIFO is made");
        let marker = root.join("cargo-ran");
        scratch
            .write("bin/rustfmt", &format!("#!/bin/sh\nread x < {}\n", root.join("f").display()));
        scratch.write("bin/cargo", &format!("#!/bin/sh\ntouch {}\n", marker.display()));
        for tool in ["bin/rustfmt", "bin/cargo"] {
            let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            std::fs::set_permissions(root.join(tool), executable).unwrap();
        }
        let rustfmt = root.join("bin/rustfmt").display().to_string();
        let cargo = root.join("bin/cargo").display().to_string();
        let work =
            WorktreeWork { budget: budget_with_deadline(5), rustfmt: &rustfmt, cargo: &cargo };
        let file = root.join("src/lib.rs");
        let payload = format!(r#"{{"tool_input":{{"file_path":"{}"}}}}"#, file.display());
        let exit = check_edit(root, &payload, Some(work));
        let feedback = ExitCode::from(EXIT_FEEDBACK);
        assert!(is(exit, feedback), "this process checked the file and found the marker: {exit:?}");
        assert!(!marker.exists(), "the worktree's check was not started: the budget was spent");
        let formatted = std::fs::read_to_string(&file).unwrap();
        let expected = format!("fn a() {{}} // {debt} later\n");
        assert_eq!(formatted, expected, "and this process's rustfmt rewrote it");
    }

    // Goal: runs that share a budget never take longer than it, reaps included: what one run
    // spends, the next cannot, and a spent budget starts nothing. Method: two runs that only a
    // kill can end share a small budget; the second, and a third, are not started.
    #[test]
    fn runs_share_one_budget_and_a_spent_one_starts_nothing() {
        use super::{Outcome, run_bounded};
        let scratch = Scratch::new("hook-budget");
        let log = scratch.root().join("run.log");
        let mut fifo = std::process::Command::new("mkfifo");
        let made = fifo.arg(scratch.root().join("f")).status().unwrap();
        assert!(made.success(), "the FIFO is made before the checks start");
        let mut budget = budget_with_deadline(5);
        let total = budget.polls_left;
        let mut run = |script: &str| {
            let mut command = std::process::Command::new("sh");
            let command = command.args(["-c", script]).current_dir(scratch.root());
            run_bounded(command, None, &log, &mut budget)
        };
        let first = run("read x < f");
        let Outcome::CouldNotRun(killed) = first else { panic!("it ended: {first:?}") };
        assert!(killed.starts_with("did not finish within 0 seconds"), "{killed}");
        for _ in 0..2 {
            let next = run("touch started; read x < f");
            let Outcome::CouldNotRun(refused) = next else { panic!("it ran: {next:?}") };
            assert_eq!(refused, "was not started: its time budget is spent", "nothing is left");
        }
        assert!(!scratch.root().join("started").exists(), "a spent budget starts nothing");
        assert!(budget.polls_left < total, "the first run spent its share: {budget:?}");
        assert_eq!(budget.deadline(), 0, "and left no deadline for another");
    }

    // Goal: the worst case of each hook, every run's deadline and reap within its budget plus
    // the time the hook keeps for its own work, ends before Claude Code's timeout for that hook
    // in `.claude/settings.json`: after an edit, the fallback check still runs.
    #[test]
    fn budgets_are_within_the_hook_timeouts() {
        use super::{EDIT_BUDGET_SECONDS_MAX, GATE_SECONDS_MAX, OWN_WORK_SECONDS_MIN};
        let settings =
            std::fs::read_to_string(crate::workspace_root().join(".claude/settings.json"))
                .expect("the settings are readable");
        let settings = crate::json::parse(&settings).expect("the settings are JSON");
        let timeout = |event: &str| -> u32 {
            let hook = settings.get("hooks").and_then(|hooks| hooks.get(event));
            let hook = hook.and_then(|entries| entries.as_array()?.first()?.get("hooks"));
            let timeout = hook.and_then(|hooks| hooks.as_array()?.first()?.get("timeout"));
            let Some(crate::json::Value::Number(seconds)) = timeout else {
                panic!("the {event} hook has no timeout: {timeout:?}");
            };
            seconds.parse().expect("the timeout is a whole number of seconds")
        };
        let worst = |budget| super::Budget::of_seconds(budget).polls_left;
        let seconds = |polls: u32| polls * super::POLL_INTERVAL_MILLISECONDS / 1000;
        assert_eq!(super::polls_in(1), 50, "one second is 50 polls of 20 ms");
        assert_eq!(seconds(worst(EDIT_BUDGET_SECONDS_MAX)), 80, "the edit budget is 80 s");
        assert_eq!(seconds(worst(GATE_SECONDS_MAX)), 480, "the gate budget is 480 s");
        let edit = seconds(worst(EDIT_BUDGET_SECONDS_MAX)) + OWN_WORK_SECONDS_MIN;
        assert!(edit <= timeout("PostToolUse"), "{edit} s exceed the edit hook's timeout");
        for event in ["Stop", "SubagentStop"] {
            let stop = seconds(worst(GATE_SECONDS_MAX)) + OWN_WORK_SECONDS_MIN;
            assert!(stop <= timeout(event), "{stop} s exceed the {event} hook's timeout");
        }
        assert_eq!((timeout("PostToolUse"), timeout("Stop")), (120, 600), "the timeouts read");
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
