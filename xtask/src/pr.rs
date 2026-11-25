//! `cargo xtask protected` and `cargo xtask pr-body`: what a pull request must say about itself.
//!
//! `AGENTS.md` requires a PR description to list every protected file that the PR changes, and
//! to record the guardrail gaps that the work ran into. These commands check both, so that the
//! rule does not depend on anyone remembering it.
//!
//! - `protected` prints the changed paths that `.github/CODEOWNERS` protects, one per line.
//! - `pr-body` checks the description in the environment variable `PR_BODY`. Every problem reads
//!   `error[pr-body]: what; fix`, like a tidy violation.

use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

/// The file that lists the protected paths, relative to the workspace root.
const CODEOWNERS_FILE: &str = ".github/CODEOWNERS";

/// The ref that a PR is compared with unless `--base` names another.
const BASE_DEFAULT: &str = "origin/main";

/// The section of a PR description that lists the protected files the PR changes.
const HEADING_PROTECTED: &str = "## Protected files";

/// The section of a PR description that records guardrail gaps.
const HEADING_GAPS: &str = "## Guardrail gaps";

/// Where the learnings that record guardrail gaps live. Each is one Markdown file.
const LEARNINGS_DIRECTORY: &str = "docs/learnings/";

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
/// HTML comments and blank lines are left out. The section ends at the next `#` or `##` heading.
#[must_use]
pub fn section(body: &str, heading: &str) -> Option<Vec<String>> {
    let visible = without_comments(body);
    let mut lines = visible.lines().map(str::trim).skip_while(|line| *line != heading);
    let _heading = lines.next()?;
    let is_next_section = |line: &&str| line.starts_with("# ") || line.starts_with("## ");
    let content = lines.take_while(|line| !is_next_section(line)).filter(|line| !line.is_empty());
    Some(content.map(str::to_owned).collect())
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
/// `protected` holds the protected paths that the PR changes. `is_file` says whether a path
/// exists in the tree; it is asked about the learnings that the description names.
#[must_use]
pub fn check_body(body: &str, protected: &[String], is_file: impl Fn(&str) -> bool) -> Vec<String> {
    let mut problems = guardrail_gap_problems(section(body, HEADING_GAPS).as_deref(), is_file);
    let listed = section(body, HEADING_PROTECTED);
    problems.extend(protected_file_problems(listed.as_deref(), protected));
    problems.iter().map(|problem| format!("error[pr-body]: {problem}")).collect()
}

fn guardrail_gap_problems(
    section: Option<&[String]>,
    is_file: impl Fn(&str) -> bool,
) -> Vec<String> {
    let how =
        format!("write `None`, or name the `{LEARNINGS_DIRECTORY}<file>.md` that records each gap");
    let Some(lines) = section else {
        return vec![format!("the section `{HEADING_GAPS}` is missing; add it and {how}")];
    };
    if let [line] = lines
        && is_none(line)
    {
        return Vec::new();
    }
    let learnings = learning_paths(&lines.join("\n"));
    if learnings.is_empty() {
        return vec![format!("the section `{HEADING_GAPS}` names no learning; {how}")];
    }
    let missing = learnings.iter().filter(|path| !is_file(path));
    missing
        .map(|path| {
            format!(
                "`{path}`, named under `{HEADING_GAPS}`, does not exist; add the file to this \
                 PR, or correct the path"
            )
        })
        .collect()
}

/// True for `None` in any case, with or without a full stop.
fn is_none(line: &str) -> bool {
    let word = line.trim();
    word.strip_suffix('.').unwrap_or(word).eq_ignore_ascii_case("none")
}

/// Every `docs/learnings/<file>.md` path that `text` names, sorted and without duplicates.
fn learning_paths(text: &str) -> Vec<String> {
    let is_name_character =
        |character: &char| character.is_ascii_alphanumeric() || ['.', '_', '-'].contains(character);
    let mut paths: Vec<String> = text
        .split(LEARNINGS_DIRECTORY)
        .skip(1)
        .filter_map(|after| {
            let name: String = after.chars().take_while(is_name_character).collect();
            // A full stop that ends the sentence is not part of the name.
            let name = name.trim_end_matches('.');
            let is_markdown =
                Path::new(name).extension().is_some_and(|extension| extension == "md");
            is_markdown.then(|| format!("{LEARNINGS_DIRECTORY}{name}"))
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths
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
pub fn run_body(options: &[&str]) -> ExitCode {
    let body = std::env::var("PR_BODY").ok();
    let problems = body_problems(&crate::workspace_root(), options, body.as_deref());
    for problem in &problems {
        eprintln!("{problem}");
    }
    if problems.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// The problems of the PR description `body` for the checkout at `root`, one line each.
///
/// `options` are the options of `pr-body`. `body` is `None` if `PR_BODY` is not set. Whatever
/// keeps the check from running is a problem too, so that a check that cannot run never passes.
#[must_use]
pub fn body_problems(root: &Path, options: &[&str], body: Option<&str>) -> Vec<String> {
    let Some(body) = body else {
        return vec![
            "error[pr-body]: the environment variable PR_BODY is not set; set it to the PR \
             description, for example `PR_BODY=\"$(gh pr view --json body --jq .body)\" cargo \
             xtask pr-body`"
                .to_owned(),
        ];
    };
    match changed_protected(root, options) {
        Ok(protected) => check_body(body, &protected, |path| root.join(path).is_file()),
        Err(error) => vec![format!("error[pr-body]: {error}")],
    }
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
    use super::{Pattern, base_of, check_body, parse_codeowners, protected_paths, section};

    /// A description that passes when `xtask/src/hook.rs` and `justfile` are the protected
    /// changes and `docs/learnings/stop-gate.md` exists.
    const BODY: &str = "## Summary\n\nFix the stop hook.\n\n## Rule changes\n\nNone\n\n\
                        ## Protected files\n\n- `xtask/`: the hook lives there.\n\
                        - justfile: new recipe.\n\n## Guardrail gaps\n\n\
                        - docs/learnings/stop-gate.md\n";

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn body_with_gaps(gaps: &str) -> String {
        format!("## Summary\n\nText.\n\n## Guardrail gaps\n{gaps}\n")
    }

    fn body_with_protected(listed: &str) -> String {
        format!("## Protected files\n{listed}\n\n## Guardrail gaps\n\nNone\n")
    }

    fn exists(path: &str) -> bool {
        path == "docs/learnings/stop-gate.md"
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
    // blank lines and comments; a heading inside a comment does not count.
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
    }

    // Goal: a complete description has no problems, and each problem is one line in the
    // documented form.
    #[test]
    fn complete_body_passes() {
        let protected = paths(&["justfile", "xtask/src/hook.rs"]);
        assert_eq!(check_body(BODY, &protected, exists), Vec::<String>::new());
        let problems = check_body("", &protected, exists);
        assert_eq!(problems.len(), 2, "one per missing section: {problems:?}");
        for problem in &problems {
            assert!(problem.starts_with("error[pr-body]: the section `## "), "{problem}");
            assert!(problem.contains(" is missing"), "{problem}");
            assert!(problem.contains("; add it and "), "a problem says how to fix it: {problem}");
            assert!(!problem.contains('\n'), "one line per problem: {problem}");
        }
    }

    // Goal: the guardrail-gaps section accepts `None` in any case with an optional full stop.
    #[test]
    fn guardrail_gaps_may_be_none() {
        for none in ["None", "none", "NONE.", "  None.  ", "<!-- say why -->\n\nNone\n"] {
            let problems = check_body(&body_with_gaps(none), &[], exists);
            assert_eq!(problems, Vec::<String>::new(), "`{none}` means no gaps");
        }
    }

    // Goal: anything else must name a learning that exists. Method: text without a path, a path
    // only inside a comment, a path of the wrong kind, `None` with more text, and missing files.
    #[test]
    fn guardrail_gaps_must_name_existing_learnings() {
        let no_learning = [
            "",
            "N/A",
            "None so far, but see below.",
            "None\nNone",
            "<!-- docs/learnings/stop-gate.md -->",
            "docs/learnings/",
            "docs/learnings/stop-gate.txt",
            "docs/learnings/md",
            "learnings/stop-gate.md",
        ];
        for gaps in no_learning {
            let problems = check_body(&body_with_gaps(gaps), &[], exists);
            assert_eq!(problems.len(), 1, "`{gaps}`: {problems:?}");
            assert!(problems[0].contains("names no learning; write `None`, or"), "{problems:?}");
        }
        for gaps in ["See docs/learnings/stop-gate.md.", "[gate](docs/learnings/stop-gate.md)"] {
            assert_eq!(check_body(&body_with_gaps(gaps), &[], exists), Vec::<String>::new());
        }
        let gaps = "- docs/learnings/stop-gate.md\n- `docs/learnings/b.md`, docs/learnings/a.md\n\
                    - docs/learnings/a.md again\n";
        let problems = check_body(&body_with_gaps(gaps), &[], exists);
        assert_eq!(problems.len(), 2, "one per missing file, each once: {problems:?}");
        assert!(problems[0].contains("`docs/learnings/a.md`, named under"), "{problems:?}");
        assert!(problems[1].contains("`docs/learnings/b.md`, named under"), "{problems:?}");
        assert!(problems[1].contains("does not exist; add the file"), "{problems:?}");
    }

    // Goal: a changed protected path must be listed, by itself or by a directory above it that
    // ends in `/`. A name that merely contains or starts the path does not count.
    #[test]
    fn protected_paths_must_be_listed() {
        let protected = paths(&["Cargo.toml", "xtask/src/hook.rs"]);
        let is_complete =
            |listed: &str| check_body(&body_with_protected(listed), &protected, exists).is_empty();
        assert!(is_complete("- `Cargo.toml`: x\n- `xtask/src/hook.rs`: y"), "each path itself");
        assert!(is_complete("Cargo.toml, xtask/."), "a directory, then a full stop");
        assert!(is_complete("Changed (Cargo.toml) and **xtask/src/**."), "inside punctuation");
        assert!(!is_complete("None"), "`None` lists nothing");
        assert!(!is_complete("Cargo.toml and xtask"), "a directory needs its trailing slash");
        assert!(!is_complete("Cargo.toml and xtask/src/hook.rs.bak"), "a longer name");
        assert!(!is_complete("xtask/Cargo.toml and xtask/"), "the same name elsewhere");
        assert!(!is_complete("Cargo.toml and task/"), "a directory that only ends the same");
        assert!(!is_complete("Cargo.toml <!-- xtask/ -->"), "a comment lists nothing");

        let problems = check_body(&body_with_protected("xtask/"), &protected, exists);
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
        for body in [body_with_protected("None"), body_with_protected(""), body_with_gaps("None")] {
            assert_eq!(check_body(&body, &[], exists), Vec::<String>::new(), "{body}");
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
