//! The tidy corpus, and the tidy half of the end-to-end check.
//!
//! Every fixture under the base's `xtask/fixtures/tidy/` is a case written down. For each one
//! the base's tidy, which is the code running here, says what it reports: the `(rule, line)`
//! pairs. The head's tidy then runs as a separate program on the same text, planted at the
//! fixture's virtual path in a scratch copy of the head tree:
//!
//! - **corpus:** `tidy --file` on the planted file must report every pair that the base reports
//!   on a firing fixture, and nothing that the base does not report on a passing one;
//! - **end to end:** `tidy --root` on the whole scratch tree must report every pair of a firing
//!   fixture at the planted path. This catches a head whose walk no longer reaches the file.
//!
//! Each case is planted at its virtual path and at paths drawn per run from the head tree
//! (`plant`). The head's tools run in one job ([`observe`]); the base judges what they printed
//! in another, reading its fixtures from its own checkout ([`judge`]).
//!
//! The head may change or delete its own copy of a fixture; the base's cases still hold.

use super::observations::{Observations, Run};
use super::plant::{self, Rng};
use super::tree::{Scratch, is_plain_relative};
use super::{Check, Finding};
use crate::tidy::Scope;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Where the tidy fixtures live, relative to the root.
pub const FIXTURE_DIRECTORY: &str = "xtask/fixtures/tidy";

/// The name endings of a firing and a passing fixture.
const FIRES_SUFFIX: &str = ".fires.fixture";
const PASSES_SUFFIX: &str = ".passes.fixture";

/// The exit code of `tidy --file` for a file that breaks a rule (`tidy::cli::EXIT_VIOLATIONS`).
const EXIT_FILE_VIOLATIONS: i32 = 2;

/// The exit code of `tidy --root` for a tree that breaks a rule.
const EXIT_TREE_VIOLATIONS: i32 = 1;

/// Lines of a failing tool's output that a finding quotes.
const QUOTED_LINES: usize = 8;

/// One fixture of the base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixture {
    /// The rule it belongs to.
    pub id: String,
    /// True for `<id>.fires.fixture`, false for `<id>.passes.fixture`.
    pub fires: bool,
    /// Its path below the base root, for the findings.
    pub file: String,
    /// The path that its first line names, which decides the rules that apply.
    pub virtual_path: PathBuf,
    /// Its text, first line included.
    pub text: String,
}

/// One violation that a tidy run printed, as `path:line: error[rule]: message`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Report {
    /// The path, relative to the tree's root.
    pub path: String,
    /// The line, or 0 for the whole file.
    pub line: u32,
    /// The rule id.
    pub rule: String,
}

/// Every fixture of the base, sorted by file name.
///
/// # Errors
///
/// Returns what is wrong if the directory or a fixture cannot be read, or if a fixture's first
/// line does not name a plain relative path.
pub fn fixtures(base: &Path) -> Result<Vec<Fixture>, String> {
    let directory = base.join(FIXTURE_DIRECTORY);
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("cannot read the base's {FIXTURE_DIRECTORY}: {error}"))?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {FIXTURE_DIRECTORY}: {error}"))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    let mut found = Vec::new();
    for name in names {
        let (id, fires) = if let Some(id) = name.strip_suffix(FIRES_SUFFIX) {
            (id, true)
        } else if let Some(id) = name.strip_suffix(PASSES_SUFFIX) {
            (id, false)
        } else {
            continue;
        };
        let file = format!("{FIXTURE_DIRECTORY}/{name}");
        let text = fs::read_to_string(base.join(&file))
            .map_err(|error| format!("cannot read the base's {file}: {error}"))?;
        let first_line = text.lines().next().unwrap_or_default();
        let virtual_path = crate::tidy::fixture_virtual_path(first_line)
            .filter(|path| is_plain_relative(path))
            .ok_or_else(|| format!("the base's {file} names no plain relative path"))?;
        found.push(Fixture { id: id.to_owned(), fires, file, virtual_path, text });
    }
    assert!(found.iter().all(|fixture| !fixture.id.is_empty()), "every fixture names a rule");
    Ok(found)
}

/// Reads the violations a tidy run printed. Lines in another form are skipped.
#[must_use]
pub fn parse_reports(text: &str) -> Vec<Report> {
    let mut reports = Vec::new();
    for line in text.lines() {
        let Some((location, rest)) = line.split_once(": error[") else {
            continue;
        };
        let Some((rule, _message)) = rest.split_once("]: ") else {
            continue;
        };
        let Some((path, number)) = location.rsplit_once(':') else {
            continue;
        };
        if let Ok(number) = number.parse::<u32>() {
            reports.push(Report { path: path.to_owned(), line: number, rule: rule.to_owned() });
        }
    }
    assert!(reports.len() <= text.lines().count(), "at most one report per line");
    reports
}

/// Plants every base fixture in the scratch copy of the head tree, at its virtual path and at
/// paths drawn from the tree, and records what the head's `tidy --file` and, for a firing
/// fixture, `tidy --root` print. Runs head code; judges nothing.
///
/// # Errors
///
/// Returns what failed if a base fixture is broken or the scratch tree cannot be written.
pub fn observe(
    base: &Path,
    scratch: &Scratch,
    head_xtask: &Path,
    rng: &mut Rng,
) -> Result<Vec<Run>, String> {
    let files: Vec<String> = super::tree::entries(scratch.root())
        .map_err(|error| format!("cannot list the scratch tree: {error}"))?
        .into_keys()
        .collect();
    let mut runs = Vec::new();
    for fixture in &fixtures(base)? {
        let expected = base_reports(base, fixture)?;
        let placements = plant::placements(
            base,
            scratch.root(),
            &fixture.virtual_path,
            &fixture.text,
            &expected,
            &files,
            rng,
        );
        for placement in placements {
            let planted = scratch
                .plant(&placement, &fixture.text)
                .map_err(|error| format!("cannot plant {}: {error}", fixture.file))?;
            let file = scratch.root().join(&placement);
            runs.push(run_tidy(head_xtask, scratch.root(), fixture, &placement, "--file", &file));
            if fixture.fires {
                let root = scratch.root().to_path_buf();
                runs.push(run_tidy(head_xtask, &root, fixture, &placement, "--root", &root));
            }
            planted
                .remove()
                .map_err(|error| format!("cannot restore the scratch tree: {error}"))?;
        }
    }
    assert!(runs.iter().all(|run| run.mode == "file" || run.mode == "tree"), "tidy runs only");
    Ok(runs)
}

/// Runs the head's `xtask tidy <option> <argument>` and records it.
fn run_tidy(
    head_xtask: &Path,
    root: &Path,
    fixture: &Fixture,
    placement: &Path,
    option: &str,
    argument: &Path,
) -> Run {
    let mut command = super::command(head_xtask, root);
    let _command = command.arg("tidy").arg(option).arg(argument);
    let mode = if option == "--file" { "file" } else { "tree" };
    let output = command.output();
    Run {
        subject: fixture.file.clone(),
        placement: placement.to_string_lossy().into_owned(),
        mode: mode.to_owned(),
        exit: output.as_ref().ok().and_then(|output| output.status.code()),
        stdout: output.as_ref().map_or_else(ToString::to_string, |o| lossy(&o.stdout)),
        stderr: output.as_ref().map_or_else(|_| String::new(), |o| lossy(&o.stderr)),
        files: std::collections::BTreeMap::new(),
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// What the base's tidy reports on a fixture at its virtual path, as `(rule, line)` pairs.
fn base_reports(base: &Path, fixture: &Fixture) -> Result<BTreeSet<(String, u32)>, String> {
    let reports = plant::reports_at(base, &fixture.virtual_path, &fixture.text)
        .ok_or_else(|| format!("the base's tidy cannot check {}", fixture.file))?;
    if fixture.fires && !reports.iter().any(|(rule, _)| *rule == fixture.id) {
        return Err(format!(
            "the base's {} does not fire `{}` under the base's own tidy; the base is broken, \
             and the ratchet cannot judge against it",
            fixture.file, fixture.id
        ));
    }
    Ok(reports)
}

/// Judges the recorded tidy runs against the base's fixtures, read from the base's own
/// checkout. Returns the corpus and end-to-end findings.
///
/// # Errors
///
/// No verdict: a base without fixtures, a fixture without its runs, a placement where the base
/// does not report what the case is for, or a run whose output cannot be read.
pub fn judge(
    base: &Path,
    head: &Path,
    observations: &Observations,
) -> Result<Vec<Finding>, String> {
    let fixtures = fixtures(base)?;
    if fixtures.is_empty() {
        return Err(format!("the base has no fixtures in {FIXTURE_DIRECTORY}"));
    }
    let mut findings = Vec::new();
    for fixture in &fixtures {
        let at_home = fixture.virtual_path.to_string_lossy().into_owned();
        let mut modes = vec!["file"];
        if fixture.fires {
            modes.push("tree");
        }
        for mode in modes {
            let runs: Vec<&Run> = observations.runs_of(&fixture.file, mode).collect();
            if !runs.iter().any(|run| run.placement == at_home) {
                return Err(format!("no `tidy` {mode} run of {} at {at_home}", fixture.file));
            }
            for run in runs {
                findings.extend(judge_run(base, head, fixture, run)?);
            }
        }
    }
    Ok(findings)
}

/// Judges one run: the base's reports at the placement, against what the head printed.
fn judge_run(
    base: &Path,
    head: &Path,
    fixture: &Fixture,
    run: &Run,
) -> Result<Vec<Finding>, String> {
    let placement = Path::new(&run.placement);
    let plain = is_plain_relative(placement) && crate::tidy::is_walked(head, placement);
    if !plain || Scope::of(placement) != Scope::of(&fixture.virtual_path) {
        return Err(format!(
            "{} was planted at {}, outside its scope or tidy's walk",
            fixture.file, run.placement
        ));
    }
    let expected = plant::reports_at(base, placement, &fixture.text).ok_or_else(|| {
        format!(
            "the base's tidy cannot check {} at \
                                {}",
            fixture.file, run.placement
        )
    })?;
    if fixture.fires && !expected.iter().any(|(rule, _)| *rule == fixture.id) {
        return Err(format!(
            "{} was planted at {}, where the base does not report `{}`",
            fixture.file, run.placement, fixture.id
        ));
    }
    let reports = reports_of(run)?;
    let found = pairs_at(&reports, placement);
    Ok(compare(fixture, run, &expected, &found))
}

/// The violations a run printed, if its exit code agrees with them; otherwise no verdict.
fn reports_of(run: &Run) -> Result<Vec<Report>, String> {
    let reports = parse_reports(&run.stderr);
    let violations = if run.mode == "file" { EXIT_FILE_VIOLATIONS } else { EXIT_TREE_VIOLATIONS };
    match run.exit {
        Some(0) if reports.is_empty() => Ok(reports),
        Some(code) if code == violations && !reports.is_empty() => Ok(reports),
        exit => Err(format!(
            "the head's `tidy` ({}) on {} at {} exited with {exit:?} and printed {} \
             violation(s); a tool that does not answer cannot be judged: {}",
            run.mode,
            run.subject,
            run.placement,
            reports.len(),
            super::tail(&run.stderr, QUOTED_LINES)
        )),
    }
}

/// The pairs of `reports` at `path`.
fn pairs_at(reports: &[Report], path: &Path) -> BTreeSet<(String, u32)> {
    let path = path.to_string_lossy();
    reports
        .iter()
        .filter(|report| report.path == path)
        .map(|report| (report.rule.clone(), report.line))
        .collect()
}

fn compare(
    fixture: &Fixture,
    run: &Run,
    expected: &BTreeSet<(String, u32)>,
    found: &BTreeSet<(String, u32)>,
) -> Vec<Finding> {
    let at = &run.placement;
    let (check, missing): (Check, Vec<&(String, u32)>) = if run.mode == "tree" {
        (Check::EndToEnd, expected.difference(found).collect())
    } else if fixture.fires {
        (Check::TidyCorpus, expected.difference(found).collect())
    } else {
        (Check::TidyCorpus, found.difference(expected).collect())
    };
    let message = |rule: &str, line: u32| match (run.mode.as_str(), fixture.fires) {
        ("tree", _) => format!(
            "planted at {at}, this case breaks `{rule}` on line {line}, and the head's \
             `cargo xtask tidy` does not report it; the PR changes which files tidy checks \
             (ADR-0022)"
        ),
        (_, true) => format!(
            "at {at}, the base's tidy reports `{rule}` here and the head's does not, so the PR \
             loosens `{rule}`; make it report this case again (ADR-0022)"
        ),
        (_, false) => format!(
            "at {at}, the head's tidy reports `{rule}` on this passing fixture, which the base \
             accepts, so the PR makes `{rule}` fire on correct code (ADR-0022)"
        ),
    };
    missing
        .into_iter()
        .map(|(rule, line)| {
            Finding::new(&format!("{}:{line}", fixture.file), check, message(rule, *line))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Report, fixtures, parse_reports, reports_of};
    use crate::ratchet::observations::Run;
    use crate::tidy::scratch::Scratch;
    use std::path::Path;

    // Goal: violations are read from tidy's output, whatever the path or message holds, and
    // other lines are skipped.
    #[test]
    fn reports_are_parsed() {
        let text = "crates/a.rs:3: error[unbounded-loop]: a `loop`; bound it: x]: y\n\
                    Cargo.toml:0: error[dependency-override]: what; fix\n\
                    tidy: 2 violation(s)\n\
                    a:b.rs:x: error[rule]: not a line number\n\
                    a.rs:4: error[no closing bracket\n";
        let report = |path: &str, line: u32, rule: &str| Report {
            path: path.to_owned(),
            line,
            rule: rule.to_owned(),
        };
        assert_eq!(
            parse_reports(text),
            [
                report("crates/a.rs", 3, "unbounded-loop"),
                report("Cargo.toml", 0, "dependency-override")
            ]
        );
        assert_eq!(parse_reports(""), []);
    }

    // Goal: fixtures are read with their rule, outcome and virtual path; other files are
    // skipped, and a fixture whose path could leave the tree is an error.
    #[test]
    fn fixtures_are_read_from_the_base() {
        let base = Scratch::new("ratchet-fixtures");
        base.write(
            "xtask/fixtures/tidy/a-rule.fires.fixture",
            "// path: crates/a/src/x.rs\nfn a() {}\n",
        );
        base.write("xtask/fixtures/tidy/a-rule.passes.fixture", "# path: Cargo.toml\n");
        base.write("xtask/fixtures/tidy/notes.md", "not a fixture\n");
        let found = fixtures(base.root()).unwrap();
        let summary: Vec<(&str, bool, &Path)> = found
            .iter()
            .map(|fixture| (fixture.id.as_str(), fixture.fires, fixture.virtual_path.as_path()))
            .collect();
        assert_eq!(
            summary,
            [
                ("a-rule", true, Path::new("crates/a/src/x.rs")),
                ("a-rule", false, Path::new("Cargo.toml"))
            ]
        );
        assert_eq!(found[0].file, "xtask/fixtures/tidy/a-rule.fires.fixture");
        base.write("xtask/fixtures/tidy/b.fires.fixture", "// path: ../outside.rs\n");
        let error = fixtures(base.root()).expect_err("a path that leaves the tree");
        assert!(error.contains("b.fires.fixture names no plain relative path"), "{error}");
        let empty = Scratch::new("ratchet-fixtures-none");
        assert!(fixtures(empty.root()).is_err(), "a base without fixtures cannot be judged");
    }

    // Goal: a run's output is read only if its exit code agrees with what it printed: 0 with
    // no violation, or the mode's violation code with at least one; anything else is no
    // verdict.
    #[test]
    fn runs_are_read_strictly() {
        let run = |mode: &str, exit: Option<i32>, stderr: &str| Run {
            subject: "a.fires.fixture".to_owned(),
            placement: "a.rs".to_owned(),
            mode: mode.to_owned(),
            exit,
            stdout: String::new(),
            stderr: stderr.to_owned(),
            files: std::collections::BTreeMap::new(),
        };
        let violation = "a.rs:3: error[unbounded-loop]: a loop; bound it\n";
        assert_eq!(reports_of(&run("file", Some(0), "")), Ok(Vec::new()));
        assert_eq!(reports_of(&run("file", Some(2), violation)).unwrap().len(), 1);
        assert_eq!(reports_of(&run("tree", Some(1), violation)).unwrap().len(), 1);
        assert!(reports_of(&run("file", Some(0), violation)).is_err(), "0 with a violation");
        assert!(reports_of(&run("file", Some(2), "")).is_err(), "2 with no violation");
        assert!(reports_of(&run("file", Some(1), violation)).is_err(), "the tree's code");
        assert!(reports_of(&run("tree", Some(2), violation)).is_err(), "the file's code");
        assert!(reports_of(&run("file", None, "")).is_err(), "killed");
    }
}
