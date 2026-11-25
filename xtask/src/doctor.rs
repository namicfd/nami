//! `cargo xtask doctor`: checks that the installed tools are the versions that CI pins.
//!
//! The versions are pinned in one place: the `cargo install --locked name@version` lines of
//! `.github/actions/setup/action.yml`. The doctor reads them from there, so there is no second
//! list to keep in step. It also checks that the nightly of the dylint library
//! (`lints/rust-toolchain.toml`) is installed.
//!
//! Every finding reads `error[doctor]: what; fix`, like a tidy violation.

use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

/// The file that pins the tool versions, relative to the workspace root.
const ACTION_FILE: &str = ".github/actions/setup/action.yml";

/// The file that names the dylint library's nightly, relative to the workspace root.
const LINTS_TOOLCHAIN_FILE: &str = "lints/rust-toolchain.toml";

/// How a line that installs pinned tools starts.
const INSTALL_COMMAND: &str = "cargo install";

/// The only flag the doctor expects on an install line. Any other flag may take a value or
/// change what is installed, so it is an error until the doctor is taught about it.
const INSTALL_FLAG: &str = "--locked";

/// Tools without a `--version` flag: `dylint-link` hands every argument to the linker. Their
/// version comes from `cargo install --list` instead.
const TOOLS_WITHOUT_VERSION_FLAG: [&str; 1] = ["dylint-link"];

/// One tool and the version of it that CI installs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// The crate name, which is also the name of the binary.
    pub name: String,
    /// The pinned version, such as `1.59.0`.
    pub version: String,
}

/// Runs the doctor on this workspace and prints one line per finding.
pub fn run() -> ExitCode {
    let findings = findings(&crate::workspace_root());
    for finding in &findings {
        eprintln!("{finding}");
    }
    if findings.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// Compares the installed tools and toolchains with the pins of the checkout at `root`.
/// Returns one line per problem, and nothing if everything matches.
#[must_use]
pub fn findings(root: &Path) -> Vec<String> {
    let mut findings = tool_findings(root);
    let channel = fs::read_to_string(root.join(LINTS_TOOLCHAIN_FILE))
        .ok()
        .and_then(|toolchain_file| toolchain_channel(&toolchain_file));
    let toolchains = command_output(root, &["rustup", "toolchain", "list"]);
    findings.extend(toolchain_finding(channel.as_deref(), toolchains.as_deref()));
    findings
}

fn tool_findings(root: &Path) -> Vec<String> {
    let pins = fs::read_to_string(root.join(ACTION_FILE))
        .map_err(|error| error.to_string())
        .and_then(|action| pinned_tools(&action));
    match pins {
        Ok(pins) => pins
            .iter()
            .filter_map(|pin| {
                let output = command_output(root, &version_command(&pin.name));
                let installed = output.and_then(|output| reported_version(&output, &pin.name));
                tool_finding(pin, installed.as_deref())
            })
            .collect(),
        Err(error) => vec![format!(
            "error[doctor]: cannot read the pinned tools from {ACTION_FILE}: {error}; keep its \
             `{INSTALL_COMMAND} {INSTALL_FLAG} name@version` lines, or teach \
             xtask/src/doctor.rs the new format"
        )],
    }
}

/// Runs `command` in `root` and returns its stdout, or `None` if it cannot run or fails.
fn command_output(root: &Path, command: &[&str]) -> Option<String> {
    let (program, arguments) = command.split_first()?;
    let output = Command::new(program)
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    output.status.success().then_some(text)
}

/// Reads the `name@version` pairs from the `cargo install` lines of the setup action.
///
/// # Errors
///
/// Returns what is wrong if an argument is not `name@version`, or if no tool is pinned at all.
/// Either means that the file's format changed, and the doctor must change with it.
pub fn pinned_tools(action: &str) -> Result<Vec<Pin>, String> {
    let mut pins = Vec::new();
    for line in action.lines() {
        let Some(arguments) = line.trim().strip_prefix(INSTALL_COMMAND) else {
            continue;
        };
        for argument in arguments.split_whitespace().filter(|argument| *argument != INSTALL_FLAG) {
            pins.push(pin_of(argument)?);
        }
    }
    if pins.is_empty() {
        return Err(format!("no `{INSTALL_COMMAND}` line pins a tool"));
    }
    Ok(pins)
}

fn pin_of(argument: &str) -> Result<Pin, String> {
    match argument.split_once('@') {
        Some((name, version)) if is_tool_name(name) && is_version(version) => {
            Ok(Pin { name: name.to_owned(), version: version.to_owned() })
        }
        _ => Err(format!("`{argument}` is not `name@version`")),
    }
}

fn is_tool_name(name: &str) -> bool {
    let is_name_character =
        |character: char| character.is_ascii_alphanumeric() || character == '-' || character == '_';
    !name.is_empty() && name.chars().all(is_name_character)
}

fn is_version(version: &str) -> bool {
    let is_version_character = |character: char| {
        character.is_ascii_alphanumeric()
            || character == '.'
            || character == '-'
            || character == '+'
    };
    version.starts_with(|character: char| character.is_ascii_digit())
        && version.chars().all(is_version_character)
}

/// The command whose output names the installed version of `tool`. Cargo subcommands are asked
/// through `cargo`, as they are used.
fn version_command(tool: &str) -> Vec<&str> {
    if TOOLS_WITHOUT_VERSION_FLAG.contains(&tool) {
        vec!["cargo", "install", "--list"]
    } else if let Some(subcommand) = tool.strip_prefix("cargo-") {
        vec!["cargo", subcommand, "--version"]
    } else {
        vec![tool, "--version"]
    }
}

/// The version of `tool` in `output`: the word after the tool's name at the start of a line.
///
/// This reads both `just 1.59.0` (a `--version` output) and `dylint-link v6.4.0:` (a line of
/// `cargo install --list`).
#[must_use]
pub fn reported_version(output: &str, tool: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let version = words.next().filter(|name| *name == tool).and_then(|_| words.next())?;
        let version = version.strip_prefix('v').unwrap_or(version);
        Some(version.strip_suffix(':').unwrap_or(version).to_owned())
    })
}

/// The finding for one tool, or `None` if the installed version is the pinned one.
#[must_use]
pub fn tool_finding(pin: &Pin, installed: Option<&str>) -> Option<String> {
    let Pin { name, version } = pin;
    let state = match installed {
        Some(installed) if installed == version => return None,
        Some(installed) => format!("{name} {installed} is installed"),
        None => format!("{name} is not installed"),
    };
    Some(format!(
        "error[doctor]: {state}, {version} is pinned; run `{INSTALL_COMMAND} {INSTALL_FLAG} \
         {name}@{version}`"
    ))
}

/// The `channel` value of a `rust-toolchain.toml`.
#[must_use]
pub fn toolchain_channel(toolchain_file: &str) -> Option<String> {
    toolchain_file.lines().find_map(|line| {
        let value = line.trim().strip_prefix("channel")?.trim_start().strip_prefix('=')?;
        let (channel, _rest) = value.trim_start().strip_prefix('"')?.split_once('"')?;
        (!channel.is_empty()).then(|| channel.to_owned())
    })
}

/// The finding for the dylint library's nightly, or `None` if it is installed.
///
/// `channel` is the pinned toolchain, if the file names one. `toolchains` is the output of
/// `rustup toolchain list`, if rustup runs.
#[must_use]
pub fn toolchain_finding(channel: Option<&str>, toolchains: Option<&str>) -> Option<String> {
    let install = "run `(cd lints && rustup toolchain install)`";
    let Some(channel) = channel else {
        return Some(format!(
            "error[doctor]: {LINTS_TOOLCHAIN_FILE} names no toolchain; restore its `channel = \
             \"nightly-...\"` line"
        ));
    };
    let Some(toolchains) = toolchains else {
        return Some(format!(
            "error[doctor]: cannot run `rustup toolchain list`, so toolchain {channel} is \
             unchecked; install rustup, then {install}"
        ));
    };
    if toolchains.lines().any(|line| names_toolchain(line, channel)) {
        return None;
    }
    Some(format!(
        "error[doctor]: toolchain {channel} is not installed, {LINTS_TOOLCHAIN_FILE} pins it; \
         {install}"
    ))
}

/// True if a line of `rustup toolchain list` names the toolchain `channel`. rustup prints
/// `nightly-2026-08-20-aarch64-apple-darwin (active)`: the channel, the host, then notes.
fn names_toolchain(line: &str, channel: &str) -> bool {
    let name = line.split_whitespace().next().unwrap_or("");
    name.strip_prefix(channel).is_some_and(|host| host.is_empty() || host.starts_with('-'))
}

#[cfg(test)]
mod tests {
    use super::{Pin, pinned_tools, reported_version, tool_finding, toolchain_channel};
    use super::{toolchain_finding, version_command};

    fn pin(name: &str, version: &str) -> Pin {
        Pin { name: name.to_owned(), version: version.to_owned() }
    }

    // Goal: the real setup action still has the format the doctor reads. Method: parse the file
    // in the repository and look for every tool that `just check-full` and CI need.
    #[test]
    fn real_action_file_pins_the_tools() {
        let action = std::fs::read_to_string(crate::workspace_root().join(super::ACTION_FILE))
            .expect("the setup action is readable");
        let pins = pinned_tools(&action).expect("the setup action pins its tools");
        for tool in ["just", "cargo-nextest", "cargo-deny", "cargo-mutants", "cargo-dylint"] {
            let found = pins.iter().filter(|pin| pin.name == tool).count();
            assert_eq!(found, 1, "{tool} is pinned exactly once in {pins:?}");
        }
        assert!(pins.iter().all(|pin| super::is_version(&pin.version)), "{pins:?}");
    }

    // Goal: install lines are read in order, one line may pin two tools, and other lines of the
    // file are ignored.
    #[test]
    fn pins_are_read_from_install_lines() {
        let action = "    run: |\n        cargo install --locked just@1.59.0\n        \
                      cargo install --locked cargo-dylint@6.4.0 dylint-link@6.4.0\n    \
                      key: tools-just1.59.0\n";
        assert_eq!(
            pinned_tools(action),
            Ok(vec![
                pin("just", "1.59.0"),
                pin("cargo-dylint", "6.4.0"),
                pin("dylint-link", "6.4.0")
            ])
        );
    }

    // Goal: names and versions may use every character that crate names and semantic versions
    // allow, such as a pre-release or build suffix.
    #[test]
    fn pins_accept_full_names_and_versions() {
        let action = "cargo install --locked a_b-c9@1.0.0-rc.1 tool@2.3.4+build.5 x@10\n";
        assert_eq!(
            pinned_tools(action),
            Ok(vec![pin("a_b-c9", "1.0.0-rc.1"), pin("tool", "2.3.4+build.5"), pin("x", "10")])
        );
    }

    // Goal: a format the doctor does not understand is an error, never an empty or partial list:
    // an unpinned tool, an unknown flag, a malformed pair, or no install line at all.
    #[test]
    fn unreadable_install_lines_are_errors() {
        assert_eq!(
            pinned_tools("cargo install --locked just"),
            Err("`just` is not `name@version`".into())
        );
        assert!(pinned_tools("cargo install --git https://x just@1.0.0").is_err());
        assert!(pinned_tools("cargo install --locked just@").is_err());
        assert!(pinned_tools("cargo install --locked @1.0.0").is_err());
        assert!(pinned_tools("cargo install --locked just@latest").is_err());
        assert!(pinned_tools("cargo install --locked ju/st@1.0.0").is_err());
        assert!(pinned_tools("cargo install --locked just@1.0.0;x").is_err());
        assert!(pinned_tools("cargo install --locked just@v1.0.0").is_err());
        assert_eq!(
            pinned_tools("run: rustup toolchain install\n"),
            Err("no `cargo install` line pins a tool".into())
        );
    }

    // Goal: the version is the word after the tool's name, in each format the tools print.
    #[test]
    fn versions_are_read_from_tool_output() {
        assert_eq!(reported_version("just 1.59.0\n", "just").as_deref(), Some("1.59.0"));
        let nextest = "cargo-nextest 0.9.152 (26ebeda79 2026-11-13)\nrelease: 0.9.152\n";
        assert_eq!(reported_version(nextest, "cargo-nextest").as_deref(), Some("0.9.152"));
        let list = "cargo-dylint v6.4.0:\n    cargo-dylint\ndylint-link v6.4.0:\n    dylint-link\n";
        assert_eq!(reported_version(list, "dylint-link").as_deref(), Some("6.4.0"));
        assert_eq!(reported_version(list, "just"), None);
        assert_eq!(reported_version("just\n", "just"), None);
        assert_eq!(reported_version("error: no such command: `nextest`\n", "cargo-nextest"), None);
    }

    // Goal: a command's output comes back only if the command runs and succeeds. Method: git,
    // which every checkout has, with a subcommand that works and one that does not.
    #[test]
    fn command_output_needs_success() {
        let root = crate::workspace_root();
        let version = super::command_output(&root, &["git", "--version"]);
        assert!(version.is_some_and(|version| version.starts_with("git version ")), "git runs");
        assert_eq!(super::command_output(&root, &["git", "no-such-subcommand"]), None);
        assert_eq!(super::command_output(&root, &["nami-no-such-program"]), None);
        assert_eq!(super::command_output(&root, &[]), None);
    }

    // Goal: each kind of tool is asked for its version in the way that works for it.
    #[test]
    fn version_commands_follow_the_tool_kind() {
        assert_eq!(version_command("just"), ["just", "--version"]);
        assert_eq!(version_command("cargo-nextest"), ["cargo", "nextest", "--version"]);
        assert_eq!(version_command("dylint-link"), ["cargo", "install", "--list"]);
    }

    // Goal: a finding names the installed version, the pinned one and the command that fixes it;
    // a matching version gives no finding.
    #[test]
    fn tool_findings_say_how_to_fix() {
        let just = pin("just", "1.59.0");
        assert_eq!(tool_finding(&just, Some("1.59.0")), None);
        assert_eq!(
            tool_finding(&just, Some("1.58.0")).as_deref(),
            Some(
                "error[doctor]: just 1.58.0 is installed, 1.59.0 is pinned; run `cargo install \
                 --locked just@1.59.0`"
            )
        );
        assert_eq!(
            tool_finding(&just, None).as_deref(),
            Some(
                "error[doctor]: just is not installed, 1.59.0 is pinned; run `cargo install \
                 --locked just@1.59.0`"
            )
        );
    }

    // Goal: the channel is read from the real `lints/rust-toolchain.toml` and from fixtures, and
    // a file without one yields nothing.
    #[test]
    fn toolchain_channel_is_read() {
        let real =
            std::fs::read_to_string(crate::workspace_root().join(super::LINTS_TOOLCHAIN_FILE))
                .expect("the lints toolchain file is readable");
        let channel = toolchain_channel(&real).expect("the lints toolchain file names a channel");
        assert!(channel.starts_with("nightly-"), "the dylint library needs a nightly: {channel}");
        let fixture =
            "[toolchain]\nchannel = \"nightly-2026-08-20\"\ncomponents = [\"rustc-dev\"]\n";
        assert_eq!(toolchain_channel(fixture).as_deref(), Some("nightly-2026-08-20"));
        assert_eq!(toolchain_channel("channel=\"1.100.0\"").as_deref(), Some("1.100.0"));
        assert_eq!(toolchain_channel("[toolchain]\ncomponents = [\"channel\"]\n"), None);
        assert_eq!(toolchain_channel("channel = \"\"\n"), None);
        assert_eq!(toolchain_channel("channel = \"nightly\n"), None);
    }

    // Goal: the nightly counts as installed only if rustup lists exactly that channel, with or
    // without a host suffix; every other case is a finding that says what to run.
    #[test]
    fn toolchain_findings_cover_every_case() {
        let channel = Some("nightly-2026-08-20");
        let installed =
            "stable-aarch64-apple-darwin (default)\nnightly-2026-08-20-aarch64-apple-darwin\n";
        assert_eq!(toolchain_finding(channel, Some(installed)), None);
        assert_eq!(toolchain_finding(channel, Some("nightly-2026-08-20 (active)\n")), None);
        let other = "stable-aarch64-apple-darwin\nnightly-2026-08-21-aarch64-apple-darwin\n";
        assert_eq!(
            toolchain_finding(channel, Some(other)).as_deref(),
            Some(
                "error[doctor]: toolchain nightly-2026-08-20 is not installed, \
                 lints/rust-toolchain.toml pins it; run `(cd lints && rustup toolchain install)`"
            )
        );
        let longer = "nightly-2026-08-201-aarch64-apple-darwin\n";
        assert!(toolchain_finding(channel, Some(longer)).is_some(), "a longer name is another");
        let no_rustup =
            toolchain_finding(channel, None).expect("an unchecked toolchain is a finding");
        assert!(no_rustup.contains("cannot run `rustup toolchain list`"), "{no_rustup}");
        let no_channel = toolchain_finding(None, Some(installed)).expect("no channel is a finding");
        assert!(no_channel.contains("names no toolchain"), "{no_channel}");
    }

    // Goal: the doctor runs end to end on this workspace without panicking, and whatever it
    // finds is in the documented form. The tools' versions themselves are not asserted: they
    // belong to the machine, not to the code.
    #[test]
    fn findings_are_well_formed_on_this_workspace() {
        for finding in super::findings(&crate::workspace_root()) {
            assert!(finding.starts_with("error[doctor]: "), "{finding}");
            assert!(finding.contains("; "), "a finding says how to fix it: {finding}");
        }
    }
}
