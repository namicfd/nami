//! `cargo xtask protected` and `cargo xtask pr-body`: what a pull request must say about itself.
//!
//! `AGENTS.md` requires a PR description to list every protected file that the PR changes, and
//! the issues that the work filed for what it found outside its task (ADR-0020, ADR-0021).
//! These commands check both, so that the rule does not depend on anyone remembering it.
//!
//! - `protected` prints the changed paths that `.github/CODEOWNERS` protects, one per line.
//! - `pr-body` checks the description in the environment variable `PR_BODY`. On the branch of
//!   a claimed issue, `task/issue-<number>` (from `PR_BRANCH`, or git), the description must
//!   close that issue. Every problem reads `error[pr-body]: what; fix`, like a tidy violation.

use crate::issue::github::{Gh, GitHub};
use crate::issue::trust::{self, Trust};
use std::cell::OnceCell;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

/// The file that lists the protected paths, relative to the workspace root.
const CODEOWNERS_FILE: &str = ".github/CODEOWNERS";

/// The ref that a PR is compared with unless `--base` names another.
const BASE_DEFAULT: &str = "origin/main";

/// The section of a PR description that lists the protected files the PR changes.
const HEADING_PROTECTED: &str = "## Protected files";

/// The section of a PR description that lists the issues the work filed (ADR-0021).
const HEADING_FOLLOW_UPS: &str = "## Follow-up issues";

/// How the attribution line starts that Claude Code appends to every PR description it writes.
/// The line is not part of the section it lands in, which is the template's last (issue #18).
const ATTRIBUTION_PREFIX: &str = "🤖 Generated with";

/// The prefix of a branch that works on a task or a claimed issue (ADR-0011).
const TASK_BRANCH_PREFIX: &str = "task/";

/// Words that close an issue when `#<number>` follows them in a merged PR's description.
const CLOSING_WORDS: [&str; 9] =
    ["close", "closed", "closes", "fix", "fixed", "fixes", "resolve", "resolved", "resolves"];

/// Characters that make a CODEOWNERS pattern a glob, a negation or an escape. Outside the
/// leading `**/`, none of them is supported.
const GLOB_CHARACTERS: [char; 6] = ['*', '?', '[', ']', '!', '\\'];

/// Markdown punctuation that may surround a path in a PR description.
const MENTION_DELIMITERS: [char; 13] =
    ['`', '\'', '"', '(', ')', '[', ']', '<', '>', ',', ';', ':', '*'];

/// One CODEOWNERS pattern, in one of the three forms that nami's file uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// `/path/file`: that path from the root. Stored without the leading slash.
    File(String),
    /// `/dir/`: everything below that directory. Stored as `dir/`.
    Directory(String),
    /// `**/name`: that name at any depth.
    AnyDepth(String),
}

impl Pattern {
    /// True if the pattern covers `path`, which is relative to the repository root.
    ///
    /// As on GitHub, a pattern that names a directory covers everything below it, also when the
    /// pattern has no trailing slash.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        match self {
            Self::File(file) => path
                .strip_prefix(file.as_str())
                .is_some_and(|below| below.is_empty() || below.starts_with('/')),
            Self::Directory(directory) => path.starts_with(directory.as_str()),
            Self::AnyDepth(name) => path.split('/').any(|component| component == name),
        }
    }
}

/// Parses the patterns of a CODEOWNERS file.
///
/// # Errors
///
/// Returns the line and the reason if a pattern is not in one of the three supported forms, or
/// has no owner. An unsupported form is an error, not skipped, so that no path loses its
/// protection silently.
pub fn parse_codeowners(text: &str) -> Result<Vec<Pattern>, String> {
    let mut patterns = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let mut words = line.split_whitespace().take_while(|word| !word.starts_with('#'));
        let Some(pattern) = words.next() else {
            continue;
        };
        let line_number = index + 1;
        let Some(parsed) = parse_pattern(pattern) else {
            return Err(format!(
                "{CODEOWNERS_FILE}:{line_number}: unsupported pattern `{pattern}`; use \
                 `/path/file`, `/dir/` or `**/name`, or teach xtask/src/pr.rs the new form"
            ));
        };
        if words.next().is_none() {
            return Err(format!(
                "{CODEOWNERS_FILE}:{line_number}: pattern `{pattern}` has no owner; name the \
                 owner, or remove the line"
            ));
        }
        patterns.push(parsed);
    }
    Ok(patterns)
}

fn parse_pattern(pattern: &str) -> Option<Pattern> {
    if let Some(name) = pattern.strip_prefix("**/") {
        let is_name = is_plain_path(name) && !name.contains('/');
        return is_name.then(|| Pattern::AnyDepth(name.to_owned()));
    }
    let path = pattern.strip_prefix('/').filter(|path| is_plain_path(path))?;
    if path.ends_with('/') {
        Some(Pattern::Directory(path.to_owned()))
    } else {
        Some(Pattern::File(path.to_owned()))
    }
}

/// True for a relative path that is spelled out: not empty, no glob characters and no empty
/// components.
fn is_plain_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(GLOB_CHARACTERS)
        && !path.starts_with('/')
        && !path.contains("//")
}

/// The paths of `changed` that a pattern covers, sorted and without duplicates.
#[must_use]
pub fn protected_paths(patterns: &[Pattern], changed: &[String]) -> Vec<String> {
    let is_protected = |path: &&String| patterns.iter().any(|pattern| pattern.matches(path));
    let mut protected: Vec<String> = changed.iter().filter(is_protected).cloned().collect();
    protected.sort();
    protected.dedup();
    protected
}

/// The lines of the section of `body` under `heading`, or `None` if there is no such heading.
///
/// HTML comments, blank lines and thematic breaks (`---`) are left out, so they neither count
/// as content nor end the section: what follows them is still checked. So are the attribution
/// lines at the end of the section, after its last other line; an attribution line in the
/// middle is content. The section ends at the next `#` or `##` heading.
#[must_use]
pub fn section(body: &str, heading: &str) -> Option<Vec<String>> {
    let visible = without_comments(body);
    let mut lines = visible.lines().map(str::trim).skip_while(|line| *line != heading);
    let _heading = lines.next()?;
    let is_next_section = |line: &&str| line.starts_with("# ") || line.starts_with("## ");
    let is_filler = |line: &&str| line.is_empty() || is_thematic_break(line);
    let mut content: Vec<String> = lines
        .take_while(|line| !is_next_section(line))
        .filter(|line| !is_filler(line))
        .map(str::to_owned)
        .collect();
    let footer_start = content
        .iter()
        .rposition(|line| !line.starts_with(ATTRIBUTION_PREFIX))
        .map_or(0, |last| last + 1);
    content.truncate(footer_start);
    assert!(
        content.last().is_none_or(|line| !line.starts_with(ATTRIBUTION_PREFIX)),
        "the trailing attribution lines are cut off"
    );
    Some(content)
}

/// True for a line of three or more `-` and nothing else.
fn is_thematic_break(line: &str) -> bool {
    line.len() >= 3 && line.chars().all(|character| character == '-')
}

/// Removes HTML comments. A comment that is never closed runs to the end of the text.
fn without_comments(text: &str) -> String {
    let mut pieces = text.split("<!--");
    let mut visible = String::from(pieces.next().unwrap_or(""));
    for piece in pieces {
        if let Some((_comment, after)) = piece.split_once("-->") {
            visible.push_str(after);
        }
    }
    visible
}

/// Checks a PR description. Returns one line per problem, and nothing if it is complete.
///
/// `protected` holds the protected paths that the PR changes, and `branch` is the PR's branch.
/// `issue_problem` says why an issue that the description lists is not a follow-up issue, or
/// `None` if it is one; it is asked about each number under `## Follow-up issues`.
#[must_use]
pub fn check_body(
    body: &str,
    protected: &[String],
    branch: &str,
    issue_problem: impl Fn(u32) -> Option<String>,
) -> Vec<String> {
    let follow_ups = section(body, HEADING_FOLLOW_UPS);
    let mut problems = follow_up_problems(follow_ups.as_deref(), issue_problem);
    problems.extend(closing_problem(body, branch));
    let listed = section(body, HEADING_PROTECTED);
    problems.extend(protected_file_problems(listed.as_deref(), protected));
    problems.iter().map(|problem| format!("error[pr-body]: {problem}")).collect()
}

fn follow_up_problems(
    section: Option<&[String]>,
    issue_problem: impl Fn(u32) -> Option<String>,
) -> Vec<String> {
    let how = "write `None`, or list each issue that this work filed with `cargo xtask issue \
               file`, as `#<number>`";
    let Some(lines) = section else {
        return vec![format!("the section `{HEADING_FOLLOW_UPS}` is missing; add it and {how}")];
    };
    if let [line] = lines
        && is_none(line)
    {
        return Vec::new();
    }
    if let [first, second, ..] = lines
        && is_none(first)
    {
        return vec![format!(
            "the section `{HEADING_FOLLOW_UPS}` says `None` and then `{second}`; write only \
             `None` there and move the rest to another section, or list each issue that this \
             work filed with `cargo xtask issue file`, as `#<number>`, without `None`"
        )];
    }
    let numbers = issue_numbers(&lines.join("\n"));
    if numbers.is_empty() {
        return vec![format!("the section `{HEADING_FOLLOW_UPS}` names no issue; {how}")];
    }
    numbers
        .iter()
        .filter_map(|&number| {
            let problem = issue_problem(number)?;
            Some(format!(
                "#{number}, listed under `{HEADING_FOLLOW_UPS}`, is not a follow-up issue: \
                 {problem}; list only issues filed with `cargo xtask issue file`"
            ))
        })
        .collect()
}

/// True for `None` in any case, with or without a full stop.
fn is_none(line: &str) -> bool {
    let word = line.trim();
    word.strip_suffix('.').unwrap_or(word).eq_ignore_ascii_case("none")
}

/// Every issue number that `text` writes as `#<number>`, sorted and without duplicates.
fn issue_numbers(text: &str) -> Vec<u32> {
    let mut numbers: Vec<u32> = text
        .split('#')
        .skip(1)
        .filter_map(|after| {
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            crate::issue::canonical_number(&digits)
        })
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

/// On the branch of a claimed issue, `task/issue-<number>`, the description must close it, so
/// that the merge marks the issue as done (ADR-0021).
fn closing_problem(body: &str, branch: &str) -> Option<String> {
    let id = branch.strip_prefix(TASK_BRANCH_PREFIX)?;
    let number = crate::issue::number_of_id(id).ok().flatten()?;
    (!closes(body, number)).then(|| {
        format!(
            "the branch `{branch}` works on issue #{number}, but the description does not close \
             it; add the line `Fixes #{number}` (ADR-0021)"
        )
    })
}

/// True if the PR description `body` closes issue `number` on merge: a closing word such as
/// `Fixes`, then `#<number>`, outside HTML comments.
#[must_use]
pub fn closes(body: &str, number: u32) -> bool {
    let reference = format!("#{number}");
    let visible = without_comments(body);
    let words: Vec<&str> = visible.split_whitespace().collect();
    words.array_windows().any(|[word, next]| {
        let word = word.trim_end_matches(':').to_ascii_lowercase();
        let next = next.trim_end_matches(['.', ',', ';', ')']);
        CLOSING_WORDS.contains(&word.as_str()) && next == reference
    })
}

/// Why issue `number` is not a follow-up issue, or `None` if it is one: GitHub has it, it is
/// an issue and not a pull request, and the bot or a code owner opened it.
#[must_use]
pub fn listed_issue_problem(github: &dyn GitHub, trust: &Trust, number: u32) -> Option<String> {
    match github.issue(number) {
        Ok(found) => trust::refused(&found, trust),
        Err(error) => Some(format!("cannot look it up: {error}")),
    }
}

fn protected_file_problems(section: Option<&[String]>, protected: &[String]) -> Vec<String> {
    if protected.is_empty() {
        return Vec::new();
    }
    let Some(lines) = section else {
        return vec![format!(
            "the section `{HEADING_PROTECTED}` is missing, and this PR changes protected paths; \
             add it and list every path that `cargo xtask protected` prints, with the reason"
        )];
    };
    let text = lines.join("\n");
    let mentions: Vec<&str> = text
        .split(|character: char| {
            character.is_whitespace() || MENTION_DELIMITERS.contains(&character)
        })
        .map(|word| word.strip_suffix('.').unwrap_or(word))
        .collect();
    let unlisted = protected.iter().filter(|path| !is_mentioned(path, &mentions));
    unlisted
        .map(|path| {
            format!(
                "the protected path `{path}` is not listed under `{HEADING_PROTECTED}`; list it \
                 there with the reason, or list a directory above it with a trailing `/`"
            )
        })
        .collect()
}

/// True if `mentions` holds `path` itself, or a directory above it that ends in `/`.
fn is_mentioned(path: &str, mentions: &[&str]) -> bool {
    mentions
        .iter()
        .any(|mention| *mention == path || (mention.ends_with('/') && path.starts_with(mention)))
}

/// Prints the protected paths that differ between the merge base with the base ref and `HEAD`.
pub fn run_protected(options: &[&str]) -> ExitCode {
    match changed_protected(&crate::workspace_root(), options) {
        Ok(paths) => {
            for path in &paths {
                println!("{path}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error[protected]: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Checks the PR description in the environment variable `PR_BODY` and prints its problems.
/// The branch is `PR_BRANCH`, or else git's current branch.
pub fn run_body(options: &[&str]) -> ExitCode {
    let root = crate::workspace_root();
    let body = std::env::var("PR_BODY").ok();
    let branch = std::env::var("PR_BRANCH").ok().filter(|branch| !branch.is_empty());
    let branch = branch.unwrap_or_else(|| {
        let current = crate::task::claims::git(&root, &["branch", "--show-current"], &[], None);
        current.map_or_else(|_| String::new(), |name| name.trim().to_owned())
    });
    let problems = body_problems(&root, options, body.as_deref(), &branch, &Gh::new(&root));
    for problem in &problems {
        eprintln!("{problem}");
    }
    if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// The problems of the PR description `body` on `branch`, for the checkout at `root`, one line
/// each. The issues it lists are looked up on `github`, and the code owners come from the base.
///
/// `options` are the options of `pr-body`. `body` is `None` if `PR_BODY` is not set. Whatever
/// keeps the check from running is a problem too, so that a check that cannot run never passes.
#[must_use]
pub fn body_problems(
    root: &Path,
    options: &[&str],
    body: Option<&str>,
    branch: &str,
    github: &dyn GitHub,
) -> Vec<String> {
    let Some(body) = body else {
        return vec![
            "error[pr-body]: the environment variable PR_BODY is not set; set it to the PR \
             description, for example `PR_BODY=\"$(gh pr view --json body --jq .body)\" cargo \
             xtask pr-body`"
                .to_owned(),
        ];
    };
    let protected = match changed_protected(root, options) {
        Ok(protected) => protected,
        Err(error) => return vec![format!("error[pr-body]: {error}")],
    };
    let base = base_of(options).expect("changed_protected accepted the options");
    // Loaded only when the description lists an issue: `None` needs neither git nor GitHub.
    let trust = OnceCell::new();
    check_body(body, &protected, branch, |number| {
        match trust.get_or_init(|| trust::load(root, base)) {
            Ok(trust) => listed_issue_problem(github, trust, number),
            Err(error) => Some(error.clone()),
        }
    })
}

/// Reads the base ref from the options of `protected` and `pr-body`.
fn base_of<'a>(options: &[&'a str]) -> Result<&'a str, String> {
    match options {
        [] => Ok(BASE_DEFAULT),
        // A ref that starts with a dash would reach git as an option.
        ["--base", base] if !base.is_empty() && !base.starts_with('-') => Ok(base),
        _ => Err(format!(
            "cannot read the options `{}`; the only option is `--base <ref>`",
            options.join(" ")
        )),
    }
}

/// The paths that differ between the merge base with the base ref and `HEAD`, and that the
/// CODEOWNERS file of the checkout at `root` protects. Sorted, without duplicates.
///
/// `options` are the options of `protected`: nothing, or `--base <ref>`.
///
/// # Errors
///
/// Returns what failed and how to fix it if the options are not understood, if git cannot
/// compare the two commits, or if the CODEOWNERS file is unreadable or has an unsupported
/// pattern.
pub fn changed_protected(root: &Path, options: &[&str]) -> Result<Vec<String>, String> {
    let base = base_of(options)?;
    let codeowners = fs::read_to_string(root.join(CODEOWNERS_FILE))
        .map_err(|error| format!("cannot read {CODEOWNERS_FILE}: {error}; restore the file"))?;
    let patterns = parse_codeowners(&codeowners)?;
    let range = format!("{base}...HEAD");
    // Without rename detection a file moved out of a protected directory still shows its old
    // path. `-z` keeps unusual file names unquoted.
    let output = Command::new("git")
        .args(["diff", "--name-only", "--no-renames", "-z", &range, "--"])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run git: {error}; install git"))?;
    if !output.status.success() {
        return Err(format!(
            "`git diff --name-only {range}` failed: {}; fetch the base with `git fetch origin \
             main`, or pass `--base <ref>`",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let names = String::from_utf8_lossy(&output.stdout);
    let changed: Vec<String> =
        names.split('\0').filter(|name| !name.is_empty()).map(str::to_owned).collect();
    Ok(protected_paths(&patterns, &changed))
}

#[cfg(test)]
mod tests {
    use super::{
        Pattern, base_of, check_body, listed_issue_problem, parse_codeowners, protected_paths,
        section,
    };
    use crate::issue::github::{Comment, GitHub, Issue, LabelEvent};
    use crate::issue::trust::Trust;

    /// A description that passes when `xtask/src/hook.rs` and `justfile` are the protected
    /// changes and #12 is a follow-up issue.
    const BODY: &str = "## Summary\n\nFix the stop hook.\n\n## Rule changes\n\nNone\n\n\
                        ## Protected files\n\n- `xtask/`: the hook lives there.\n\
                        - justfile: new recipe.\n\n## Follow-up issues\n\n- #12\n";

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn body_with_follow_ups(follow_ups: &str) -> String {
        format!("## Summary\n\nText.\n\n## Follow-up issues\n{follow_ups}\n")
    }

    fn body_with_protected(listed: &str) -> String {
        format!("## Protected files\n{listed}\n\n## Follow-up issues\n\nNone\n")
    }

    /// Issue #12 is a follow-up issue; no other number is.
    fn exists(number: u32) -> Option<String> {
        (number != 12).then(|| "it is not #12".to_owned())
    }

    /// GitHub holding #1, a pull request #2 and #3 by an outsider.
    struct Issues;

    impl GitHub for Issues {
        fn issue(&self, number: u32) -> Result<Issue, String> {
            let (author, is_pull_request) = match number {
                1 => ("nami-agent[bot]", false),
                2 => ("owner", true),
                3 => ("outsider", false),
                _ => return Err("not found".to_owned()),
            };
            Ok(Issue {
                number,
                title: String::new(),
                author: author.to_owned(),
                is_open: true,
                is_pull_request,
                labels: Vec::new(),
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

    // Goal: the real CODEOWNERS file parses, and its patterns protect what AGENTS.md says is
    // protected and nothing next to it. A pattern in a new form fails this test.
    #[test]
    fn real_codeowners_protects_the_documented_paths() {
        let text = std::fs::read_to_string(crate::workspace_root().join(super::CODEOWNERS_FILE))
            .expect("CODEOWNERS is readable");
        let patterns = parse_codeowners(&text).expect("CODEOWNERS uses only supported forms");
        let changed = paths(&[
            ".cargo/mutants.toml",
            ".claude/settings.json",
            ".github/workflows/ci.yml",
            "AGENTS.md",
            "Cargo.lock",
            "Cargo.toml",
            "README.md",
            "crates/base/Cargo.toml",
            "crates/base/src/lib.rs",
            "crates/reference/src/lib.rs",
            "docs/TIGER_STYLE.md",
            "docs/design/0001-dependency-policy.md",
            "docs/research/notes.md",
            "justfile",
            "lints/src/lib.rs",
            "xtask/src/hook.rs",
        ]);
        let unprotected = paths(&["README.md", "crates/base/src/lib.rs", "docs/research/notes.md"]);
        let expected: Vec<String> =
            changed.iter().filter(|path| !unprotected.contains(path)).cloned().collect();
        assert_eq!(protected_paths(&patterns, &changed), expected);
    }

    // Goal: each of the three supported forms parses to its own variant; comments, blank lines
    // and trailing comments are skipped.
    #[test]
    fn supported_patterns_are_parsed() {
        let text = "# Comment.\n\n/Cargo.lock @a\n/docs/design/  @a @b # why\n**/Cargo.toml\t@a\n";
        assert_eq!(
            parse_codeowners(text),
            Ok(vec![
                Pattern::File("Cargo.lock".to_owned()),
                Pattern::Directory("docs/design/".to_owned()),
                Pattern::AnyDepth("Cargo.toml".to_owned()),
            ])
        );
    }

    // Goal: every other pattern form is an error that names the line, and so is a pattern
    // without an owner, which on GitHub would remove the protection.
    #[test]
    fn unsupported_patterns_are_errors() {
        let unsupported = [
            "*.rs",
            "docs/",
            "justfile",
            "/src/*.rs",
            "/a/**/b",
            "**/a/b",
            "**/",
            "**/*.toml",
            "/",
            "//",
            "/a//b",
            "!/a",
            "/a?",
            "/[ab]",
            "/a\\b",
            "**",
            "*",
        ];
        for pattern in unsupported {
            let error = parse_codeowners(&format!("/ok @a\n{pattern} @a\n"))
                .expect_err("the pattern is not in a supported form");
            assert!(error.starts_with(".github/CODEOWNERS:2: unsupported pattern"), "{error}");
            assert!(error.contains(&format!("`{pattern}`")), "{error}");
        }
        let error = parse_codeowners("/a # @owner\n").expect_err("a comment is not an owner");
        assert!(error.starts_with(".github/CODEOWNERS:1: pattern `/a` has no owner"), "{error}");
    }

    // Goal: each form covers exactly its paths. Method: for each pattern, paths on both sides of
    // every boundary: the name as a prefix of another name, as a directory, and at depth.
    #[test]
    fn patterns_cover_exactly_their_paths() {
        let file = Pattern::File("docs/TIGER_STYLE.md".to_owned());
        assert!(file.matches("docs/TIGER_STYLE.md"), "the file itself");
        assert!(file.matches("docs/TIGER_STYLE.md/part"), "a directory of that name");
        assert!(!file.matches("docs/TIGER_STYLE.md.bak"), "a longer name");
        assert!(!file.matches("old/docs/TIGER_STYLE.md"), "the pattern is anchored");
        assert!(!file.matches("docs/TIGER_STYLE"), "a shorter name");

        let directory = Pattern::Directory("xtask/".to_owned());
        assert!(directory.matches("xtask/src/hook.rs"), "a file below the directory");
        assert!(!directory.matches("xtask"), "a file of that name is not below it");
        assert!(!directory.matches("xtask2/src/hook.rs"), "a longer directory name");
        assert!(!directory.matches("crates/xtask/src/hook.rs"), "the pattern is anchored");

        let any_depth = Pattern::AnyDepth("Cargo.toml".to_owned());
        assert!(any_depth.matches("Cargo.toml"), "at the root");
        assert!(any_depth.matches("crates/base/Cargo.toml"), "at depth");
        assert!(any_depth.matches("Cargo.toml/part"), "a directory of that name");
        assert!(!any_depth.matches("crates/base/Cargo.toml.orig"), "a longer name");
        assert!(!any_depth.matches("crates/base/myCargo.toml"), "a name that ends the same");
    }

    // Goal: the protected paths come back sorted and once each, whatever the input order.
    #[test]
    fn protected_paths_are_sorted_and_unique() {
        let patterns = [Pattern::Directory("xtask/".to_owned())];
        let changed = paths(&["xtask/b.rs", "src/a.rs", "xtask/a.rs", "xtask/b.rs"]);
        assert_eq!(protected_paths(&patterns, &changed), paths(&["xtask/a.rs", "xtask/b.rs"]));
        assert_eq!(protected_paths(&[], &changed), Vec::<String>::new());
    }

    // Goal: a section is the text between its heading and the next `#` or `##` heading, without
    // blank lines, comments, `---` lines and the attribution line; a heading inside a comment
    // does not count, and a `---` line does not end the section.
    #[test]
    fn sections_are_cut_out_of_the_body() {
        let body = "# Title\n## A\n\none\n<!-- hidden\n## B\n-->\n### Detail\n two \n## B\nthree\n";
        assert_eq!(section(body, "## A"), Some(paths(&["one", "### Detail", "two"])));
        assert_eq!(section(body, "## B"), Some(paths(&["three"])));
        assert_eq!(section(body, "## C"), None);
        assert_eq!(section("## A\none\n# Next\ntwo\n", "## A"), Some(paths(&["one"])));
        assert_eq!(section("## A\r\none\r\n", "## A"), Some(paths(&["one"])));
        assert_eq!(section("## A\n<!-- only a comment -->\n", "## A"), Some(Vec::new()));
        assert_eq!(section("## A\none <!-- open\ntwo\n", "## A"), Some(paths(&["one"])));
        assert_eq!(section("## A and more\none\n", "## A"), None);
        assert_eq!(section("## A\none\n---\ntwo\n", "## A"), Some(paths(&["one", "two"])));
        assert_eq!(section("## A\n-----\n", "## A"), Some(Vec::new()));
        assert_eq!(section("## A\none\n--\n", "## A"), Some(paths(&["one", "--"])));
        let attribution = "## A\n🤖 Generated with [Claude Code](x)\n";
        assert_eq!(section(attribution, "## A"), Some(Vec::new()));
    }

    // Goal: a complete description has no problems, and each problem is one line in the
    // documented form.
    #[test]
    fn complete_body_passes() {
        let protected = paths(&["justfile", "xtask/src/hook.rs"]);
        assert_eq!(check_body(BODY, &protected, "work", exists), Vec::<String>::new());
        let problems = check_body("", &protected, "work", exists);
        assert_eq!(problems.len(), 2, "one per missing section: {problems:?}");
        for problem in &problems {
            assert!(problem.starts_with("error[pr-body]: the section `## "), "{problem}");
            assert!(problem.contains(" is missing"), "{problem}");
            assert!(problem.contains("; add it and "), "a problem says how to fix it: {problem}");
            assert!(!problem.contains('\n'), "one line per problem: {problem}");
        }
    }

    // Goal: the follow-up section accepts `None` in any case with an optional full stop.
    #[test]
    fn follow_ups_may_be_none() {
        for none in ["None", "none", "NONE.", "  None.  ", "<!-- say why -->\n\nNone\n"] {
            let problems = check_body(&body_with_follow_ups(none), &[], "work", exists);
            assert_eq!(problems, Vec::<String>::new(), "`{none}` means no follow-ups");
        }
    }

    // Goal: the attribution line that Claude Code appends to a PR description, and a `---` line
    // before it, are not part of the last section (issue #18): they neither count as content
    // nor make an empty section pass.
    #[test]
    fn attribution_is_not_part_of_the_section() {
        let footer = "🤖 Generated with [Claude Code](https://claude.com/claude-code)";
        let passing = [
            format!("\nNone.\n\n{footer}"),
            format!("\nNone.\n\n---\n\n{footer}"),
            format!("\n- #12\n\n{footer}"),
            format!("- #12\n---\n{footer}"),
            "---\n- #12".to_owned(),
        ];
        for follow_ups in passing {
            let body = body_with_follow_ups(&follow_ups);
            assert_eq!(check_body(&body, &[], "work", exists), Vec::<String>::new(), "{body}");
        }
        let more_footers = format!("None.\n{footer}\n\n---\n<!-- end -->\n{footer}\n");
        let body = body_with_follow_ups(&more_footers);
        assert_eq!(check_body(&body, &[], "work", exists), Vec::<String>::new(), "{body}");
        let failing = [format!("\n{footer}"), format!("\n---\n\n{footer}"), "---".to_owned()];
        for follow_ups in failing {
            let problems = check_body(&body_with_follow_ups(&follow_ups), &[], "work", exists);
            assert_eq!(problems.len(), 1, "`{follow_ups}`: {problems:?}");
            assert!(problems[0].contains("names no issue; write `None`, or"), "{problems:?}");
        }
    }

    // Goal: only the attribution lines at the end of a section are left out; one in the middle
    // is content and is checked like any other line.
    #[test]
    fn attribution_in_the_middle_is_content() {
        let middle = "🤖 Generated with #3";
        assert_eq!(
            section(&format!("## A\n- #12\n{middle}\n- #13\n"), "## A"),
            Some(paths(&["- #12", middle, "- #13"]))
        );
        let follow_ups = format!("- #12\n{middle}\n- #13");
        let problems = check_body(&body_with_follow_ups(&follow_ups), &[], "work", exists);
        assert_eq!(problems.len(), 2, "#3 and #13 are checked: {problems:?}");
        assert!(problems[0].contains("#3, listed under"), "{problems:?}");
        assert!(problems[1].contains("#13, listed under"), "{problems:?}");
        let after_none = format!("None.\n{middle}\n---\nA footer.");
        let none_problems = check_body(&body_with_follow_ups(&after_none), &[], "work", exists);
        assert_eq!(none_problems.len(), 1, "{none_problems:?}");
        let expected = format!("says `None` and then `{middle}`;");
        assert!(none_problems[0].contains(&expected), "{none_problems:?}");
    }

    // Goal: `None` followed by more text is reported as such, not as a section that names no
    // issue, and the message names the extra line. A `---` line between them hides nothing,
    // and an issue after `None` does not make the section pass, not even a follow-up issue.
    #[test]
    fn none_with_more_text_names_the_extra_line() {
        let cases = [
            ("None\nNone", "None"),
            ("None.\n\nA footer.", "A footer."),
            ("None.\n---\nSee #99", "See #99"),
            ("None.\n\n---\n\n- #12", "- #12"),
        ];
        for (follow_ups, extra) in cases {
            let problems = check_body(&body_with_follow_ups(follow_ups), &[], "work", exists);
            assert_eq!(problems.len(), 1, "`{follow_ups}`: {problems:?}");
            let expected = format!("says `None` and then `{extra}`;");
            assert!(problems[0].contains(&expected), "{problems:?}");
        }
    }

    // Goal: anything else must name follow-up issues as `#<number>`. Method: text without a
    // number, a number only inside a comment or with a leading zero, and numbers that are not
    // follow-up issues, each reported once.
    #[test]
    fn follow_ups_must_name_follow_up_issues() {
        let no_issue = [
            "",
            "N/A",
            "None so far, but see below.",
            "<!-- #12 -->",
            "#",
            "#012",
            "issue 12",
            "### Details",
        ];
        for follow_ups in no_issue {
            let problems = check_body(&body_with_follow_ups(follow_ups), &[], "work", exists);
            assert_eq!(problems.len(), 1, "`{follow_ups}`: {problems:?}");
            assert!(problems[0].contains("names no issue; write `None`, or"), "{problems:?}");
        }
        for follow_ups in ["See #12.", "- #12 (the loop rule)", "#12#12"] {
            let body = body_with_follow_ups(follow_ups);
            assert_eq!(check_body(&body, &[], "work", exists), Vec::<String>::new(), "{body}");
        }
        let follow_ups = "- #13\n- #12, #11\n- #13 again\n";
        let problems = check_body(&body_with_follow_ups(follow_ups), &[], "work", exists);
        assert_eq!(problems.len(), 2, "one per wrong issue, each once: {problems:?}");
        assert!(problems[0].contains("#11, listed under"), "{problems:?}");
        assert!(problems[1].contains("#13, listed under"), "{problems:?}");
        assert!(problems[1].contains("is not a follow-up issue: it is not #12;"), "{problems:?}");
    }

    // Goal: on a claimed issue's branch the description closes that issue, with any of GitHub's
    // closing words, outside a comment; other branches need nothing.
    #[test]
    fn issue_branches_close_their_issue() {
        let closes = |text: &str, branch: &str| {
            let body = format!("{text}\n\n{}", body_with_follow_ups("None"));
            check_body(&body, &[], branch, exists).is_empty()
        };
        for text in ["Fixes #12", "fixes: #12.", "Work.\nCloses #12)", "RESOLVED #12,", "fix #12"] {
            assert!(closes(text, "task/issue-12"), "`{text}` closes #12");
        }
        for text in ["", "#12", "Fixes 12", "Fixes #120", "<!-- Fixes #12 -->", "Fixes  issue #12"]
        {
            assert!(!closes(text, "task/issue-12"), "`{text}` does not close #12");
        }
        for branch in ["work", "task/a", "task/issue-x", "issue-12", "task/issue-012"] {
            assert!(closes("", branch), "`{branch}` is not a claimed issue's branch");
        }
        let body = body_with_follow_ups("None");
        let problems = check_body(&body, &[], "task/issue-12", exists);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("add the line `Fixes #12` (ADR-0021)"), "{problems:?}");
    }

    // Goal: a listed issue must exist, be an issue and come from the bot or a code owner.
    #[test]
    fn listed_issues_must_be_trusted_issues() {
        let trust = Trust::from_codeowners("/xtask/ @owner\n");
        assert_eq!(listed_issue_problem(&Issues, &trust, 1), None);
        let problem = |number| listed_issue_problem(&Issues, &trust, number).expect("a problem");
        assert!(problem(2).contains("is a pull request"), "{}", problem(2));
        assert!(problem(3).contains("neither the bot nor a code owner"), "{}", problem(3));
        assert!(problem(4).contains("cannot look it up: not found"), "{}", problem(4));
    }

    // Goal: a changed protected path must be listed, by itself or by a directory above it that
    // ends in `/`. A name that merely contains or starts the path does not count.
    #[test]
    fn protected_paths_must_be_listed() {
        let protected = paths(&["Cargo.toml", "xtask/src/hook.rs"]);
        let is_complete = |listed: &str| {
            check_body(&body_with_protected(listed), &protected, "work", exists).is_empty()
        };
        assert!(is_complete("- `Cargo.toml`: x\n- `xtask/src/hook.rs`: y"), "each path itself");
        assert!(is_complete("Cargo.toml, xtask/."), "a directory, then a full stop");
        assert!(is_complete("Changed (Cargo.toml) and **xtask/src/**."), "inside punctuation");
        assert!(!is_complete("None"), "`None` lists nothing");
        assert!(!is_complete("Cargo.toml and xtask"), "a directory needs its trailing slash");
        assert!(!is_complete("Cargo.toml and xtask/src/hook.rs.bak"), "a longer name");
        assert!(!is_complete("xtask/Cargo.toml and xtask/"), "the same name elsewhere");
        assert!(!is_complete("Cargo.toml and task/"), "a directory that only ends the same");
        assert!(!is_complete("Cargo.toml <!-- xtask/ -->"), "a comment lists nothing");

        let problems = check_body(&body_with_protected("xtask/"), &protected, "work", exists);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0]
                .starts_with("error[pr-body]: the protected path `Cargo.toml` is not listed"),
            "{problems:?}"
        );
    }

    // Goal: without protected changes the protected-files section is free: `None`, anything
    // else, or no section at all.
    #[test]
    fn protected_section_is_free_without_protected_changes() {
        for body in
            [body_with_protected("None"), body_with_protected(""), body_with_follow_ups("None")]
        {
            assert_eq!(check_body(&body, &[], "work", exists), Vec::<String>::new(), "{body}");
        }
    }

    // Goal: the base ref defaults to `origin/main`; anything but `--base <ref>` is an error, and
    // so is a ref that git would read as an option.
    #[test]
    fn base_option_is_read() {
        assert_eq!(base_of(&[]), Ok("origin/main"));
        assert_eq!(base_of(&["--base", "main"]), Ok("main"));
        for options in [&["--base"][..], &["--base", ""], &["--base", "--output=x"], &["main"]] {
            assert!(base_of(options).is_err(), "{options:?} is not `--base <ref>`");
        }
        assert!(base_of(&["--base", "main", "more"]).is_err(), "one ref only");
    }
}
