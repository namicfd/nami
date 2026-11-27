//! The command line of the ratchet (ADR-0022).
//!
//! ```text
//! cargo xtask ratchet [observe | judge] --base <dir> --head <dir> [options]
//!
//!   observe:  --out <file>                      write what the head's tools print
//!   judge:    --observations <file>             judge what they printed
//!             [--carried <dir>]                 mutation evidence of an earlier observation
//!   neither:  observe and judge in one process
//!
//!   (--pr <number> --pr-head <sha> [--reviewer <app login>]
//!    | --no-review)                                           judge and neither
//!   [--commits <base>..<head>]                                judge and neither
//!   [--pr-head <sha>] [--commit <sha>]                        observe
//!   [--no-mutants] [--work <dir>]                             all
//!   [--head-xtask <file>] [--seed <number>]                   observe and neither
//! ```
//!
//! `--base` must be the checkout whose ratchet runs, because the base's tidy judges in-process.
//! `--pr-head` is the pull request's head commit that was judged: reviews count only on it,
//! and the judge gives no verdict if the pull request has moved on. `--commits` names the
//! commits whose trees are judged, for the summary; its head must be the commit that `observe`
//! recorded with `--commit`, if it did. Without `--seed`, the placements are drawn
//! from a seed of the head's content, so that a re-run draws the same ones. `--carried` names a
//! directory with a pull request's `observations.json` and the trees `base/` and `head/` it
//! observed; a merge group's judge reads its mutation evidence when the rule-path diffs are
//! identical, and gives no verdict otherwise ([`super::carry`]).

use super::observations::Observations;
use super::review::GhReviews;
use super::{ReviewSource, Tools, Verdict};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The exit code for "no verdict".
const EXIT_NO_VERDICT: u8 = 2;

const USAGE: &str = "usage: cargo xtask ratchet [observe --out <file> | judge --observations \
                     <file>] --base <dir> --head <dir> [(--pr <number> --pr-head <sha> \
                     [--reviewer <login>]) | --no-review] [--commits <base>..<head>] \
                     [--commit <sha>] [--carried <dir>] \
                     [--no-mutants] [--head-xtask <file>] [--seed <number>] [--work <dir>]";

/// True for a full commit hash.
fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|character| character.is_ascii_hexdigit())
}

/// What to do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    Both,
    Observe(PathBuf),
    Judge(PathBuf),
}

/// The parsed command line.
#[derive(Debug)]
struct Arguments {
    mode: Mode,
    base: PathBuf,
    head: PathBuf,
    pull: Option<u32>,
    pull_head: Option<String>,
    reviewer: Option<String>,
    commits: Option<String>,
    commit: Option<String>,
    mutants: bool,
    head_xtask: Option<PathBuf>,
    seed: Option<u64>,
    work: Option<PathBuf>,
    /// A directory with a pull request's observation and the trees it observed, whose mutation
    /// evidence a merge group's judge reads ([`super::carry`]).
    carried: Option<PathBuf>,
}

fn number<T: std::str::FromStr>(option: &str, text: &str) -> Result<T, String> {
    text.parse().map_err(|_| format!("`{option}` takes a number, not `{text}`; {USAGE}"))
}

fn parse(options: &[&str]) -> Result<Arguments, String> {
    let (subcommand, options) = match options.first() {
        Some(&"observe") => ("observe", options.get(1..).unwrap_or_default()),
        Some(&"judge") => ("judge", options.get(1..).unwrap_or_default()),
        _ => ("", options),
    };
    let mut rest = options.iter();
    let (mut base, mut head, mut pull, mut reviewer, mut no_review) =
        (None, None, None, None, false);
    let (mut pull_head, mut commits, mut commit, mut carried) = (None, None, None, None);
    let (mut mutants, mut head_xtask, mut seed, mut work, mut file) =
        (true, None, None, None, None);
    for _ in 0..options.len() {
        let Some(option) = rest.next() else {
            break;
        };
        let mut value = || rest.next().copied().ok_or(format!("`{option}` needs a value; {USAGE}"));
        match *option {
            "--base" => base = Some(PathBuf::from(value()?)),
            "--head" => head = Some(PathBuf::from(value()?)),
            "--pr" => pull = Some(number::<u32>("--pr", value()?)?).filter(|number| *number > 0),
            "--pr-head" => pull_head = Some(value()?.to_owned()),
            "--commits" => commits = Some(value()?.to_owned()),
            "--commit" => commit = Some(value()?.to_owned()),
            "--reviewer" => reviewer = Some(value()?.to_owned()).filter(|login| !login.is_empty()),
            "--no-review" => no_review = true,
            "--no-mutants" => mutants = false,
            "--head-xtask" => head_xtask = Some(PathBuf::from(value()?)),
            "--seed" => seed = Some(number::<u64>("--seed", value()?)?),
            "--work" => work = Some(PathBuf::from(value()?)),
            "--out" if subcommand == "observe" => file = Some(PathBuf::from(value()?)),
            "--observations" if subcommand == "judge" => file = Some(PathBuf::from(value()?)),
            "--carried" if subcommand == "judge" => carried = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown option `{other}`; {USAGE}")),
        }
    }
    let mode = mode_of(subcommand, file, pull.is_some(), no_review)?;
    check_commits(pull.is_some(), pull_head.as_deref(), commits.as_deref())?;
    if !commit.as_deref().is_none_or(is_commit) {
        return Err(format!("`--commit` takes a full commit; {USAGE}"));
    }
    let (Some(base), Some(head)) = (base, head) else {
        return Err(format!("`--base` and `--head` are required; {USAGE}"));
    };
    Ok(Arguments {
        mode,
        base,
        head,
        pull,
        pull_head,
        reviewer,
        commits,
        commit,
        mutants,
        head_xtask,
        seed,
        work,
        carried,
    })
}

/// The mode, from the subcommand and its file, checked against the review options.
fn mode_of(
    subcommand: &str,
    file: Option<PathBuf>,
    has_pull: bool,
    no_review: bool,
) -> Result<Mode, String> {
    let mode = match (subcommand, file) {
        ("observe", Some(file)) => Mode::Observe(file),
        ("judge", Some(file)) => Mode::Judge(file),
        ("", None) => Mode::Both,
        _ => return Err(format!("`observe` needs `--out` and `judge` `--observations`; {USAGE}")),
    };
    if matches!(mode, Mode::Observe(_)) && (has_pull || no_review) {
        return Err(format!("`observe` reads no reviews; {USAGE}"));
    }
    if !matches!(mode, Mode::Observe(_)) && has_pull == no_review {
        return Err(format!("pass exactly one of `--pr <number>` and `--no-review`; {USAGE}"));
    }
    Ok(mode)
}

/// Checks the commits given: `--pr-head` with `--pr`, a full commit, and `--commits` two full
/// commits.
fn check_commits(
    has_pull: bool,
    pull_head: Option<&str>,
    commits: Option<&str>,
) -> Result<(), String> {
    if (has_pull && pull_head.is_none()) || !pull_head.is_none_or(is_commit) {
        return Err(format!("`--pr` needs `--pr-head` with the full head commit; {USAGE}"));
    }
    let is_range = |range: &str| {
        range.split_once("..").is_some_and(|(from, to)| is_commit(from) && is_commit(to))
    };
    if !commits.is_none_or(is_range) {
        return Err(format!("`--commits` takes `<base>..<head>`, two full commits; {USAGE}"));
    }
    Ok(())
}

/// The work directory: `--work`, or one in the system's temporary directory named after the
/// head. Never inside the head, whose cargo configuration and toolchain file would then apply
/// to the base's runs.
fn work_directory(work: Option<&Path>, head: &Path) -> Result<PathBuf, String> {
    let chosen = if let Some(work) = work {
        std::path::absolute(work).map_err(|error| format!("`--work`: {error}"))?
    } else {
        let bytes = head.as_os_str().as_encoded_bytes();
        let name = crate::dylint::fnv1a(crate::dylint::FNV_OFFSET, bytes);
        std::env::temp_dir().join(format!("nami-ratchet-{name:016x}"))
    };
    if chosen.starts_with(head) {
        return Err(format!(
            "the work directory {} is inside the head; pass a `--work` outside it",
            chosen.display()
        ));
    }
    Ok(chosen)
}

/// Resolves a directory argument to an absolute, canonical path.
fn directory(path: &Path, what: &str) -> Result<PathBuf, String> {
    let canonical =
        fs::canonicalize(path).map_err(|error| format!("{what} `{}`: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{what} `{}` is not a directory", path.display()));
    }
    Ok(canonical)
}

/// `cargo xtask ratchet`: see the module documentation of [`super`] for the protocol.
pub fn run(options: &[&str]) -> ExitCode {
    match run_checked(options) {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(verdict)) => {
            for finding in &verdict.findings {
                eprintln!("{finding}");
            }
            print!("{}", verdict.summary());
            if !verdict.findings.is_empty() {
                ExitCode::FAILURE
            } else if verdict.unproven.is_empty() && verdict.failed.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_NO_VERDICT)
            }
        }
        Err(problem) => {
            eprintln!("ratchet: no verdict: {problem}");
            println!("ratchet: no verdict: {problem}");
            ExitCode::from(EXIT_NO_VERDICT)
        }
    }
}

/// Runs the mode; `None` for `observe`, which judges nothing.
fn run_checked(options: &[&str]) -> Result<Option<Verdict>, String> {
    let arguments = parse(options)?;
    let base = directory(&arguments.base, "the base")?;
    let head = directory(&arguments.head, "the head")?;
    let own = directory(&crate::workspace_root(), "this checkout")?;
    if base != own {
        return Err(format!(
            "`--base` must be the checkout whose ratchet runs, {}, because the base's tidy \
             judges in-process; run `cargo xtask ratchet` inside the base",
            own.display()
        ));
    }
    if base == head {
        return Err("`--base` and `--head` are the same tree".to_owned());
    }
    let work = work_directory(arguments.work.as_deref(), &head)?;
    let github = GhReviews::new(&head);
    // Carried evidence that cannot be read fails its step only, and the observations are
    // judged all the same (review of #57, round 13).
    let carried = arguments.carried.as_deref().map(super::carry::Carried::read);
    let tools = Tools {
        head_xtask: arguments.head_xtask.clone(),
        cargo: OsString::from("cargo"),
        work: std::path::absolute(&work).map_err(|error| format!("`--work`: {error}"))?,
        mutants: arguments.mutants,
        review: arguments.pull.zip(arguments.pull_head.clone()).map(|(pull, pull_head)| {
            ReviewSource {
                reviews: &github,
                pull,
                head: pull_head,
                reviewer: arguments.reviewer.clone(),
            }
        }),
        seed: arguments.seed,
        commits: arguments.commits.clone(),
        pr_head: arguments.pull_head.clone(),
        commit: arguments.commit.clone(),
        carried,
        faults: super::Faults::default(),
    };
    match &arguments.mode {
        Mode::Observe(out) => {
            let observed = super::observe(&base, &head, &tools)?;
            fs::write(out, observed.to_json())
                .map_err(|error| format!("cannot write {}: {error}", out.display()))?;
            Ok(None)
        }
        Mode::Judge(file) => {
            let observed = Observations::read(file);
            let observed = observed.as_ref().map_err(String::as_str);
            let judged = super::judge_with(&base, &head, observed, &tools);
            super::settled(&base, &tools, judged).map(Some)
        }
        Mode::Both => {
            super::settled(&base, &tools, super::check_all(&base, &head, &tools)).map(Some)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, parse};

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    // Goal: the options parse into the arguments for each mode.
    #[test]
    fn options_are_parsed() {
        let range = format!("{SHA}..{SHA}");
        let both = parse(&[
            "--base",
            "b",
            "--head",
            "h",
            "--pr",
            "12",
            "--pr-head",
            SHA,
            "--commits",
            &range,
            "--reviewer",
            "r[bot]",
            "--no-mutants",
            "--seed",
            "5",
        ])
        .unwrap();
        assert_eq!(
            (both.pull, both.reviewer.as_deref(), both.mutants, both.seed),
            (Some(12), Some("r[bot]"), false, Some(5))
        );
        assert_eq!(both.mode, Mode::Both);
        assert_eq!((both.pull_head.as_deref(), both.commits), (Some(SHA), Some(range.clone())));
        let observe = parse(&["observe", "--base", "b", "--head", "h", "--out", "o.json"]).unwrap();
        assert_eq!(observe.mode, Mode::Observe("o.json".into()));
        let judge = parse(&[
            "judge",
            "--base",
            "b",
            "--head",
            "h",
            "--observations",
            "o.json",
            "--pr",
            "3",
            "--pr-head",
            SHA,
        ])
        .unwrap();
        assert_eq!((judge.mode, judge.reviewer), (Mode::Judge("o.json".into()), None));
        let empty_reviewer =
            parse(&["--base", "b", "--head", "h", "--pr", "3", "--pr-head", SHA, "--reviewer", ""])
                .unwrap();
        assert_eq!(empty_reviewer.reviewer, None, "an unset repository variable is no reviewer");
    }

    // Goal: `observe` takes the commits it records.
    #[test]
    fn observe_records_its_commits() {
        let recorded = parse(&[
            "observe",
            "--base",
            "b",
            "--head",
            "h",
            "--out",
            "o",
            "--pr-head",
            SHA,
            "--commit",
            SHA,
        ])
        .unwrap();
        assert_eq!(
            (recorded.pull_head.as_deref(), recorded.commit.as_deref()),
            (Some(SHA), Some(SHA))
        );
    }

    // Goal: the work directory is never inside the head, by default or by choice.
    #[test]
    fn the_work_directory_is_outside_the_head() {
        let head = std::path::Path::new("/tmp/nami-head");
        let default = super::work_directory(None, head).unwrap();
        assert!(!default.starts_with(head) && default.starts_with(std::env::temp_dir()));
        assert!(super::work_directory(Some(&head.join("target")), head).is_err());
        assert_eq!(super::work_directory(Some(std::path::Path::new("/w")), head), Ok("/w".into()));
    }

    // Goal: a missing value, an unknown option, a bad number, a mode without its file, neither
    // or both of `--pr` and `--no-review` where reviews are judged, `--pr` without a full
    // `--pr-head`, and a `--commits` that is not two full commits, are errors.
    #[test]
    fn bad_options_are_errors() {
        let errors = [
            vec!["--base", "b", "--head", "h"],
            vec!["--base", "b", "--head", "h", "--pr", "1", "--no-review"],
            vec!["--base", "b", "--head", "h", "--pr", "x"],
            vec!["--base", "b", "--no-review"],
            vec!["--base", "b", "--head", "h", "--no-review", "--fast"],
            vec!["--base"],
            vec!["observe", "--base", "b", "--head", "h"],
            vec!["observe", "--base", "b", "--head", "h", "--out", "o", "--pr", "1"],
            vec!["judge", "--base", "b", "--head", "h", "--out", "o", "--no-review"],
            vec!["--base", "b", "--head", "h", "--no-review", "--seed", "-1"],
            vec!["--base", "b", "--head", "h", "--pr", "1"],
            vec!["--base", "b", "--head", "h", "--pr", "1", "--pr-head", "abc"],
            vec!["--base", "b", "--head", "h", "--no-review", "--commit", "abc"],
            vec!["--base", "b", "--head", "h", "--no-review", "--commits", SHA],
        ];
        for options in errors {
            assert!(parse(&options).is_err(), "{options:?}");
        }
    }
}
