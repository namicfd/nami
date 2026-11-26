//! The session-start check that an agent session cannot reach a human's GitHub login (issue #69,
//! `docs/operations/github-setup.md`, step 6).
//!
//! `gh` looks for a token for github.com in this order: `GH_TOKEN`, `GITHUB_TOKEN`, the
//! `hosts.yml` in `GH_CONFIG_DIR`, and last the system keyring, where `gh auth login` keeps a
//! token for the host whether or not `hosts.yml` names a user. Agents name the bot's token in
//! each command, `GH_TOKEN=$(xtask/agent-token.sh) gh ...`, so every later source is a person's
//! login that a forgotten prefix, a token script that printed nothing, or text that bash ran as
//! commands would use. `.claude/settings.json` cuts those sources off: `GH_CONFIG_DIR` names a
//! directory that does not exist and cannot be created, and `DBUS_SESSION_BUS_ADDRESS` names no
//! bus, so that on Linux gh cannot reach the Secret Service. Nothing in the environment can hide
//! the macOS keychain.
//!
//! The check runs gh as a command without the prefix would, without `GH_TOKEN` and
//! `GITHUB_TOKEN`:
//!
//! - `gh auth status` must report no login;
//! - `gh auth token --no-refresh` must find no token. It also sees the keyring, which
//!   `gh auth status` does not list when `hosts.yml` names no user. Its stdout, where a token
//!   would be, goes nowhere.
//!
//! `GH_TOKEN` and `GITHUB_TOKEN`, if the session has them, must hold an app's installation token
//! (`ghs_`), not a person's. A login in reach stops the session ([`Severity::Reachable`]); a check
//! that cannot run, or that gh answers in a way the check does not know, is reported and the
//! session goes on ([`Severity::Unverified`]).

use std::process::{Command, Stdio};

/// The variables through which gh takes a token for github.com, in the order it reads them.
pub const TOKEN_VARIABLES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// The variable that names gh's configuration directory, and with it `hosts.yml`.
const CONFIG_VARIABLE: &str = "GH_CONFIG_DIR";

/// The variable through which gh, on Linux, finds the Secret Service that holds its keyring.
const BUS_VARIABLE: &str = "DBUS_SESSION_BUS_ADDRESS";

/// How every installation token of a GitHub App starts: the only kind an agent may hold.
const INSTALLATION_TOKEN_PREFIX: &str = "ghs_";

/// How the tokens of a person start, and what each kind is.
const PERSONAL_TOKEN_KINDS: [(&str, &str); 4] = [
    ("gho_", "an OAuth token, such as `gh auth login` stores"),
    ("ghp_", "a classic personal access token"),
    ("github_pat_", "a fine-grained personal access token"),
    ("ghu_", "an app's user token, which acts as the user"),
];

/// What `gh auth status` prints when it finds no login.
const NO_LOGIN: &str = "not logged into any";

/// What `gh auth token` prints when it finds no token.
const NO_TOKEN: &str = "no oauth token found";

/// The flag that keeps `gh auth token` from refreshing a token over the network, and what a gh
/// too old to know it prints. Such a gh refreshes nothing, so the check asks it without the flag.
const NO_REFRESH: &str = "--no-refresh";
const UNKNOWN_NO_REFRESH: &str = "unknown flag: --no-refresh";

/// The host whose login the check looks for.
const HOST: &str = "github.com";

/// Where the human finds what to do.
const RUNBOOK: &str = "docs/operations/github-setup.md, step 6";

/// What a finding means for the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A person's login is in reach of gh: the session stops.
    Reachable,
    /// The check could not tell: the human and the agent are told, and the session goes on.
    Unverified,
}

/// One problem with the session's GitHub login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Whether the session stops.
    pub severity: Severity,
    /// What is wrong and how to fix it, as one line that names no token.
    pub message: String,
}

/// What gh did when the check ran it.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    /// It exited 0, and printed this: stderr, after stdout if stdout was kept.
    Success(String),
    /// It exited with another code, and printed this.
    Failure(String),
    /// It could not start, or a signal ended it.
    CouldNotRun(String),
}

/// The findings for this session. `gh` makes a command that runs gh, and `variable` reads one
/// variable of the session's environment.
#[must_use]
pub fn findings(
    gh: &dyn Fn() -> Command,
    variable: &dyn Fn(&str) -> Option<String>,
) -> Vec<Finding> {
    let mut findings: Vec<Finding> = TOKEN_VARIABLES
        .iter()
        .filter_map(|name| variable(name).and_then(|value| variable_finding(name, &value)))
        .collect();
    let config = variable(CONFIG_VARIABLE);
    let status = ask(gh, &["auth", "status", "--hostname", HOST], true);
    let stored = status_finding(&status, config.as_deref());
    let stored_login =
        stored.as_ref().is_some_and(|finding| finding.severity == Severity::Reachable);
    findings.extend(stored);
    // A stored login also has a token; the second finding would say the same again.
    if !stored_login {
        let mut token = ask(gh, &["auth", "token", NO_REFRESH, "--hostname", HOST], false);
        if matches!(&token, Answer::Failure(printed) if printed.contains(UNKNOWN_NO_REFRESH)) {
            token = ask(gh, &["auth", "token", "--hostname", HOST], false);
        }
        findings.extend(token_finding(&token, variable(BUS_VARIABLE).as_deref()));
    }
    assert!(findings.len() <= TOKEN_VARIABLES.len() + 1, "one finding per source at most");
    assert!(findings.iter().all(|finding| !finding.message.contains('\n')), "one line each");
    findings
}

/// The finding for the token in `variable`, if it is not an installation token. The message
/// names the kind of token, never the token.
fn variable_finding(variable: &str, value: &str) -> Option<Finding> {
    assert!(TOKEN_VARIABLES.contains(&variable), "only gh's token variables are checked");
    if value.is_empty() || value.starts_with(INSTALLATION_TOKEN_PREFIX) {
        return None;
    }
    let kind = PERSONAL_TOKEN_KINDS
        .iter()
        .find(|(prefix, _kind)| value.starts_with(prefix))
        .map_or("a token that is not an app's installation token", |(_prefix, kind)| kind);
    let message = format!(
        "error[gh-login]: {variable} holds {kind}, so gh in this session acts as a person, not \
         as nami-agent[bot]; unset it before the session starts, and let each command name the \
         bot's token: `GH_TOKEN=$(xtask/agent-token.sh) gh ...` ({RUNBOOK})"
    );
    assert!(!message.contains(value), "a finding never shows the token");
    Some(Finding { severity: Severity::Reachable, message })
}

/// The finding for what `gh auth status` answered without a token in the environment. `config`
/// is the session's `GH_CONFIG_DIR`.
fn status_finding(answer: &Answer, config: Option<&str>) -> Option<Finding> {
    let config = config.map_or_else(|| "unset".to_owned(), |directory| format!("`{directory}`"));
    let (severity, message) = match answer {
        Answer::Failure(printed) if printed.contains(NO_LOGIN) => return None,
        Answer::Success(printed) => {
            let accounts = accounts(printed);
            let message = format!(
                "error[gh-login]: `gh auth status` without GH_TOKEN reports a stored login \
                 ({accounts}), so a gh command without `GH_TOKEN=$(xtask/agent-token.sh)` acts \
                 as that account; gh reads it from GH_CONFIG_DIR ({config}), which \
                 `.claude/settings.json` sets to a directory that holds no login: start agents \
                 with that file's `env` block ({RUNBOOK})"
            );
            (Severity::Reachable, message)
        }
        Answer::Failure(printed) | Answer::CouldNotRun(printed) => {
            let first = printed.lines().map(str::trim).find(|line| !line.is_empty());
            let message = format!(
                "warning[gh-login]: `gh auth status` without GH_TOKEN answered in a way this \
                 check does not know ({}), so a stored gh login is not ruled out; run it by hand \
                 and check that it reports no login ({RUNBOOK})",
                first.unwrap_or("no output")
            );
            (Severity::Unverified, message)
        }
    };
    assert!(message.contains(RUNBOOK), "a finding says where to read on");
    Some(Finding { severity, message })
}

/// The accounts that the output of `gh auth status` names, from its `Logged in to` lines. The
/// lines with a (masked) token are left out.
fn accounts(printed: &str) -> String {
    const LOGGED_IN: &str = "Logged in to ";
    let accounts: Vec<&str> = printed
        .lines()
        .filter_map(|line| line.find(LOGGED_IN).map(|start| line.get(start..).unwrap_or(line)))
        .map(str::trim)
        .collect();
    let joined =
        if accounts.is_empty() { "no account named".to_owned() } else { accounts.join("; ") };
    assert!(!joined.is_empty(), "the accounts are named, or their absence is");
    assert!(!joined.contains('\n'), "the accounts fit on the finding's line");
    joined
}

/// The finding for what `gh auth token` answered without a token in the environment. `bus` is
/// the session's `DBUS_SESSION_BUS_ADDRESS`.
fn token_finding(answer: &Answer, bus: Option<&str>) -> Option<Finding> {
    let bus = bus.map_or_else(|| "unset".to_owned(), |address| format!("`{address}`"));
    let (severity, message) = match answer {
        Answer::Failure(printed) if printed.contains(NO_TOKEN) => return None,
        Answer::Success(_) => {
            let message = format!(
                "error[gh-login]: gh finds a token for {HOST} without GH_TOKEN and without a \
                 login in GH_CONFIG_DIR: the one that `gh auth login` keeps in the system \
                 keyring, so a gh command without `GH_TOKEN=$(xtask/agent-token.sh)` acts as its \
                 owner; on Linux, `.claude/settings.json` cuts gh off from the keyring with \
                 DBUS_SESSION_BUS_ADDRESS (now {bus}); on macOS nothing in the environment can: \
                 run `gh auth logout`, then `gh auth login --insecure-storage`, so that the \
                 keychain holds no gh token ({RUNBOOK})"
            );
            (Severity::Reachable, message)
        }
        Answer::Failure(printed) | Answer::CouldNotRun(printed) => {
            let first = printed.lines().map(str::trim).find(|line| !line.is_empty());
            let message = format!(
                "warning[gh-login]: `gh auth token` without GH_TOKEN answered in a way this check \
                 does not know ({}), so a token in the system keyring is not ruled out; check by \
                 hand that `gh api user` fails in this session ({RUNBOOK})",
                first.unwrap_or("no output")
            );
            (Severity::Unverified, message)
        }
    };
    assert!(message.contains(RUNBOOK), "a finding says where to read on");
    Some(Finding { severity, message })
}

/// Runs gh with `arguments`, as a command without the bot's token would: without
/// [`TOKEN_VARIABLES`], without a prompt and with nothing on stdin. Its stdout is read only if
/// `keep_stdout`; otherwise it goes nowhere, so that a token gh prints never reaches this process.
fn ask(gh: &dyn Fn() -> Command, arguments: &[&str], keep_stdout: bool) -> Answer {
    assert!(arguments.first() == Some(&"auth"), "the check asks gh only about its login");
    let mut command = gh();
    let stdout = if keep_stdout { Stdio::piped() } else { Stdio::null() };
    let command = TOKEN_VARIABLES
        .iter()
        .fold(&mut command, |command, variable| command.env_remove(variable))
        .args(arguments)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::piped());
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => return Answer::CouldNotRun(format!("cannot start gh: {error}")),
    };
    assert!(keep_stdout || output.stdout.is_empty(), "a discarded stdout is never read");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match output.status.code() {
        Some(0) => Answer::Success(printed),
        Some(_) => Answer::Failure(printed),
        None => Answer::CouldNotRun(format!("gh was ended by {}", output.status)),
    }
}

#[cfg(test)]
mod tests {
    use super::{Answer, Finding, Severity, ask, findings, status_finding, token_finding};
    use super::{accounts, variable_finding};
    use crate::tidy::scratch::Scratch;
    use std::process::Command;

    /// What gh prints when no login is stored.
    const STATUS_NONE: &str = "You are not logged into any GitHub hosts. To log in, run: gh \
                               auth login\n";

    /// What gh prints for a stored login, with the token masked as gh masks it.
    const STATUS_LOGGED_IN: &str = "github.com\n  ✓ Logged in to github.com account human \
                                    (/home/human/.config/gh/hosts.yml)\n  - Active account: \
                                    true\n  - Token: gho_************************************\n";

    /// A fake gh in `scratch`, run as `sh <script>` so that no test executes a file that it has
    /// just written (issue #67). `status` and `token` are shell code for `gh auth status` and
    /// `gh auth token`.
    fn fake_gh(
        scratch: &Scratch,
        name: &str,
        status: &str,
        token: &str,
    ) -> impl Fn() -> Command + use<> {
        let script = format!(
            "case \"$1 $2\" in\n  'auth status') {status} ;;\n  'auth token') {token} ;;\n  \
             *) echo \"unexpected: $*\" >&2; exit 64 ;;\nesac\n"
        );
        let file = format!("{name}.sh");
        scratch.write(&file, &script);
        let path = scratch.root().join(file);
        move || {
            let mut command = Command::new("sh");
            let _command = command.arg(&path);
            command
        }
    }

    /// A shell command that prints `text` on stderr and exits with `code`.
    fn says(text: &str, code: u8) -> String {
        format!("printf '%s' '{text}' >&2; exit {code}")
    }

    fn no_variables(_name: &str) -> Option<String> {
        None
    }

    fn severities(found: &[Finding]) -> Vec<Severity> {
        found.iter().map(|finding| finding.severity).collect()
    }

    /// The message of the one finding for a session, which must be a stop.
    fn the_stop(gh: &dyn Fn() -> Command, variable: &dyn Fn(&str) -> Option<String>) -> String {
        let found = findings(gh, variable);
        assert_eq!(severities(&found), [Severity::Reachable], "one stop: {found:?}");
        found.into_iter().map(|finding| finding.message).collect()
    }

    // Goal: only an installation token passes; every personal token is a stop that names its
    // kind and never the token itself.
    #[test]
    fn token_variables_must_hold_an_installation_token() {
        assert_eq!(variable_finding("GH_TOKEN", "ghs_bot"), None);
        assert_eq!(variable_finding("GITHUB_TOKEN", ""), None, "an empty variable is unset");
        let cases = [
            ("gho_secret", "an OAuth token"),
            ("ghp_secret", "a classic personal access token"),
            ("github_pat_secret", "a fine-grained personal access token"),
            ("ghu_secret", "an app's user token"),
            ("0123456789abcdef", "not an app's installation token"),
        ];
        for (token, kind) in cases {
            let finding = variable_finding("GH_TOKEN", token).expect("a personal token is found");
            assert_eq!(finding.severity, Severity::Reachable, "{token}");
            assert!(finding.message.contains(kind), "{token}: {}", finding.message);
            assert!(!finding.message.contains("secret"), "never the token: {}", finding.message);
            assert!(finding.message.starts_with("error[gh-login]: GH_TOKEN holds "), "{token}");
        }
    }

    // Goal: `gh auth status` passes only when it reports no login; a stored login stops the
    // session and names the account, not the masked token; any other answer is unverified.
    #[test]
    fn auth_status_answers_are_classified() {
        let none = Answer::Failure(STATUS_NONE.to_owned());
        assert_eq!(status_finding(&none, Some("/nonexistent/x")), None);
        let stored = status_finding(&Answer::Success(STATUS_LOGGED_IN.to_owned()), None);
        let stored = stored.expect("a stored login is found");
        assert_eq!(stored.severity, Severity::Reachable);
        assert!(stored.message.contains("Logged in to github.com account human"), "{stored:?}");
        assert!(stored.message.contains("GH_CONFIG_DIR (unset)"), "{stored:?}");
        assert!(!stored.message.contains("gho_"), "not even the masked token: {stored:?}");
        let odd = Answer::Failure("\n  HTTP 502\nmore\n".to_owned());
        let odd = status_finding(&odd, Some("/x")).expect("an unknown answer is reported");
        assert_eq!(odd.severity, Severity::Unverified);
        assert!(odd.message.contains("(HTTP 502)"), "its first line: {odd:?}");
        let missing = Answer::CouldNotRun(String::new());
        let missing = status_finding(&missing, None).expect("a gh that cannot run is reported");
        assert_eq!(missing.severity, Severity::Unverified);
        assert!(missing.message.contains("(no output)"), "{missing:?}");
    }

    // Goal: the accounts come from the `Logged in to` lines alone.
    #[test]
    fn accounts_are_read_from_the_status() {
        assert_eq!(
            accounts(STATUS_LOGGED_IN),
            "Logged in to github.com account human (/home/human/.config/gh/hosts.yml)"
        );
        let two = "✓ Logged in to github.com account a (x)\n✓ Logged in to github.com account b\n";
        assert_eq!(
            accounts(two),
            "Logged in to github.com account a (x); Logged in to github.com account b"
        );
        assert_eq!(accounts("github.com\n"), "no account named");
    }

    // Goal: `gh auth token` passes only when it finds no token; a token stops the session and
    // says what the bus variable is; any other answer is unverified.
    #[test]
    fn auth_token_answers_are_classified() {
        let none = Answer::Failure("no oauth token found for github.com\n".to_owned());
        assert_eq!(token_finding(&none, Some("disabled:")), None);
        let found = token_finding(&Answer::Success(String::new()), Some("disabled:"));
        let found = found.expect("a token in reach is found");
        assert_eq!(found.severity, Severity::Reachable);
        assert!(found.message.contains("DBUS_SESSION_BUS_ADDRESS (now `disabled:`)"), "{found:?}");
        let unset = token_finding(&Answer::Success(String::new()), None).expect("found");
        assert!(unset.message.contains("(now unset)"), "{unset:?}");
        let odd = Answer::Failure("keyring timeout\n".to_owned());
        let odd = token_finding(&odd, None).expect("an unknown answer is reported");
        assert_eq!(odd.severity, Severity::Unverified);
        assert!(odd.message.contains("(keyring timeout)"), "{odd:?}");
        let missing = token_finding(&Answer::CouldNotRun("cannot start gh".to_owned()), None);
        assert_eq!(missing.map(|finding| finding.severity), Some(Severity::Unverified));
    }

    // Goal: gh is asked without the token variables and without a prompt, a discarded stdout
    // never reaches this process, and exit codes, signals and a missing gh are told apart.
    #[test]
    fn gh_is_asked_without_a_token() {
        let scratch = Scratch::new("gh-login-ask");
        let environment = "printf '%s|%s|%s' \"${GH_TOKEN-none}\" \"${GITHUB_TOKEN-none}\" \
                           \"$GH_PROMPT_DISABLED\"; echo ' err' >&2";
        let gh = fake_gh(
            &scratch,
            "environment",
            &format!("{environment}; exit 0"),
            "echo gho_secret; exit 0",
        );
        let leaky = || {
            let mut command = gh();
            let _command = command.env("GH_TOKEN", "gho_a").env("GITHUB_TOKEN", "gho_b");
            command
        };
        let status = ask(&leaky, &["auth", "status"], true);
        assert_eq!(status, Answer::Success("none|none|1 err\n".to_owned()), "no token, no prompt");
        let token = ask(&leaky, &["auth", "token"], false);
        assert_eq!(token, Answer::Success(String::new()), "the token went nowhere");
        let failing = fake_gh(&scratch, "failing", &says("bad", 3), "kill -TERM $$");
        assert_eq!(ask(&failing, &["auth", "status"], true), Answer::Failure("bad".to_owned()));
        let killed = ask(&failing, &["auth", "token"], false);
        let Answer::CouldNotRun(killed) = killed else { panic!("it exited: {killed:?}") };
        assert!(killed.starts_with("gh was ended by signal: 15"), "{killed}");
        let missing = || Command::new("/nonexistent/gh");
        let missing = ask(&missing, &["auth", "status"], true);
        let Answer::CouldNotRun(missing) = missing else { panic!("it ran: {missing:?}") };
        assert!(missing.starts_with("cannot start gh: "), "{missing}");
    }

    // Goal: a session whose gh finds neither a login nor a token has no finding; a stored login
    // is one stop, and its token is not reported again; a token in the keyring alone is a stop.
    #[test]
    fn findings_cover_the_login_sources() {
        let scratch = Scratch::new("gh-login-sources");
        let no_token = says("no oauth token found for github.com", 1);
        let clean = fake_gh(&scratch, "clean", &says(STATUS_NONE, 1), &no_token);
        assert_eq!(findings(&clean, &no_variables), []);
        let stored = fake_gh(&scratch, "stored", &format!("echo '{STATUS_LOGGED_IN}'"), "exit 0");
        let login = the_stop(&stored, &no_variables);
        assert!(login.contains("stored login"), "{login}");
        let keyring = fake_gh(&scratch, "keyring", &says(STATUS_NONE, 1), "exit 0");
        let kept = the_stop(&keyring, &no_variables);
        assert!(kept.contains("system keyring"), "{kept}");
        let variable = |name: &str| (name == "GITHUB_TOKEN").then(|| "ghp_secret".to_owned());
        let exported = the_stop(&clean, &variable);
        assert!(exported.contains("GITHUB_TOKEN holds"), "{exported}");
    }

    // Goal: a gh too old to know `--no-refresh` is asked again without it, and its answer
    // counts, whether it finds a token or not.
    #[test]
    fn an_old_gh_is_asked_without_the_refresh_flag() {
        let scratch = Scratch::new("gh-login-old");
        let old = |without_flag: &str| {
            format!(
                "case \"$*\" in *--no-refresh*) {} ;; *) {without_flag} ;; esac",
                says("unknown flag: --no-refresh", 1)
            )
        };
        let none = fake_gh(
            &scratch,
            "none",
            &says(STATUS_NONE, 1),
            &old(&says("no oauth token found", 1)),
        );
        assert_eq!(findings(&none, &no_variables), []);
        let token = fake_gh(&scratch, "token", &says(STATUS_NONE, 1), &old("exit 0"));
        assert_eq!(severities(&findings(&token, &no_variables)), [Severity::Reachable]);
    }

    // Goal: the session environment that this check guards is still there: `GH_CONFIG_DIR` is
    // an absolute path that does not exist, so gh reads no login from it on any machine and in
    // any worktree, and `DBUS_SESSION_BUS_ADDRESS` names no bus, so gh reaches no keyring on
    // Linux. Method: read the real `.claude/settings.json`.
    #[test]
    fn the_settings_hide_a_person_s_login() {
        let file = crate::workspace_root().join(".claude/settings.json");
        let text = std::fs::read_to_string(file).expect("the settings are readable");
        let settings = crate::json::parse(&text).expect("the settings are JSON");
        let variable = |name: &str| {
            let value = settings.get("env").and_then(|env| env.get(name));
            value.and_then(crate::json::Value::as_str).map(str::to_owned)
        };
        let config = variable(super::CONFIG_VARIABLE).expect("the settings set GH_CONFIG_DIR");
        let config = std::path::Path::new(&config);
        assert!(config.is_absolute(), "the same in every directory: {}", config.display());
        assert!(!config.exists(), "it holds no login: {}", config.display());
        let parent = config.parent().expect("an absolute path below the root has a parent");
        assert!(!parent.exists(), "and nothing below `/` can create it: {}", parent.display());
        assert_eq!(variable(super::BUS_VARIABLE).as_deref(), Some("disabled:"), "no bus");
        for token in super::TOKEN_VARIABLES {
            assert_eq!(variable(token), None, "the settings name no token: {token}");
        }
    }
}
