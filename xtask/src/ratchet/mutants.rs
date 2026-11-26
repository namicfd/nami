//! Mutation testing of the rule-path diff: `cargo mutants --in-diff` must leave no mutant
//! missed (ADR-0009, ADR-0022).
//!
//! The corpus shows that the base's cases still hold. Mutation testing shows that the head's
//! own tests pin down the code it changed, so that the cases this PR adds are real cases. The
//! diff is built here from the two trees, one per cargo workspace: `xtask` in the root workspace
//! and the lint library in `lints/`, which is its own.
//!
//! cargo-mutants runs the head's tests, so it runs in the observing job ([`observe`]), which
//! records its exit code, the number of mutants it listed, `outcomes.json` and `missed.txt`.
//! The judge reads them strictly ([`judge`]): anything missing, unexpected or inconsistent is
//! no verdict. A changed hunk without a mutant of its own proves nothing either, because a
//! changed constant or a deleted line makes none ([`super::hunks`]). The registry's tables are
//! the exception: the rule-set check reads them as data, so a change to them alone passes with
//! no mutant.

use super::observations::{Observations, Run};
use super::{Check, Finding, Tools};
use crate::json::{self, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// One cargo workspace whose rule paths are mutated.
struct Workspace {
    /// Name, for the files the ratchet writes and the observations.
    name: &'static str,
    /// Rule-path Rust sources below this prefix are mutated in this workspace.
    sources: &'static str,
    /// The workspace's directory below the root; empty for the root.
    directory: &'static str,
    /// Arguments that select the package and the test runner.
    arguments: &'static [&'static str],
}

/// The two workspaces that hold rule paths.
const WORKSPACES: [Workspace; 2] = [
    Workspace {
        name: "xtask",
        sources: "xtask/src/",
        directory: "",
        arguments: &["--package", "xtask", "--test-tool", "nextest"],
    },
    Workspace { name: "lints", sources: "lints/src/", directory: "lints", arguments: &[] },
];

/// Seconds a test run may take under one mutant before it counts as timed out.
const TEST_SECONDS_MAX: &str = "300";

/// The names under which an observation keeps cargo-mutants' results.
const LISTED: &str = "listed";
const OUTCOMES: &str = "outcomes.json";
const MISSED: &str = "missed.txt";
const TIMEOUT: &str = "timeout.txt";

/// The number of non-blank lines of `text`.
fn line_count(text: &str) -> u64 {
    u64::try_from(text.lines().filter(|line| !line.trim().is_empty()).count()).expect("counts fit")
}

/// The changed rule-path Rust sources of the head that belong to `workspace`.
fn sources<'a>(head: &Path, rule_paths: &'a [String], workspace: &Workspace) -> Vec<&'a String> {
    rule_paths
        .iter()
        .filter(|path| path.starts_with(workspace.sources) && is_rust_source(path))
        .filter(|path| head.join(path).is_file())
        .collect()
}

/// Runs cargo-mutants on the head, once per workspace with changed rule-path sources, and
/// records what it left. Runs head code; judges nothing.
///
/// # Errors
///
/// Returns what failed if the diff cannot be built or written.
pub fn observe(
    base: &Path,
    head: &Path,
    rule_paths: &[String],
    tools: &Tools<'_>,
) -> Result<Vec<Run>, String> {
    let mut runs = Vec::new();
    for workspace in &WORKSPACES {
        let changed = sources(head, rule_paths, workspace);
        if changed.is_empty() {
            continue;
        }
        let mut diff = String::new();
        for path in &changed {
            diff.push_str(&unified_diff(base, head, path, workspace.directory)?);
        }
        runs.push(run_mutants(head, workspace, &diff, tools)?);
    }
    Ok(runs)
}

/// What mutation testing showed: findings, a note per workspace with its mutant count, and
/// what it could not show.
#[derive(Debug, Default)]
pub struct Judged {
    /// Missed mutants.
    pub findings: Vec<Finding>,
    /// The number of mutants per workspace.
    pub notes: Vec<String>,
    /// Changes that yield no mutant, which mutation testing cannot vouch for.
    pub unproven: Vec<String>,
}

/// Judges the recorded mutation runs. `Err` is no verdict.
///
/// # Errors
///
/// No verdict: a missing run, an unexpected exit code, or an unreadable or inconsistent
/// listing, `outcomes.json` or `missed.txt`.
pub fn judge(
    base: &Path,
    head: &Path,
    rule_paths: &[String],
    observations: &Observations,
    work: &Path,
) -> Result<Judged, String> {
    let mut judged = Judged::default();
    for workspace in &WORKSPACES {
        if sources(head, rule_paths, workspace).is_empty() {
            continue;
        }
        let run = observations.runs_of(workspace.name, "mutants").next().ok_or_else(|| {
            format!("the observations hold no mutation run of {}", workspace.name)
        })?;
        let (count, missed) = read_run(run)?;
        let missed_count = missed.lines().filter(|line| !line.trim().is_empty()).count();
        judged
            .notes
            .push(format!("Mutants in {}: {count}, missed: {missed_count}.", workspace.name));
        // A hunk that the data checks cover (the registry's tables) needs no mutant, so a
        // data-only change passes with none; every other hunk needs one of its own.
        let starts =
            if count == 0 { BTreeMap::new() } else { mutant_starts(run, workspace.directory)? };
        let mut uncovered = Vec::new();
        for path in changed_sources(rule_paths, workspace) {
            uncovered.extend(uncovered_hunks(base, head, path, &starts, work)?);
        }
        assert!(count > 0 || starts.is_empty(), "no mutant places without mutants");
        if count > 0 {
            judged.unproven.extend(uncovered);
        } else if let Some(first) = uncovered.first() {
            judged.unproven.push(format!(
                "the rule-path change in {} yields no mutant, so mutation testing proves \
                 nothing about it (a changed constant or a deleted line makes none); {first}",
                workspace.name
            ));
        }
        judged.findings.extend(missed_findings(&missed, workspace.directory));
    }
    Ok(judged)
}

/// The changed rule-path Rust sources of `workspace`, in either tree, but the registry, which
/// the rule-set check reads as data.
fn changed_sources<'a>(rule_paths: &'a [String], workspace: &Workspace) -> Vec<&'a String> {
    rule_paths
        .iter()
        .filter(|path| path.starts_with(workspace.sources) && is_rust_source(path))
        .filter(|path| path.as_str() != super::source::REGISTRY)
        .collect()
}

/// The first line of every caught or missed mutant, by file relative to the root.
fn mutant_starts(run: &Run, directory: &str) -> Result<BTreeMap<String, Vec<u32>>, String> {
    let text = run.files.get(OUTCOMES).cloned().flatten().unwrap_or_default();
    let outcomes =
        json::parse(&text).map_err(|error| format!("{OUTCOMES} is not JSON: {}", error.message))?;
    let list = outcomes.get("outcomes").and_then(Value::as_array).ok_or_else(|| {
        format!("{OUTCOMES} of {} lists no outcomes, so no mutant can be placed", run.subject)
    })?;
    let mut starts: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for outcome in list {
        let summary = outcome.get("summary").and_then(Value::as_str).unwrap_or_default();
        let Some(mutant) = outcome.get("scenario").and_then(|scenario| scenario.get("Mutant"))
        else {
            continue;
        };
        if summary != "CaughtMutant" && summary != "MissedMutant" {
            continue;
        }
        let file = mutant.get("file").and_then(Value::as_str);
        let line = match mutant
            .get("span")
            .and_then(|span| span.get("start"))
            .and_then(|start| start.get("line"))
        {
            Some(Value::Number(digits)) => digits.parse::<u32>().ok(),
            _ => None,
        };
        let (Some(file), Some(line)) = (file, line) else {
            return Err(format!("a mutant in {OUTCOMES} of {} has no file or line", run.subject));
        };
        let path =
            if directory.is_empty() { file.to_owned() } else { format!("{directory}/{file}") };
        starts.entry(path).or_default().push(line);
    }
    Ok(starts)
}

/// Every hunk of `path` that needs a mutant and contains none, as an unproven item.
fn uncovered_hunks(
    base: &Path,
    head: &Path,
    path: &str,
    starts: &BTreeMap<String, Vec<u32>>,
    work: &Path,
) -> Result<Vec<String>, String> {
    let ranges = |root: &Path| -> Result<Vec<(u32, u32)>, String> {
        match super::source::read_optional(root, path)? {
            Some(text) => super::hunks::exempt_ranges(path, &text),
            None => Ok(Vec::new()),
        }
    };
    let (base_ranges, head_ranges) = (ranges(base)?, ranges(head)?);
    let lines = starts.get(path).map_or(&[][..], Vec::as_slice);
    let mut unproven = Vec::new();
    for hunk in super::hunks::hunks(base, head, path, work)? {
        if super::hunks::is_exempt(&hunk, &head_ranges, &base_ranges)
            || super::hunks::contains_mutant(&hunk, lines)
        {
            continue;
        }
        unproven.push(format!(
            "the change at {path}:{} contains no mutant (a changed literal, a deleted line or a \
             moved statement makes none), so mutation testing proves nothing about it",
            hunk.head_first
        ));
    }
    Ok(unproven)
}

/// Reads one mutation run: the number of mutants, and `missed.txt`. Everything must agree:
/// the listing, the outcomes, the exit code and the missed list. A mutant that timed out is
/// neither caught nor missed, so any timeout is no verdict, naming the mutants from
/// `timeout.txt`.
fn read_run(run: &Run) -> Result<(u64, String), String> {
    let file = |name: &str| -> Result<String, String> {
        run.files
            .get(name)
            .cloned()
            .flatten()
            .ok_or_else(|| format!("cargo-mutants left no {name} for {}", run.subject))
    };
    let listed: u64 =
        file(LISTED)?.trim().parse().map_err(|_| "the mutant count is not a number")?;
    if listed == 0 {
        return Ok((0, String::new()));
    }
    let outcomes = json::parse(&file(OUTCOMES)?)
        .map_err(|error| format!("{OUTCOMES} is not JSON: {}", error.message))?;
    let count = |key: &str| -> Result<u64, String> {
        match outcomes.get(key) {
            Some(Value::Number(digits)) => {
                digits.parse().map_err(|_| format!("`{key}` is not a count"))
            }
            _ => Err(format!("{OUTCOMES} has no `{key}`")),
        }
    };
    let missed_text = file(MISSED)?;
    let missed_lines = line_count(&missed_text);
    let (total, missed, timeouts) = (count("total_mutants")?, count("missed")?, count("timeout")?);
    let tested = count("caught")? + missed + timeouts + count("unviable")?;
    let exit_agrees = match run.exit {
        Some(0) => missed == 0 && timeouts == 0,
        Some(2) => missed > 0,
        Some(3) => missed == 0 && timeouts > 0,
        _ => false,
    };
    if total != listed || tested != total || missed != missed_lines || !exit_agrees {
        return Err(format!(
            "cargo-mutants in {} does not add up: listed {listed}, total {total}, tested {tested}, \
             missed {missed} with {missed_lines} in {MISSED}, exit {:?}",
            run.subject, run.exit
        ));
    }
    if timeouts > 0 {
        let timed_out = file(TIMEOUT)?;
        if line_count(&timed_out) != timeouts {
            return Err(format!(
                "cargo-mutants in {} counts {timeouts} timeouts and lists {} in {TIMEOUT}",
                run.subject,
                line_count(&timed_out)
            ));
        }
        return Err(format!(
            "{timeouts} mutant(s) in {} timed out, so the head's tests neither catch nor miss \
             them; make the tests that cover them finish: {}",
            run.subject,
            super::tail(&timed_out, 20)
        ));
    }
    Ok((total, missed_text))
}

/// True for a path with the extension `rs`.
fn is_rust_source(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|extension| extension == "rs")
}

/// The diff of one file between base and head, with paths relative to the workspace in
/// `directory`, as cargo-mutants reads them. Empty if the file did not change.
fn unified_diff(base: &Path, head: &Path, path: &str, directory: &str) -> Result<String, String> {
    let old = base.join(path);
    let old = if old.is_file() { old } else { Path::new("/dev/null").to_path_buf() };
    let mut command = super::command("git", head);
    let _command = command
        .args(["diff", "--no-index", "--no-color", "--no-ext-diff", "--unified=3", "--"])
        .arg(&old)
        .arg(head.join(path));
    let output = super::output(&mut command, "`git diff --no-index`")?;
    match output.status.code() {
        Some(0) => Ok(String::new()),
        Some(1) => {
            // The root workspace's directory is empty, and no path starts with `/`.
            let relative = path.strip_prefix(&format!("{directory}/")).unwrap_or(path);
            let is_new = old == Path::new("/dev/null");
            Ok(with_headers(&String::from_utf8_lossy(&output.stdout), relative, is_new))
        }
        status => Err(format!(
            "`git diff --no-index` exited with {status:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// Replaces the headers that `git diff --no-index` writes, which name the two trees, with the
/// headers of an ordinary diff of `relative`.
#[must_use]
pub fn with_headers(diff: &str, relative: &str, is_new: bool) -> String {
    let Some(first_hunk) = diff.find("\n@@").map(|index| index + 1) else {
        return String::new();
    };
    let old = if is_new { "/dev/null".to_owned() } else { format!("a/{relative}") };
    let hunks = diff.get(first_hunk..).expect("the hunk starts inside the diff");
    let rewritten =
        format!("diff --git a/{relative} b/{relative}\n--- {old}\n+++ b/{relative}\n{hunks}");
    assert!(rewritten.ends_with(hunks), "the hunks are kept as they are");
    rewritten
}

/// Lists and runs cargo-mutants on the head with the diff, and records what it left.
fn run_mutants(
    head: &Path,
    workspace: &Workspace,
    diff: &str,
    tools: &Tools<'_>,
) -> Result<Run, String> {
    let diff_file = tools.work.join(format!("mutants-{}.diff", workspace.name));
    let output_directory = tools.work.join(format!("mutants-{}", workspace.name));
    fs::write(&diff_file, diff).map_err(|error| format!("cannot write the diff: {error}"))?;
    if output_directory.exists() {
        fs::remove_dir_all(&output_directory)
            .map_err(|error| format!("cannot clear {}: {error}", output_directory.display()))?;
    }
    let directory = head.join(workspace.directory);
    let mutants = |list: bool| {
        let mut command = super::command(&tools.cargo, &directory);
        let _command = command
            .args(["mutants", "--in-diff"])
            .arg(&diff_file)
            .arg("--dir")
            .arg(&directory)
            .args(workspace.arguments);
        if list {
            let _list = command.arg("--list");
        } else {
            let _run = command.arg("--output").arg(&output_directory).args([
                "--no-shuffle",
                "--timeout",
                TEST_SECONDS_MAX,
            ]);
        }
        super::output(&mut command, "`cargo mutants`")
    };
    let listing = mutants(true)?;
    let listed = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let listed = if listing.status.success() { Some(listed.to_string()) } else { None };
    let output = if listed.as_deref() == Some("0") { listing } else { mutants(false)? };
    let read =
        |name: &str| fs::read_to_string(output_directory.join("mutants.out").join(name)).ok();
    let files = BTreeMap::from([
        (LISTED.to_owned(), listed),
        (OUTCOMES.to_owned(), read(OUTCOMES)),
        (MISSED.to_owned(), read(MISSED)),
        (TIMEOUT.to_owned(), read(TIMEOUT)),
    ]);
    Ok(Run {
        subject: workspace.name.to_owned(),
        placement: String::new(),
        mode: "mutants".to_owned(),
        exit: output.status.code(),
        stdout: super::tail(&String::from_utf8_lossy(&output.stdout), 40),
        stderr: super::tail(&String::from_utf8_lossy(&output.stderr), 40),
        files,
    })
}

/// One finding per line of cargo-mutants' `missed.txt`: `path:line:column: what`.
#[must_use]
pub fn missed_findings(missed: &str, directory: &str) -> Vec<Finding> {
    let findings: Vec<Finding> = missed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (place, mutation) = line.split_once(": ").unwrap_or((line, "a mutation"));
            let place = if directory.is_empty() {
                place.to_owned()
            } else {
                format!("{directory}/{place}")
            };
            let message = format!(
                "the mutant `{mutation}` survives the head's tests; add a case that tells the \
                 mutated rule from the real one (ADR-0009, ADR-0022)"
            );
            Finding::new(&place, Check::Mutants, message)
        })
        .collect();
    assert!(
        u64::try_from(findings.len()) == Ok(line_count(missed)),
        "one finding per missed mutant"
    );
    findings
}

#[cfg(test)]
mod tests {
    use super::{LISTED, MISSED, OUTCOMES, TIMEOUT, missed_findings, read_run, with_headers};
    use crate::ratchet::observations::Run;
    use std::collections::BTreeMap;

    // Goal: the headers of a `--no-index` diff are replaced by those of the relative path, for
    // a changed and for a new file, and the hunks are kept; a diff without hunks is empty.
    #[test]
    fn diff_headers_name_the_relative_path() {
        let diff = "diff --git a/tmp/base/lints/src/a.rs b/tmp/head/lints/src/a.rs\n\
                    index 1..2 100644\n--- a/tmp/base/lints/src/a.rs\n\
                    +++ b/tmp/head/lints/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
        assert_eq!(
            with_headers(diff, "src/a.rs", false),
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
             @@ -1 +1 @@\n-old\n+new\n"
        );
        let added = with_headers(diff, "src/a.rs", true);
        assert!(added.contains("\n--- /dev/null\n+++ b/src/a.rs\n"), "{added}");
        assert_eq!(with_headers("Binary files differ\n", "src/a.rs", false), "");
    }

    // Goal: each missed mutant becomes a finding at its place, below the workspace directory.
    #[test]
    fn missed_mutants_are_findings() {
        let missed = "src/a.rs:12:5: replace f -> bool with true\n\nsrc/b.rs:3:1: delete !\n";
        let findings = missed_findings(missed, "lints");
        let places: Vec<&str> = findings.iter().map(|finding| finding.place.as_str()).collect();
        assert_eq!(places, ["lints/src/a.rs:12:5", "lints/src/b.rs:3:1"]);
        assert!(findings[0].message.contains("`replace f -> bool with true` survives"));
        assert_eq!(missed_findings("xtask/src/c.rs:1:1: x\n", "")[0].place, "xtask/src/c.rs:1:1");
        assert!(missed_findings("", "").is_empty(), "nothing missed");
    }

    fn run(exit: Option<i32>, listed: &str, counts: [u32; 5], missed: &str) -> Run {
        let [total, caught, lost, timeouts, unviable] = counts;
        let outcomes = format!(
            "{{\"total_mutants\":{total},\"caught\":{caught},\"missed\":{lost},\
             \"timeout\":{timeouts},\"unviable\":{unviable}}}"
        );
        let files = BTreeMap::from([
            (LISTED.to_owned(), Some(listed.to_owned())),
            (OUTCOMES.to_owned(), Some(outcomes)),
            (MISSED.to_owned(), Some(missed.to_owned())),
            (
                TIMEOUT.to_owned(),
                Some("src/a.rs:2:1: replace b\n".repeat(usize::try_from(timeouts).unwrap())),
            ),
        ]);
        Run {
            subject: "lints".to_owned(),
            placement: String::new(),
            mode: "mutants".to_owned(),
            exit,
            stdout: String::new(),
            stderr: String::new(),
            files,
        }
    }

    // Goal: a mutation run is read only if the listing, the outcomes, `missed.txt` and the
    // exit code agree: 0 for none missed and none timed out, 2 for some missed, 3 for only
    // timeouts. Any timeout is no verdict that names the mutants. Zero mutants listed is read
    // as zero, for the judge to call unproven.
    #[test]
    fn mutation_runs_are_read_strictly() {
        let missed = "src/a.rs:1:1: replace a\n";
        assert_eq!(read_run(&run(Some(0), "2", [2, 2, 0, 0, 0], "")), Ok((2, String::new())));
        assert_eq!(read_run(&run(Some(2), "2", [2, 1, 1, 0, 0], missed)).unwrap().0, 2);
        let timed_out = read_run(&run(Some(3), "2", [2, 1, 0, 1, 0], "")).unwrap_err();
        assert!(timed_out.contains("1 mutant(s) in lints timed out"), "{timed_out}");
        assert!(timed_out.contains("src/a.rs:2:1: replace b"), "{timed_out}");
        let mut unlisted = run(Some(3), "2", [2, 1, 0, 1, 0], "");
        let _emptied = unlisted.files.insert(TIMEOUT.to_owned(), Some(String::new()));
        assert!(read_run(&unlisted).unwrap_err().contains("lists 0 in timeout.txt"));
        assert_eq!(read_run(&run(Some(0), "0", [0, 0, 0, 0, 0], "")), Ok((0, String::new())));
        let inconsistent = [
            run(Some(0), "2", [2, 1, 1, 0, 0], missed),
            run(Some(0), "2", [2, 1, 0, 1, 0], ""),
            run(Some(2), "2", [2, 2, 0, 0, 0], ""),
            run(Some(2), "2", [2, 1, 1, 0, 0], ""),
            run(Some(3), "2", [2, 2, 0, 0, 0], ""),
            run(Some(3), "2", [2, 0, 1, 1, 0], missed),
            run(Some(0), "3", [2, 2, 0, 0, 0], ""),
            run(Some(0), "2", [2, 1, 0, 0, 0], ""),
            run(Some(1), "2", [2, 2, 0, 0, 0], ""),
            run(None, "2", [2, 2, 0, 0, 0], ""),
        ];
        for (index, bad) in inconsistent.iter().enumerate() {
            assert!(read_run(bad).is_err(), "case {index} must not be read");
        }
        let mut gone = run(Some(2), "1", [1, 0, 1, 0, 0], missed);
        let _removed = gone.files.insert(MISSED.to_owned(), None);
        assert!(read_run(&gone).unwrap_err().contains("left no missed.txt"));
    }
}
