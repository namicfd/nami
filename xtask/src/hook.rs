//! Claude Code hooks (ADR-0008, `.claude/settings.json`).
//!
//! Hooks are defence in depth: they give agents fast feedback, while the server-side gates stay
//! authoritative (ADR-0009). Each hook reads the event JSON on stdin. Exit code 2 sends stderr
//! back to the agent; any other failure is reported to the human as a hook error.
//!
//! - `post-edit`: formats an edited Rust file and runs the single-file tidy rules on it.
//! - `stop`: runs `just check` before an agent or subagent may finish, if anything changed.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Exit code that makes Claude Code show stderr to the agent.
const EXIT_FEEDBACK: u8 = 2;

/// Upper bound on directories walked up when looking for the workspace root.
const ANCESTORS_MAX: u32 = 64;

/// Upper bound on characters read from one JSON string value.
const JSON_STRING_CHARACTERS_MAX: u32 = 65_536;

/// Lines of failing `just check` output passed back to the agent.
const FEEDBACK_LINES_MAX: usize = 60;

/// Runs the hook named `event`, reading its JSON payload from stdin.
pub fn run(event: &str) -> ExitCode {
    let mut payload = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut payload) {
        eprintln!("hook {event}: cannot read stdin: {error}");
        return ExitCode::FAILURE;
    }
    match event {
        "post-edit" => post_edit(&payload),
        "stop" => stop(&payload),
        _ => {
            eprintln!("hook: unknown event `{event}`; expected post-edit or stop");
            ExitCode::FAILURE
        }
    }
}

fn post_edit(payload: &str) -> ExitCode {
    let Some(file) = json_string_field(payload, "file_path").map(PathBuf::from) else {
        return ExitCode::SUCCESS;
    };
    let is_rust = file.extension().is_some_and(|extension| extension == "rs");
    let Some(root) = workspace_root_of(&file) else {
        return ExitCode::SUCCESS;
    };
    if !is_rust || !file.is_file() {
        return ExitCode::SUCCESS;
    }
    let formatted = Command::new("rustfmt").arg("--edition").arg("2024").arg(&file).status();
    if !formatted.is_ok_and(|status| status.success()) {
        eprintln!("rustfmt failed on {}; fix the syntax error first", file.display());
        return ExitCode::from(EXIT_FEEDBACK);
    }
    let relative = file.strip_prefix(&root).expect("root is an ancestor of the file");
    let Ok(text) = std::fs::read_to_string(&file) else {
        return ExitCode::SUCCESS;
    };
    let (violations, _stats) = crate::tidy::check_rust_source(relative, &text);
    if violations.is_empty() {
        return ExitCode::SUCCESS;
    }
    for violation in &violations {
        eprintln!("{violation}");
    }
    ExitCode::from(EXIT_FEEDBACK)
}

fn stop(payload: &str) -> ExitCode {
    // A previous stop hook already sent the agent back to work; let it stop this time, so that
    // a check it cannot fix does not trap it in a loop. CI still runs every check.
    if json_bool_field(payload, "stop_hook_active") == Some(true) {
        return ExitCode::SUCCESS;
    }
    let directory =
        json_string_field(payload, "cwd").map_or_else(|| PathBuf::from("."), PathBuf::from);
    let Some(root) = workspace_root_of(&directory) else {
        return ExitCode::SUCCESS;
    };
    if !has_changes(&root) {
        return ExitCode::SUCCESS;
    }
    let Ok(output) = Command::new("just").arg("check").current_dir(&root).output() else {
        eprintln!("hook stop: cannot run `just check`; is `just` installed?");
        return ExitCode::FAILURE;
    };
    if output.status.success() {
        return ExitCode::SUCCESS;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines.iter().skip(lines.len().saturating_sub(FEEDBACK_LINES_MAX));
    eprintln!("`just check` fails, so the work is not done yet. Fix these and try again:");
    for line in tail {
        eprintln!("{line}");
    }
    ExitCode::from(EXIT_FEEDBACK)
}

/// True if the working tree differs from `HEAD` (tracked or untracked files).
fn has_changes(root: &Path) -> bool {
    let output = Command::new("git").args(["status", "--porcelain"]).current_dir(root).output();
    output.map_or(true, |output| !output.stdout.is_empty())
}

/// The nearest ancestor of `path` that holds `rust-toolchain.toml`, i.e. a nami checkout or
/// worktree.
fn workspace_root_of(path: &Path) -> Option<PathBuf> {
    let mut candidate = Some(path);
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
    use super::{json_bool_field, json_string_field};

    // Goal: the file path is extracted from a PostToolUse payload, with escapes decoded.
    #[test]
    fn string_fields_are_extracted() {
        let payload = concat!(
            r#"{"tool_name":"Edit","tool_input":"#,
            r#"{"file_path":"/a/b \"c\"\\d.rs","old_string":"x"}}"#
        );
        assert_eq!(json_string_field(payload, "file_path").as_deref(), Some("/a/b \"c\"\\d.rs"));
        assert_eq!(json_string_field(r#"{"cwd": "/w/\u00e9"}"#, "cwd").as_deref(), Some("/w/é"));
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

    // Goal: the workspace root is found from any file inside it, and nothing is found outside.
    #[test]
    fn workspace_root_is_found_from_a_file() {
        let root = crate::workspace_root();
        let file = root.join("crates/nami-core/src/lib.rs");
        assert_eq!(super::workspace_root_of(&file), Some(root));
        assert_eq!(super::workspace_root_of(std::path::Path::new("/")), None);
    }

    // Goal: malformed or unterminated strings yield None instead of a partial value.
    #[test]
    fn malformed_strings_are_rejected() {
        assert_eq!(json_string_field(r#"{"file_path":"/a/b"#, "file_path"), None);
        assert_eq!(json_string_field(r#"{"file_path":42}"#, "file_path"), None);
    }
}
