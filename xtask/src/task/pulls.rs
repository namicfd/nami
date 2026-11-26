//! GitHub's pull requests for `passes` and `check-pr` (ADR-0017), through `gh`, with the two
//! tokens kept apart (ADR-0023, rule 2).
//!
//! - **Reads** (a PR's head, whether a branch's PR merged, the open PR from a branch) act as
//!   `GH_TOKEN`, in CI the workflow's own read-only token, and gh never sees [`WRITE_TOKEN`].
//! - **Writes** (opening, editing and closing the PR, and turning on auto-merge) act as
//!   [`WRITE_TOKEN`], in CI the `nami-ci` app's token, and nothing else does.
//!
//! What GitHub's answers mean is decided by plain functions of its JSON, tested without gh.
//! [`GhProcess`] only runs the program.

use super::passes::{PullHead, PullState, Pulls};
use crate::issue::github::{optional_string, parse_lines, string_at};
use crate::json::{self, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The environment variable that holds the token for writes.
pub const WRITE_TOKEN: &str = "NAMI_PASSES_TOKEN";

/// The branch that tasks merge into.
const BASE_BRANCH: &str = "main";

/// Which token a call acts as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// `GH_TOKEN`, without [`WRITE_TOKEN`] in gh's environment.
    Read,
    /// [`WRITE_TOKEN`], passed to gh as its `GH_TOKEN`.
    Write,
}

/// Runs gh.
pub trait Api {
    /// Runs `gh <arguments>` as `token`, with `input` on stdin, and returns what it printed.
    ///
    /// # Errors
    ///
    /// When gh cannot run, fails, or the token for writes is missing.
    fn call(&self, token: Token, arguments: &[&str], input: Option<&str>)
    -> Result<String, String>;
}

/// [`Api`] through the gh program, run in the checkout at `root`, which decides the repository.
#[derive(Debug, Clone)]
pub struct GhProcess {
    root: PathBuf,
}

impl GhProcess {
    /// Runs gh in the checkout at `root`.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        assert!(!root.as_os_str().is_empty(), "gh runs in a checkout");
        Self { root: root.to_path_buf() }
    }

    /// The command `gh <arguments>` as `token`, with the token for writes from `lookup`.
    ///
    /// # Errors
    ///
    /// When `token` is [`Token::Write`] and `lookup` gives no token for [`WRITE_TOKEN`]: a
    /// write never falls back to `GH_TOKEN`.
    pub fn command(
        &self,
        token: Token,
        arguments: &[&str],
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Command, String> {
        let mut command = Command::new("gh");
        let _ = command.args(arguments).current_dir(&self.root).env_remove(WRITE_TOKEN);
        if token == Token::Write {
            let value = lookup(WRITE_TOKEN).filter(|value| !value.trim().is_empty());
            let value = value.ok_or_else(|| {
                format!("`{WRITE_TOKEN}` holds no token, and writes never use `GH_TOKEN`")
            })?;
            let _ = command.env("GH_TOKEN", value);
        }
        let sees = command.get_envs().any(|(name, value)| name == WRITE_TOKEN && value.is_some());
        assert!(!sees, "gh never sees the variable that holds the token for writes");
        Ok(command)
    }
}

impl Api for GhProcess {
    fn call(
        &self,
        token: Token,
        arguments: &[&str],
        input: Option<&str>,
    ) -> Result<String, String> {
        let mut command = self.command(token, arguments, &|name| std::env::var(name).ok())?;
        let _ = command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
        let _ = command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot run gh: {error}; install the GitHub CLI"))?;
        if let Some(text) = input {
            let mut pipe = child.stdin.take().expect("stdin is piped");
            std::io::Write::write_all(&mut pipe, text.as_bytes())
                .map_err(|error| format!("cannot write to gh: {error}"))?;
        }
        let output = child.wait_with_output().map_err(|error| format!("gh failed: {error}"))?;
        printed(arguments, output.status.success(), output.stdout, &output.stderr)
    }
}

/// What gh printed when it succeeded, or why `gh <arguments>` failed, with what it printed
/// on stderr.
///
/// # Errors
///
/// When gh failed, or printed something that is not UTF-8.
pub fn printed(
    arguments: &[&str],
    success: bool,
    stdout: Vec<u8>,
    stderr: &[u8],
) -> Result<String, String> {
    if !success {
        let reason = String::from_utf8_lossy(stderr);
        return Err(format!("`gh {}` failed: {}", arguments.join(" "), reason.trim()));
    }
    String::from_utf8(stdout).map_err(|_| "gh printed invalid UTF-8".to_owned())
}

/// [`Pulls`] through an [`Api`]: reads as [`Token::Read`], writes as [`Token::Write`].
#[derive(Clone, Copy)]
pub struct GitHubPulls<'api> {
    api: &'api dyn Api,
}

impl<'api> GitHubPulls<'api> {
    /// Pull requests through `api`.
    #[must_use]
    pub fn new(api: &'api dyn Api) -> Self {
        Self { api }
    }

    fn read_one(&self, path: &str) -> Result<Value, String> {
        let text = self.api.call(Token::Read, &["api", path], None)?;
        json::parse(&text).map_err(|error| format!("gh printed invalid JSON: {}", error.message))
    }

    fn read_list(&self, path: &str) -> Result<Vec<Value>, String> {
        parse_lines(&self.api.call(
            Token::Read,
            &["api", "--paginate", path, "--jq", ".[]"],
            None,
        )?)
    }

    fn write(&self, method: &str, path: &str, request: &str) -> Result<String, String> {
        let arguments = ["api", "--method", method, path, "--input", "-"];
        self.api.call(Token::Write, &arguments, Some(request))
    }
}

impl Pulls for GitHubPulls<'_> {
    fn pull(&self, number: u32) -> Result<PullState, String> {
        pull_from_json(&self.read_one(&format!("repos/{{owner}}/{{repo}}/pulls/{number}"))?)
    }

    fn has_merged(&self, branch: &str) -> Result<bool, String> {
        let path = format!(
            "repos/{{owner}}/{{repo}}/pulls?state=closed&base={BASE_BRANCH}&head={{owner}}:\
             {branch}&per_page=100"
        );
        Ok(merged_from_json(&self.read_list(&path)?))
    }

    fn open_from(&self, branch: &str) -> Result<Option<u32>, String> {
        let path = format!(
            "repos/{{owner}}/{{repo}}/pulls?state=open&head={{owner}}:{branch}&per_page=100"
        );
        one_open(&self.read_list(&path)?, branch)
    }

    fn open(&self, branch: &str, title: &str, body: &str) -> Result<u32, String> {
        let request = format!(
            "{{\"title\": {}, \"head\": {}, \"base\": {}, \"body\": {}}}",
            json::quote(title),
            json::quote(branch),
            json::quote(BASE_BRANCH),
            json::quote(body)
        );
        let text = self.write("POST", "repos/{owner}/{repo}/pulls", &request)?;
        let pull = json::parse(&text)
            .map_err(|error| format!("gh printed invalid JSON: {}", error.message))?;
        number_of(&pull)
    }

    fn edit(&self, number: u32, title: &str, body: &str) -> Result<(), String> {
        let request =
            format!("{{\"title\": {}, \"body\": {}}}", json::quote(title), json::quote(body));
        let _ =
            self.write("PATCH", &format!("repos/{{owner}}/{{repo}}/pulls/{number}"), &request)?;
        Ok(())
    }

    fn close(&self, number: u32) -> Result<(), String> {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}");
        let _ = self.write("PATCH", &path, "{\"state\": \"closed\"}")?;
        Ok(())
    }

    fn auto_merge(&self, number: u32) -> Result<(), String> {
        let number = number.to_string();
        let arguments = ["pr", "merge", number.as_str(), "--auto", "--squash"];
        let result = self.api.call(Token::Write, &arguments, None);
        result
            .map(|_| ())
            .map_err(|error| format!("{error}; add #{number} to the merge queue by hand"))
    }
}

/// True if one of `pulls`, closed PRs from GitHub's pulls API, merged: its `merged_at` is a
/// time. A PR that was closed without merging has `merged_at: null`.
#[must_use]
pub fn merged_from_json(pulls: &[Value]) -> bool {
    pulls.iter().any(|pull| pull.get("merged_at").and_then(Value::as_str).is_some())
}

/// The one open PR among `pulls`, GitHub's open PRs from `branch`, or `None`.
///
/// # Errors
///
/// When a PR has no valid number, or there is more than one: a branch heads one PR at a time.
pub fn one_open(pulls: &[Value], branch: &str) -> Result<Option<u32>, String> {
    let numbers: Vec<u32> = pulls.iter().map(number_of).collect::<Result<_, String>>()?;
    match numbers.as_slice() {
        [] => Ok(None),
        [number] => Ok(Some(*number)),
        many => Err(format!("GitHub lists {} open PRs from `{branch}`", many.len())),
    }
}

/// Reads a pull request's head branch and author from GitHub's pulls API. A head in another
/// repository, or in one that was deleted, is written `<owner>:<branch>`.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn head_from_json(pull: &Value) -> Result<PullHead, String> {
    let branch = string_at(pull, &["head", "ref"])?;
    let base = string_at(pull, &["base", "repo", "full_name"])?;
    let head = string_at(pull, &["head", "repo", "full_name"]).unwrap_or_default();
    let author = string_at(pull, &["user", "login"])?;
    Ok(PullHead::from_event(&branch, &head, &base, &author))
}

/// Reads a pull request's head, title, description and auto-merge from GitHub's pulls API.
/// Auto-merge is on when `auto_merge` is an object; GitHub sends `null` when it is off.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn pull_from_json(pull: &Value) -> Result<PullState, String> {
    let auto_merge = match pull.get("auto_merge") {
        Some(Value::Object(_)) => true,
        None | Some(Value::Null) => false,
        Some(other) => return Err(format!("GitHub sent {} as `auto_merge`", other.kind())),
    };
    let state = PullState {
        head: head_from_json(pull)?,
        title: string_at(pull, &["title"])?,
        body: optional_string(pull, "body")?,
        auto_merge,
    };
    Ok(state)
}

/// The `number` of a pull request in GitHub's pulls API.
///
/// # Errors
///
/// When it is missing, or not a positive integer.
pub fn number_of(pull: &Value) -> Result<u32, String> {
    let Some(Value::Number(digits)) = pull.get("number") else {
        return Err("a pull request has no `number`".to_owned());
    };
    let number = digits.parse::<u32>().ok().filter(|number| *number > 0);
    number.ok_or_else(|| format!("a pull request has the number `{digits}`"))
}

#[cfg(test)]
mod tests {
    use super::{
        Api, GhProcess, GitHubPulls, Token, WRITE_TOKEN, head_from_json, merged_from_json,
        number_of, one_open, printed, pull_from_json,
    };
    use crate::json::{self, Value};
    use crate::task::passes::{PullHead, Pulls};
    use std::cell::RefCell;

    /// gh with fixed answers, which logs each call with its token.
    struct Fake {
        answer: String,
        calls: RefCell<Vec<(Token, String)>>,
    }

    impl Api for Fake {
        fn call(
            &self,
            token: Token,
            arguments: &[&str],
            _input: Option<&str>,
        ) -> Result<String, String> {
            self.calls.borrow_mut().push((token, arguments.join(" ")));
            Ok(self.answer.clone())
        }
    }

    fn fake(answer: &str) -> Fake {
        Fake { answer: answer.to_owned(), calls: RefCell::new(Vec::new()) }
    }

    fn values(text: &str) -> Vec<Value> {
        let value = json::parse(text).expect("the fixture is JSON");
        value.as_array().expect("an array").to_vec()
    }

    const PULL: &str = r#"{"number": 9, "title": "Set passes: a", "body": null,
        "auto_merge": null, "user": {"login": "nami-ci[bot]"}, "base": {"repo":
        {"full_name": "o/n"}}, "head": {"ref": "bot/passes", "repo": {"full_name": "o/n"}}}"#;

    // Goal: every read acts as the read token and every write as the write token, so the
    // app's token never reads and the workflow's never writes (ADR-0023, rule 2).
    #[test]
    fn each_call_takes_only_its_own_token() {
        let gh = fake(PULL);
        let pulls = GitHubPulls::new(&gh);
        let state = pulls.pull(9).expect("a pull request");
        assert_eq!(state.head.author, "nami-ci[bot]");
        let lines = fake(&format!("{}\n", PULL.replace('\n', " ")));
        let listing = GitHubPulls::new(&lines);
        assert_eq!(listing.has_merged("task/a"), Ok(false), "no merged_at");
        assert_eq!(listing.open_from("bot/passes"), Ok(Some(9)));
        assert_eq!(pulls.open("bot/passes", "t", "b"), Ok(9));
        assert_eq!(pulls.edit(9, "t", "b"), Ok(()));
        assert_eq!(pulls.close(9), Ok(()));
        assert_eq!(pulls.auto_merge(9), Ok(()));
        let tokens = |calls: &[(Token, String)]| -> Vec<Token> {
            calls.iter().map(|(token, _)| *token).collect()
        };
        assert_eq!(tokens(&lines.calls.borrow()), [Token::Read, Token::Read]);
        let written = [Token::Read, Token::Write, Token::Write, Token::Write, Token::Write];
        assert_eq!(tokens(&gh.calls.borrow()), written);
        let calls = gh.calls.borrow();
        assert!(calls[1].1.starts_with("api --method POST repos/{owner}/{repo}/pulls"), "open");
        assert_eq!(calls[4].1, "pr merge 9 --auto --squash");
        assert!(lines.calls.borrow()[0].1.contains("state=closed&base=main&head={owner}:task/a"));
    }

    // Goal: the read client never sees the write token, and the write client passes it to gh
    // as GH_TOKEN; a missing or blank write token is an error, never GH_TOKEN.
    #[test]
    fn the_write_token_reaches_only_writes() {
        let process = GhProcess::new(std::path::Path::new("."));
        let tokens = |name: &str| (name == WRITE_TOKEN).then(|| "write".to_owned());
        let environment = |token: Token| -> Vec<(String, Option<String>)> {
            let command = process.command(token, &["api", "x"], &tokens).expect("a token");
            let text = |value: &std::ffi::OsStr| value.to_str().expect("UTF-8").to_owned();
            command.get_envs().map(|(name, value)| (text(name), value.map(text))).collect()
        };
        let removed = (WRITE_TOKEN.to_owned(), None);
        assert_eq!(
            environment(Token::Read),
            std::slice::from_ref(&removed),
            "GH_TOKEN is inherited"
        );
        let mut written = environment(Token::Write);
        written.sort();
        assert_eq!(written, [("GH_TOKEN".to_owned(), Some("write".to_owned())), removed]);
        for blank in [None, Some(" ".to_owned())] {
            let error = process.command(Token::Write, &["x"], &|_| blank.clone());
            let error = error.expect_err("no token for writes");
            assert!(error.contains("`NAMI_PASSES_TOKEN` holds no token"), "{error}");
        }
        assert!(process.command(Token::Read, &["x"], &|_| None).is_ok(), "reads need none");
    }

    // Goal: only a PR with a `merged_at` time counts as merged; a closed, unmerged PR does not.
    #[test]
    fn only_merged_pulls_count() {
        assert!(!merged_from_json(&[]), "no PR");
        let closed = r#"[{"number": 1, "merged_at": null}, {"number": 2}]"#;
        assert!(!merged_from_json(&values(closed)), "closed without merging");
        let merged = r#"[{"number": 1, "merged_at": null}, {"merged_at": "2026-11-26T08:25Z"}]"#;
        assert!(merged_from_json(&values(merged)), "one merged");
    }

    // Goal: a branch heads no PR or one; two open PRs, or one without a number, are errors.
    #[test]
    fn a_branch_has_at_most_one_open_pull() {
        assert_eq!(one_open(&[], "b"), Ok(None));
        assert_eq!(one_open(&values(r#"[{"number": 4}]"#), "b"), Ok(Some(4)));
        let two = one_open(&values(r#"[{"number": 4}, {"number": 5}]"#), "b");
        assert_eq!(two, Err("GitHub lists 2 open PRs from `b`".to_owned()));
        assert!(one_open(&values(r#"[{"title": "x"}]"#), "b").is_err(), "no number");
    }

    // Goal: a pull request's number is read from GitHub's JSON, and a missing, zero or
    // non-integer number is an error.
    #[test]
    fn pull_numbers_come_from_github() {
        let pull = |text: &str| json::parse(text).expect("the fixture is JSON");
        assert_eq!(number_of(&pull(r#"{"number": 64}"#)), Ok(64));
        for bad in [r#"{"number": 0}"#, r#"{"number": "7"}"#, r#"{"number": 1.5}"#, "{}"] {
            assert!(number_of(&pull(bad)).is_err(), "{bad}");
        }
    }

    // Goal: a head in this repository gives its branch name, and a fork's gives
    // `<owner>:<branch>`, so a fork's `bot/passes` is never the bot's.
    #[test]
    fn heads_name_their_repository() {
        let pull = |head: &str| {
            json::parse(&format!(
                r#"{{"user": {{"login": "nami-ci[bot]"}}, "base": {{"repo": {{"full_name":
                "o/n"}}}}, "head": {{"ref": "bot/passes", "label": "x:bot/passes", "repo":
                {head}}}}}"#
            ))
            .expect("the fixture is JSON")
        };
        let own = head_from_json(&pull(r#"{"full_name": "o/n"}"#)).expect("valid");
        let expected =
            PullHead { branch: "bot/passes".to_owned(), author: "nami-ci[bot]".to_owned() };
        assert_eq!(own, expected);
        let fork = head_from_json(&pull(r#"{"full_name": "x/n"}"#)).expect("valid");
        assert_eq!(fork.branch, "x:bot/passes");
        let deleted = head_from_json(&pull("null")).expect("valid");
        assert_eq!(deleted.branch, "(unknown):bot/passes", "a deleted fork is not this repository");
        let gone = json::parse(r#"{"head": {"ref": "b"}}"#).expect("JSON");
        assert!(head_from_json(&gone).is_err(), "no base");
    }

    // Goal: a PR's title, description and auto-merge are read as GitHub sends them: a null
    // description is empty, and auto-merge is on only when GitHub sends an object.
    #[test]
    fn pull_states_come_from_github() {
        let state = |text: &str| pull_from_json(&json::parse(text).expect("the fixture is JSON"));
        let off = state(PULL).expect("valid");
        assert_eq!(off.title, "Set passes: a");
        assert_eq!((off.body.as_str(), off.auto_merge), ("", false), "null body, auto-merge off");
        let on = PULL.replace(r#""body": null"#, r#""body": "b""#);
        let on = on.replace(r#""auto_merge": null"#, r#""auto_merge": {}"#);
        let on = state(&on).expect("valid");
        assert_eq!((on.body.as_str(), on.auto_merge), ("b", true));
        let odd = PULL.replace(r#""auto_merge": null"#, r#""auto_merge": true"#);
        let error = state(&odd).expect_err("not an object");
        assert!(error.contains("`auto_merge`"), "{error}");
        let untitled = PULL.replace(r#""title": "Set passes: a","#, "");
        assert!(state(&untitled).is_err(), "no title");
    }

    // Goal: gh's output is returned only when it succeeded; a failure names the command and
    // what gh said, and output that is not UTF-8 is an error.
    #[test]
    fn gh_output_counts_only_on_success() {
        assert_eq!(printed(&["api", "x"], true, b"ok".to_vec(), b""), Ok("ok".to_owned()));
        let failed = printed(
            &["api", "x"],
            false,
            b"ok".to_vec(),
            b" HTTP 404 
",
        );
        assert_eq!(failed, Err("`gh api x` failed: HTTP 404".to_owned()));
        assert!(printed(&["x"], true, vec![0xff], b"").is_err(), "not UTF-8");
    }
}
