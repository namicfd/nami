//! The lint corpus, and the dylint half of the end-to-end check.
//!
//! Every UI case under the base's `lints/ui/` is a case written down. The ratchet compiles the
//! base's cases as targets of one scratch package, once with the base's lint library and once
//! with the head's (`cargo dylint --path`), and reads the diagnostics as JSON, which names the
//! lint of each. The base's run must reproduce the locations in the base's `.stderr` files,
//! which the base's own UI test compares exactly; otherwise the harness is not trusted and the
//! ratchet stops. Then:
//!
//! - **corpus:** every `(lint, line, column)` that the base's library reports, the head's
//!   library reports too, at the same level. More diagnostics are a tightening.
//! - **end to end:** each case is planted as an example with a random name in a crate drawn
//!   from the head tree, and the head's own `cargo xtask dylint` must report every one of those
//!   locations. This catches a head whose wrapper or workspace no longer lints a crate.
//!
//! The head's library and xtask run in one job ([`observe`], under a random package name); the
//! base's library runs in the judging job, on the base's own checkout ([`judge`]).

use super::observations::{Observations, Run};
use super::plant::Rng;
use super::tree::{Scratch, is_plain_relative};
use super::{Check, Finding, Tools};
use crate::json::{self, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Where the UI cases live, relative to the root.
pub const UI_DIRECTORY: &str = "lints/ui";

/// The header line of a UI case that passes flags to the compiler.
const COMPILE_FLAGS: &str = "// compile-flags:";

/// The one compiler flag that UI cases use: compile the case as a test harness.
const TEST_FLAG: &str = "--test";

/// The edition the UI cases are compiled with: the lint library's, which `dylint_testing`
/// passes on to its UI tests.
const UI_EDITION: &str = "2024";

/// Lines of a failing tool's output that a finding quotes.
const QUOTED_LINES: usize = 12;

/// One UI case of the base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiCase {
    /// The file name without `.rs`, which is also the target name.
    pub stem: String,
    /// True if the case is compiled as a test harness (`// compile-flags: --test`).
    pub is_test: bool,
    /// The case's source.
    pub text: String,
    /// The expected compiler output, empty if the case has no `.stderr` file.
    pub stderr: String,
}

/// One diagnostic of a lint, at its primary location.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Diagnostic {
    /// The file it points at, relative to the package or workspace.
    pub file: String,
    /// One-based line.
    pub line: u32,
    /// One-based column.
    pub column: u32,
    /// The lint's name, such as `nami_recursion`.
    pub lint: String,
    /// `error` or `warning`.
    pub level: String,
}

/// A diagnostic as the compiler prints it for people: a `level: message` line, then
/// `--> path:line:column`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Located {
    /// `error` or `warning`.
    pub level: String,
    /// The path as printed.
    pub path: String,
    /// One-based line.
    pub line: u32,
    /// One-based column.
    pub column: u32,
}

/// Every UI case of the base, sorted by name.
///
/// # Errors
///
/// Returns what is wrong if a case cannot be read, has a name that is not a target name, or
/// passes a compiler flag that the ratchet cannot reproduce.
pub fn cases(base: &Path) -> Result<Vec<UiCase>, String> {
    let directory = base.join(UI_DIRECTORY);
    let mut names: Vec<String> = Vec::new();
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("cannot read the base's {UI_DIRECTORY}: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {UI_DIRECTORY}: {error}"))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    let mut found = Vec::new();
    for stem in names.iter().filter_map(|name| name.strip_suffix(".rs")) {
        let is_target_name =
            stem.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if stem.is_empty() || !is_target_name {
            return Err(format!("the base's {UI_DIRECTORY}/{stem}.rs is not a target name"));
        }
        let text = fs::read_to_string(directory.join(format!("{stem}.rs")))
            .map_err(|error| format!("cannot read {UI_DIRECTORY}/{stem}.rs: {error}"))?;
        let expected = format!("{UI_DIRECTORY}/{stem}.stderr");
        let stderr = super::source::read_optional(base, &expected)?.unwrap_or_default();
        let is_test = is_test_case(&text).map_err(|flags| {
            format!(
                "{UI_DIRECTORY}/{stem}.rs passes `{flags}`; the ratchet knows only `{TEST_FLAG}`"
            )
        })?;
        found.push(UiCase { stem: stem.to_owned(), is_test, text, stderr });
    }
    assert!(
        found.windows(2).all(|pair| matches!(pair, [first, second] if first.stem < second.stem)),
        "the cases are sorted by name, each once"
    );
    Ok(found)
}

/// True if the case asks to be compiled as a test harness. An error names flags the ratchet
/// cannot reproduce.
fn is_test_case(text: &str) -> Result<bool, String> {
    let mut is_test = false;
    for line in text.lines() {
        let Some(flags) = line.trim().strip_prefix(COMPILE_FLAGS) else {
            continue;
        };
        for flag in flags.split_whitespace() {
            if flag != TEST_FLAG {
                return Err(flags.trim().to_owned());
            }
            is_test = true;
        }
    }
    Ok(is_test)
}

/// The diagnostics that `cargo --message-format=json` printed for lints: messages with a lint
/// code and a primary span. Other lines are skipped.
#[must_use]
pub fn parse_json(text: &str) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    for line in text.lines().filter(|line| line.starts_with('{')) {
        let Ok(value) = json::parse(line) else {
            continue;
        };
        if value.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        if let Some(diagnostic) = value.get("message").and_then(diagnostic_of) {
            found.push(diagnostic);
        }
    }
    found
}

fn diagnostic_of(message: &Value) -> Option<Diagnostic> {
    let lint = message.get("code")?.get("code")?.as_str()?.to_owned();
    let level = message.get("level")?.as_str()?.to_owned();
    let spans = message.get("spans")?.as_array()?;
    let primary = spans.iter().find(|span| span.get("is_primary") == Some(&Value::Bool(true)))?;
    let number = |key: &str| match primary.get(key) {
        Some(Value::Number(digits)) => digits.parse::<u32>().ok(),
        _ => None,
    };
    let file = primary.get("file_name")?.as_str()?.to_owned();
    Some(Diagnostic {
        file,
        line: number("line_start")?,
        column: number("column_start")?,
        lint,
        level,
    })
}

/// The diagnostics with a location in compiler output written for people, such as a UI case's
/// `.stderr` or what `cargo dylint` prints.
#[must_use]
pub fn parse_located(text: &str) -> Vec<Located> {
    let mut found = Vec::new();
    let mut level: Option<&str> = None;
    for line in text.lines() {
        if line.starts_with("error: ") || line.starts_with("error[") {
            level = Some("error");
        } else if line.starts_with("warning: ") || line.starts_with("warning[") {
            level = Some("warning");
        } else if let Some(location) = line.trim_start().strip_prefix("--> ")
            && let Some(current) = level.take()
            && let Some((path, line_number, column)) = split_location(location)
        {
            found.push(Located { level: current.to_owned(), path, line: line_number, column });
        }
    }
    found
}

/// Splits `path:line:column`.
fn split_location(location: &str) -> Option<(String, u32, u32)> {
    let (rest, column) = location.trim().rsplit_once(':')?;
    let (path, line) = rest.rsplit_once(':')?;
    Some((path.to_owned(), line.parse().ok()?, column.parse().ok()?))
}

/// Runs the head's lint library on the base's UI cases, and the head's `cargo xtask dylint` on
/// each case planted as an example of a crate drawn from the head tree. Runs head code; judges
/// nothing.
///
/// # Errors
///
/// Returns what failed if the base's cases cannot be read or the scratch trees written.
pub fn observe(
    base: &Path,
    head: &Path,
    scratch: &Scratch,
    head_xtask: &Path,
    tools: &Tools<'_>,
    rng: &mut Rng,
) -> Result<Vec<Run>, String> {
    let cases = cases(base)?;
    if cases.is_empty() {
        return Ok(Vec::new());
    }
    let (name, placements) = plan(&cases, &plant_crates(scratch.root())?, rng);
    let package = tools.work.join("ui-head");
    write_package(&package, &name, &cases)
        .map_err(|error| format!("cannot write {}: {error}", package.display()))?;
    let (output, other) = run_library(tools, head, "head", &package)?;
    let library = Run {
        subject: UI_DIRECTORY.to_owned(),
        placement: String::new(),
        mode: "library".to_owned(),
        exit: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: other,
        files: BTreeMap::new(),
    };
    let mut runs = vec![library];
    for (case, placement) in cases.iter().zip(&placements) {
        runs.push(run_dylint(case, placement, scratch, head_xtask, tools)?);
    }
    Ok(runs)
}

/// The head package's random name, and where each case is planted as an example: a crate of
/// `crates` and a random file name, drawn from `rng`. The judge draws them again.
fn plan(cases: &[UiCase], crates: &[String], rng: &mut Rng) -> (String, Vec<String>) {
    let name = rng.stem();
    let placements: Vec<String> = cases
        .iter()
        .map(|_| {
            let crate_directory = rng.pick(crates);
            format!("{crate_directory}/examples/{}.rs", rng.stem())
        })
        .collect();
    assert_eq!(placements.len(), cases.len(), "one placement per case");
    (name, placements)
}

/// The crates of the tree at `root` that a case can be planted in: `crates/<name>` with a
/// manifest.
fn plant_crates(root: &Path) -> Result<Vec<String>, String> {
    let entries =
        super::tree::entries(root).map_err(|error| format!("cannot list the tree: {error}"))?;
    let crates: Vec<String> = entries
        .keys()
        .filter_map(|path| path.strip_suffix("/Cargo.toml"))
        .filter(|directory| directory.starts_with("crates/") && directory.matches('/').count() == 1)
        .map(str::to_owned)
        .collect();
    if crates.is_empty() {
        return Err("the head has no crate under crates/ to plant the lint cases in".to_owned());
    }
    Ok(crates)
}

/// Plants one case and runs the head's `cargo xtask dylint` on the scratch tree.
fn run_dylint(
    case: &UiCase,
    placement: &str,
    scratch: &Scratch,
    head_xtask: &Path,
    tools: &Tools<'_>,
) -> Result<Run, String> {
    let planted = scratch
        .plant(&PathBuf::from(placement), &as_example(case))
        .map_err(|error| format!("cannot plant {placement}: {error}"))?;
    let mut command = super::command(head_xtask, scratch.root());
    let _command = command
        .arg("dylint")
        .env("CARGO", &tools.cargo)
        .env("CARGO_TARGET_DIR", tools.work.join("end-to-end-target"));
    let output = command.output();
    planted.remove().map_err(|error| format!("cannot restore the scratch tree: {error}"))?;
    let output = output.map_err(|error| format!("cannot run the head's xtask: {error}"))?;
    Ok(Run {
        subject: format!("{UI_DIRECTORY}/{}.rs", case.stem),
        placement: placement.to_owned(),
        mode: "dylint".to_owned(),
        exit: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        files: BTreeMap::new(),
    })
}

/// Judges the recorded lint runs: the base's library, run here on the base's own cases, gives
/// what must be reported; the head's runs must report all of it.
///
/// # Errors
///
/// No verdict: a base without UI cases, a base library that does not reproduce the base's
/// `.stderr`, or a missing or unreadable run.
pub fn judge(
    base: &Path,
    head: &Path,
    observations: &Observations,
    tools: &Tools<'_>,
    rng: &mut Rng,
) -> Result<Vec<Finding>, String> {
    let cases = cases(base)?;
    if cases.is_empty() {
        return Err(format!("the base has no UI cases in {UI_DIRECTORY}"));
    }
    let (_name, placements) = plan(&cases, &plant_crates(head)?, rng);
    let expected = base_expectations(base, &cases, tools)?;
    let library = observations
        .runs_of(UI_DIRECTORY, "library")
        .next()
        .ok_or("the observations hold no run of the head's lint library")?;
    let reported = parse_json(&library.stdout);
    let errors = reported.iter().filter(|diagnostic| diagnostic.level == "error").count();
    exits_agree("the head's lint library", library.exit, errors)?;
    let mut findings = missing_diagnostics(&expected, &reported, &library.stderr);
    for (case, placement) in cases.iter().zip(&placements) {
        let file = format!("{}.rs", case.stem);
        let of_case: Vec<&Diagnostic> =
            expected.iter().filter(|diagnostic| diagnostic.file == file).collect();
        let subject = format!("{UI_DIRECTORY}/{file}");
        let run = observations
            .runs_of(&subject, "dylint")
            .find(|run| run.placement == *placement)
            .ok_or_else(|| {
                format!("no `xtask dylint` run of {subject} at {placement}, which the seed draws")
            })?;
        findings.extend(judge_dylint(run, &of_case)?);
    }
    Ok(findings)
}

/// The lint diagnostics of the base's own library on its cases, after checking that they
/// reproduce the base's `.stderr` files.
fn base_expectations(
    base: &Path,
    cases: &[UiCase],
    tools: &Tools<'_>,
) -> Result<Vec<Diagnostic>, String> {
    let package = tools.work.join("ui-base");
    write_package(&package, "nami-ratchet-ui", cases)
        .map_err(|error| format!("cannot write {}: {error}", package.display()))?;
    let (output, other) = run_library(tools, base, "base", &package)?;
    let all = parse_json(&String::from_utf8_lossy(&output.stdout));
    let declared = crate::tidy::lints::declared(base)
        .map_err(|error| format!("cannot read the base's lint declarations: {error}"))?;
    let names: BTreeSet<String> = declared.into_iter().map(|lint| lint.name).collect();
    let expected: Vec<Diagnostic> =
        all.iter().filter(|diagnostic| names.contains(&diagnostic.lint)).cloned().collect();
    reproduces_expectations(cases, &all, &expected).map_err(|problem| {
        format!("{problem}; the base's library output: {}", super::tail(&other, QUOTED_LINES))
    })?;
    Ok(expected)
}

/// Fails, for no verdict, unless the exit code of a dylint run agrees with the errors it
/// printed: 0 with none, an error code with some. An error exit without an error diagnostic is
/// a run that broke, and a clean exit beside an error diagnostic is output that cannot be
/// trusted; neither shows what the lints report.
fn exits_agree(what: &str, exit: Option<i32>, errors: usize) -> Result<(), String> {
    match exit {
        Some(0) if errors == 0 => Ok(()),
        Some(code) if code != 0 && errors > 0 => Ok(()),
        Some(0) => Err(format!("{what} exited 0 and printed {errors} error diagnostic(s)")),
        Some(code) => Err(format!(
            "{what} exited {code} without an error diagnostic, so it did not run the lints"
        )),
        None => Err(format!("{what} was killed")),
    }
}

/// Judges one end-to-end run against the base's diagnostics of its case.
fn judge_dylint(run: &Run, expected: &[&Diagnostic]) -> Result<Vec<Finding>, String> {
    let placement = &run.placement;
    let in_a_crate = placement.starts_with("crates/") && placement.contains("/examples/");
    if !in_a_crate || !is_plain_relative(Path::new(placement)) {
        return Err(format!("{} was planted at {placement}, not as an example", run.subject));
    }
    let printed = format!("{}\n{}", run.stdout, run.stderr);
    let located = parse_located(&printed);
    let errors = located.iter().filter(|diagnostic| diagnostic.level == "error").count();
    exits_agree(&format!("the head's `xtask dylint` on {}", run.subject), run.exit, errors)?;
    let found: BTreeSet<(String, u32, u32)> = located
        .into_iter()
        .filter(|located| located.path == *placement)
        .map(|located| (located.level, located.line, located.column))
        .collect();
    let mut findings = Vec::new();
    for diagnostic in expected {
        if found.contains(&(diagnostic.level.clone(), diagnostic.line, diagnostic.column)) {
            continue;
        }
        let place =
            format!("{UI_DIRECTORY}/{}:{}:{}", diagnostic.file, diagnostic.line, diagnostic.column);
        let message = format!(
            "planted at {placement} in the head tree, this case breaks `{}` here, and the \
             head's `cargo xtask dylint` does not report it; the PR changes which code dylint \
             checks (ADR-0022). Output: {}",
            diagnostic.lint,
            super::tail(&printed, 4)
        );
        findings.push(Finding::new(&place, Check::EndToEnd, message));
    }
    Ok(findings)
}

/// Writes a package named `name` with one target per UI case: a binary, or a test for a test
/// harness.
fn write_package(directory: &Path, name: &str, cases: &[UiCase]) -> std::io::Result<()> {
    assert!(!cases.is_empty(), "a package needs a target");
    if directory.exists() {
        fs::remove_dir_all(directory)?;
    }
    fs::create_dir_all(directory)?;
    let mut manifest: Vec<String> = vec![format!(
        "# Written by `cargo xtask ratchet`: the base's lint UI cases as targets.\n[package]\n\
         name = \"{name}\"\nversion = \"0.0.0\"\nedition = \"{UI_EDITION}\"\n\
         publish = false\nautobins = false\nautotests = false\nautoexamples = false\n\
         autobenches = false\n"
    )];
    for case in cases {
        let kind = if case.is_test { "test" } else { "bin" };
        let harness = if case.is_test { "" } else { "test = false\n" };
        let stem = &case.stem;
        manifest.push(format!("\n[[{kind}]]\nname = \"{stem}\"\npath = \"{stem}.rs\"\n{harness}"));
        fs::write(directory.join(format!("{stem}.rs")), &case.text)?;
    }
    manifest.push("\n[workspace]\n".to_owned());
    fs::write(directory.join("Cargo.toml"), manifest.concat())
}

/// Compiles the package with the lint library of the tree at `root`. Returns cargo's output,
/// and its non-JSON output for the findings.
///
/// Each side builds in its own target directory: the two libraries have the same file name, and
/// cargo would take one for the other. The library's fingerprint goes into `DYLINT_RUSTFLAGS`,
/// as `cargo xtask dylint` does, because cargo does not check the package again when only the
/// library changed, and replays the diagnostics of the last library instead (ADR-0018).
fn run_library(
    tools: &Tools<'_>,
    root: &Path,
    side: &str,
    package: &Path,
) -> Result<(std::process::Output, String), String> {
    let fingerprint = crate::dylint::fingerprint(root)
        .map_err(|error| format!("cannot read the {side}'s lint library: {error}"))?;
    let mut command = super::command(&tools.cargo, package);
    let _command = command
        .args(["dylint", "--path"])
        .arg(root.join("lints"))
        .args(["--all", "--manifest-path"])
        .arg(package.join("Cargo.toml"))
        .args(["--", "--bins", "--tests", "--keep-going", "--message-format=json"])
        .env("CARGO_TARGET_DIR", tools.work.join(format!("lint-target-{side}")))
        .env("DYLINT_RUSTFLAGS", crate::dylint::rustflags(None, fingerprint));
    let output = super::output(&mut command, "`cargo dylint` on the UI cases")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let other = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        stdout.lines().filter(|line| !line.starts_with('{')).collect::<Vec<_>>().join("\n")
    );
    Ok((output, other))
}

/// Checks that the base's library, run through this harness, gives what the base's `.stderr`
/// files say: each located error there is a diagnostic here, and each lint diagnostic here
/// is located there. Otherwise the harness and the UI test disagree, and nothing can be judged.
fn reproduces_expectations(
    cases: &[UiCase],
    all: &[Diagnostic],
    lints: &[Diagnostic],
) -> Result<(), String> {
    for case in cases {
        let file = format!("{}.rs", case.stem);
        let written: BTreeSet<(String, u32, u32)> = parse_located(&case.stderr)
            .into_iter()
            .map(|located| (located.level, located.line, located.column))
            .collect();
        let compiled: BTreeSet<(String, u32, u32)> = all
            .iter()
            .filter(|diagnostic| diagnostic.file == file)
            .map(|diagnostic| (diagnostic.level.clone(), diagnostic.line, diagnostic.column))
            .collect();
        let unreproduced =
            written.iter().find(|entry| entry.0 == "error" && !compiled.contains(*entry));
        let unwritten =
            lints.iter().filter(|diagnostic| diagnostic.file == file).find(|diagnostic| {
                !written.contains(&(diagnostic.level.clone(), diagnostic.line, diagnostic.column))
            });
        if let Some((_, line, column)) = unreproduced {
            return Err(format!(
                "the base's library does not report {UI_DIRECTORY}/{file}:{line}:{column}, \
                 which the base's .stderr expects"
            ));
        }
        if let Some(diagnostic) = unwritten {
            return Err(format!(
                "the base's library reports `{}` at {UI_DIRECTORY}/{file}:{}:{}, which the \
                 base's .stderr does not expect",
                diagnostic.lint, diagnostic.line, diagnostic.column
            ));
        }
    }
    Ok(())
}

/// One finding for each base diagnostic that the head's library does not report.
fn missing_diagnostics(
    expected: &[Diagnostic],
    actual: &[Diagnostic],
    head_output: &str,
) -> Vec<Finding> {
    let actual: BTreeSet<&Diagnostic> = actual.iter().collect();
    let mut findings: Vec<Finding> = expected
        .iter()
        .filter(|diagnostic| !actual.contains(diagnostic))
        .map(|diagnostic| {
            let place = format!(
                "{UI_DIRECTORY}/{}:{}:{}",
                diagnostic.file, diagnostic.line, diagnostic.column
            );
            let message = format!(
                "the base's `{0}` reports an {1} here and the head's lint library does not, so \
                 the PR loosens `{0}`; make it report this case again (ADR-0022)",
                diagnostic.lint, diagnostic.level
            );
            Finding::new(&place, Check::LintCorpus, message)
        })
        .collect();
    if actual.is_empty() && !expected.is_empty() {
        let message = format!(
            "the head's lint library reported nothing on the base's UI cases; does it build? {}",
            super::tail(head_output, QUOTED_LINES)
        );
        findings.push(Finding::new(UI_DIRECTORY, Check::LintCorpus, message));
    }
    findings
}

/// The text of a UI case as an example program, which `cargo xtask dylint` checks with
/// `--all-targets`. A case compiled as a test harness has no `main`, and gets an empty one
/// after its last line, so that no line moves. It is not planted below `tests/`, because the
/// lints treat a whole integration test as test code. Its `#[test]` functions and
/// `#[cfg(test)]` items are left out as an example; the base's diagnostics are outside them.
fn as_example(case: &UiCase) -> String {
    let has_main = case.text.lines().any(|line| line.starts_with("fn main("));
    if !case.is_test || has_main {
        return case.text.clone();
    }
    let separator = if case.text.ends_with('\n') { "" } else { "\n" };
    let text = format!("{}{separator}fn main() {{}}\n", case.text);
    assert!(text.starts_with(&case.text), "the case's lines stay where they are");
    text
}

#[cfg(test)]
mod tests {
    use super::{
        Diagnostic, Located, UiCase, as_example, cases, is_test_case, judge_dylint,
        missing_diagnostics, parse_json, parse_located, reproduces_expectations,
    };
    use crate::tidy::scratch::Scratch;

    fn diagnostic(file: &str, line: u32, column: u32, lint: &str, level: &str) -> Diagnostic {
        Diagnostic {
            file: file.to_owned(),
            line,
            column,
            lint: lint.to_owned(),
            level: level.to_owned(),
        }
    }

    // Goal: lint diagnostics are read from cargo's JSON at their primary span; messages without
    // a code, other reasons and broken lines are skipped.
    #[test]
    fn json_diagnostics_are_read() {
        let span = |primary: bool, file: &str, line: u32, column: u32| {
            format!(
                "{{\"is_primary\":{primary},\"file_name\":\"{file}\",\"line_start\":{line},\
                 \"column_start\":{column}}}"
            )
        };
        let spans = format!("[{},{}]", span(false, "a.rs", 1, 1), span(true, "main.rs", 4, 9));
        let message = |code: &str, level: &str| {
            format!(
                "{{\"reason\":\"compiler-message\",\"message\":{{\"code\":{code},\
                 \"level\":\"{level}\",\"spans\":{spans}}}}}"
            )
        };
        let text = [
            message("{\"code\":\"nami_recursion\",\"explanation\":null}", "error"),
            message("null", "error"),
            "{\"reason\":\"compiler-artifact\"}".to_owned(),
            "{broken".to_owned(),
            "   Compiling nami".to_owned(),
        ]
        .join("\n");
        assert_eq!(parse_json(&text), [diagnostic("main.rs", 4, 9, "nami_recursion", "error")]);
    }

    // Goal: located diagnostics are read from human output, with the level of the header they
    // follow; a summary without a location takes none.
    #[test]
    fn located_diagnostics_are_read() {
        let text = "warning: function cannot return\n  --> $DIR/main.rs:48:5\n   |\n\
                    error: `direct` is recursive\n  --> $DIR/main.rs:4:1\n\
                    error: aborting due to 1 previous error\n\
                    error[E0425]: cannot find value\n --> crates/solver/examples/x.rs:2:3\n\
                    note: a note\n  --> elsewhere.rs:1:1\n";
        let located = |level: &str, path: &str, line: u32, column: u32| Located {
            level: level.to_owned(),
            path: path.to_owned(),
            line,
            column,
        };
        assert_eq!(
            parse_located(text),
            [
                located("warning", "$DIR/main.rs", 48, 5),
                located("error", "$DIR/main.rs", 4, 1),
                located("error", "crates/solver/examples/x.rs", 2, 3),
            ]
        );
    }

    // Goal: cases are read in order with their expected output; `--test` makes a test harness,
    // and any other compiler flag or a name that is not a target name is an error.
    #[test]
    fn ui_cases_are_read() {
        let base = Scratch::new("ratchet-ui-cases");
        base.write("lints/ui/main.rs", "fn main() {}\n");
        base.write("lints/ui/main.stderr", "error: x\n");
        base.write("lints/ui/sums.rs", "// compile-flags: --test\n");
        let found = cases(base.root()).unwrap();
        let summary: Vec<(&str, bool, &str)> = found
            .iter()
            .map(|case| (case.stem.as_str(), case.is_test, case.stderr.as_str()))
            .collect();
        assert_eq!(summary, [("main", false, "error: x\n"), ("sums", true, "")]);
        assert_eq!(
            is_test_case("// compile-flags: --edition 2021"),
            Err("--edition 2021".to_owned())
        );
        base.write("lints/ui/Bad-Name.rs", "");
        assert!(cases(base.root()).is_err(), "not a target name");
    }

    // Goal: a test-harness case becomes an example by an empty `main` after its last line; a
    // case with a `main`, or one that is not a test harness, is planted as it is.
    #[test]
    fn cases_become_examples() {
        let case = |text: &str, is_test: bool| UiCase {
            stem: "a".to_owned(),
            is_test,
            text: text.to_owned(),
            stderr: String::new(),
        };
        assert_eq!(as_example(&case("fn a() {}\n", true)), "fn a() {}\nfn main() {}\n");
        assert_eq!(as_example(&case("fn a() {}", true)), "fn a() {}\nfn main() {}\n");
        assert_eq!(as_example(&case("fn main() {}\n", true)), "fn main() {}\n");
        assert_eq!(as_example(&case("fn a() {}\n", false)), "fn a() {}\n");
    }

    // Goal: the base's run is trusted only if it reproduces the base's `.stderr`: a located
    // error there that the run lacks, or a lint diagnostic of the run not written there, stops
    // the ratchet.
    #[test]
    fn the_harness_must_reproduce_the_base() {
        let case = UiCase {
            stem: "main".to_owned(),
            is_test: false,
            text: String::new(),
            stderr: "error: `a` is recursive\n  --> $DIR/main.rs:4:1\n\
                     warning: other\n  --> $DIR/main.rs:9:1\n"
                .to_owned(),
        };
        let lint = diagnostic("main.rs", 4, 1, "nami_recursion", "error");
        let all = [lint.clone(), diagnostic("main.rs", 9, 1, "unconditional_recursion", "warning")];
        let one = std::slice::from_ref(&case);
        assert_eq!(reproduces_expectations(one, &all, std::slice::from_ref(&lint)), Ok(()));
        let error = reproduces_expectations(one, &[], &[]).expect_err("nothing compiled");
        assert!(error.contains("does not report lints/ui/main.rs:4:1"), "{error}");
        let extra = diagnostic("main.rs", 7, 1, "nami_recursion", "error");
        let unwritten = reproduces_expectations(one, &all, &[lint, extra]).expect_err("unwritten");
        assert!(unwritten.contains("reports `nami_recursion` at lints/ui/main.rs:7:1"), "{error}");
    }

    // Goal: a missing diagnostic is a finding at its place; a head that reports nothing at all
    // gets one more finding that quotes its output, and nothing expected means no finding.
    #[test]
    fn missing_diagnostics_are_findings() {
        let expected = [diagnostic("main.rs", 4, 1, "nami_recursion", "error")];
        let places = |actual: &[Diagnostic]| -> Vec<String> {
            let findings = missing_diagnostics(&expected, actual, "error: cannot build");
            findings.into_iter().map(|finding| finding.place).collect()
        };
        assert_eq!(places(&[]), ["lints/ui/main.rs:4:1", "lints/ui"]);
        let other = diagnostic("main.rs", 9, 1, "nami_recursion", "error");
        assert_eq!(places(std::slice::from_ref(&other)), ["lints/ui/main.rs:4:1"]);
        assert_eq!(places(&expected), Vec::<String>::new());
        assert!(missing_diagnostics(&[], &[], "").is_empty(), "nothing expected");
    }

    // Goal: an end-to-end run counts only as an example of a crate at a plain path, and only
    // when its exit code agrees with the errors it printed: not when it was killed, exited 0
    // beside an error, or failed without one. There, each expected diagnostic must be at the
    // planted path.
    #[test]
    fn end_to_end_runs_are_read_strictly() {
        let run =
            |placement: &str, exit: Option<i32>, stderr: &str| crate::ratchet::observations::Run {
                subject: "lints/ui/main.rs".to_owned(),
                placement: placement.to_owned(),
                mode: "dylint".to_owned(),
                exit,
                stdout: String::new(),
                stderr: stderr.to_owned(),
                files: std::collections::BTreeMap::new(),
            };
        let expected = diagnostic("main.rs", 4, 1, "nami_recursion", "error");
        let at = "crates/a/examples/r1.rs";
        let printed = "error: recursive\n  --> crates/a/examples/r1.rs:4:1\n";
        assert_eq!(judge_dylint(&run(at, Some(101), printed), &[&expected]), Ok(Vec::new()));
        assert_eq!(judge_dylint(&run(at, Some(0), ""), &[&expected]).unwrap().len(), 1);
        assert!(judge_dylint(&run(at, None, printed), &[&expected]).is_err(), "killed");
        let clean_exit = judge_dylint(&run(at, Some(0), printed), &[&expected]).unwrap_err();
        assert!(clean_exit.contains("exited 0 and printed 1 error"), "{clean_exit}");
        let broke = judge_dylint(&run(at, Some(101), "error: could not compile"), &[&expected]);
        assert!(broke.unwrap_err().contains("exited 101 without an error diagnostic"));
        assert!(judge_dylint(&run("crates/a/src/r1.rs", Some(101), printed), &[]).is_err());
        assert!(judge_dylint(&run("docs/examples/r1.rs", Some(101), printed), &[]).is_err());
        assert!(judge_dylint(&run("crates/../examples/r1.rs", Some(101), printed), &[]).is_err());
    }
}
