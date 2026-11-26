//! The command line of the ratchet (ADR-0022).
//!
//! ```text
//! cargo xtask ratchet [observe | judge] --base <dir> --head <dir> [options]
//!
//!   observe:  --out <file>                      write what the head's tools print
//!   judge:    --observations <file>             judge what they printed
//!   neither:  observe and judge in one process
//!
//!   (--pr <number> [--reviewer <app login>] | --no-review)    judge and neither
//!   [--no-mutants] [--work <dir>]                             all
//!   [--head-xtask <file>] [--seed <number>]                   observe and neither
//! ```
//!
//! `--base` must be the checkout whose ratchet runs, because the base's tidy judges in-process.

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
                     <file>] --base <dir> --head <dir> [(--pr <number> [--reviewer <login>]) | \
                     --no-review] [--no-mutants] [--head-xtask <file>] [--seed <number>] \
                     [--work <dir>]";

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
    reviewer: Option<String>,
    mutants: bool,
    head_xtask: Option<PathBuf>,
    seed: Option<u64>,
    work: Option<PathBuf>,
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
            "--reviewer" => reviewer = Some(value()?.to_owned()).filter(|login| !login.is_empty()),
            "--no-review" => no_review = true,
            "--no-mutants" => mutants = false,
            "--head-xtask" => head_xtask = Some(PathBuf::from(value()?)),
            "--seed" => seed = Some(number::<u64>("--seed", value()?)?),
            "--work" => work = Some(PathBuf::from(value()?)),
            "--out" if subcommand == "observe" => file = Some(PathBuf::from(value()?)),
            "--observations" if subcommand == "judge" => file = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown option `{other}`; {USAGE}")),
        }
    }
    let mode = match (subcommand, file) {
        ("observe", Some(file)) => Mode::Observe(file),
        ("judge", Some(file)) => Mode::Judge(file),
        ("", None) => Mode::Both,
        _ => return Err(format!("`observe` needs `--out` and `judge` `--observations`; {USAGE}")),
    };
    if matches!(mode, Mode::Observe(_)) && (pull.is_some() || no_review) {
        return Err(format!("`observe` reads no reviews; {USAGE}"));
    }
    if !matches!(mode, Mode::Observe(_)) && pull.is_some() == no_review {
        return Err(format!("pass exactly one of `--pr <number>` and `--no-review`; {USAGE}"));
    }
    let (Some(base), Some(head)) = (base, head) else {
        return Err(format!("`--base` and `--head` are required; {USAGE}"));
    };
    Ok(Arguments { mode, base, head, pull, reviewer, mutants, head_xtask, seed, work })
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
            } else if verdict.unproven.is_empty() {
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
    let work = arguments.work.clone().unwrap_or_else(|| head.join("target/ratchet"));
    let github = GhReviews::new(&head);
    let tools = Tools {
        head_xtask: arguments.head_xtask.clone(),
        cargo: OsString::from("cargo"),
        work: std::path::absolute(&work).map_err(|error| format!("`--work`: {error}"))?,
        mutants: arguments.mutants,
        review: arguments.pull.map(|pull| ReviewSource {
            reviews: &github,
            pull,
            reviewer: arguments.reviewer.clone(),
        }),
        seed: arguments.seed,
    };
    match &arguments.mode {
        Mode::Observe(out) => {
            let observed = super::observe(&base, &head, &tools)?;
            fs::write(out, observed.to_json())
                .map_err(|error| format!("cannot write {}: {error}", out.display()))?;
            Ok(None)
        }
        Mode::Judge(file) => {
            super::judge(&base, &head, &Observations::read(file)?, &tools).map(Some)
        }
        Mode::Both => super::check(&base, &head, &tools).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, parse};

    // Goal: the options parse into the arguments for each mode, and a missing value, an unknown
    // option, a bad number, a mode without its file, or neither or both of `--pr` and
    // `--no-review` where reviews are judged, are errors.
    #[test]
    fn options_are_parsed() {
        let both = parse(&[
            "--base",
            "b",
            "--head",
            "h",
            "--pr",
            "12",
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
        ])
        .unwrap();
        assert_eq!((judge.mode, judge.reviewer), (Mode::Judge("o.json".into()), None));
        let empty_reviewer =
            parse(&["--base", "b", "--head", "h", "--pr", "3", "--reviewer", ""]).unwrap();
        assert_eq!(empty_reviewer.reviewer, None, "an unset repository variable is no reviewer");
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
        ];
        for options in errors {
            assert!(parse(&options).is_err(), "{options:?}");
        }
    }
}
