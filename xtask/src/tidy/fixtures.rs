//! Fixture tests: every source rule fires on its firing example and stays silent on its passing
//! one (ADR-0006).
//!
//! A rule `<id>` has two files in `xtask/fixtures/tidy/`: `<id>.fires.fixture` and
//! `<id>.passes.fixture`. The first line of each names the virtual path that the file is
//! checked under, which decides the rules that apply, in a comment that suits the content:
//!
//! ```text
//! // path: crates/kernels/src/example.rs
//! # path: crates/demo/Cargo.toml
//! <!-- path: docs/design/0001-example.md -->
//! ```
//!
//! The extension is not `.rs` on purpose: rustfmt, the edit hook and tidy's own walk then leave
//! the deliberate violations alone. Context such as the ADR index comes from the real workspace.

use super::fixture_virtual_path as virtual_path;
use super::registry::{self, RuleKind};
use super::{FIXTURE_DIRECTORY, FIXTURE_PATH_MARKER as PATH_MARKER, check_single_file};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory of tidy's fixtures, below [`FIXTURE_DIRECTORY`].
const TIDY_FIXTURES: &str = "tidy";

/// The two fixtures of every rule: one that fires and one that passes.
const OUTCOMES: [&str; 2] = ["fires", "passes"];

fn fixture_directory() -> PathBuf {
    crate::workspace_root().join(FIXTURE_DIRECTORY).join(TIDY_FIXTURES)
}

/// Runs the source rules on the fixture `<id>.<outcome>.fixture` and returns the rules that
/// fired, or a description of what is wrong with the fixture.
fn rules_fired(id: &str, outcome: &str) -> Result<BTreeSet<&'static str>, String> {
    let name = format!("{id}.{outcome}.fixture");
    let text = fs::read_to_string(fixture_directory().join(&name))
        .map_err(|error| format!("{name}: cannot read the fixture: {error}"))?;
    let first_line = text.lines().next().unwrap_or_default();
    let path = virtual_path(first_line)
        .ok_or_else(|| format!("{name}: the first line must be a comment `{PATH_MARKER}<path>`"))?;
    let violations = check_single_file(&crate::workspace_root(), &path, &text)
        .map_err(|error| format!("{name}: cannot read the context: {error}"))?;
    Ok(violations.iter().map(|violation| violation.rule).collect())
}

/// Checks one fixture of every source rule against the rules that `expected` says must fire on
/// it. Returns one line per fixture that is missing, malformed or behaves differently.
fn problems(outcome: &str, expected: impl Fn(&'static str) -> BTreeSet<&'static str>) -> String {
    let mut problems = Vec::new();
    for id in registry::rule_ids_of(RuleKind::Source) {
        let expected = expected(id);
        match rules_fired(id, outcome) {
            Ok(fired) if fired == expected => {}
            Ok(fired) => {
                let name = format!("{id}.{outcome}.fixture");
                problems.push(format!("{name}: expected {expected:?}, but {fired:?} fired"));
            }
            Err(problem) => problems.push(problem),
        }
    }
    problems.join("\n")
}

// Goal: every source rule in the registry has a firing fixture, and the fixture produces that
// rule and no other, so each fixture shows one rule.
#[test]
fn every_source_rule_fires_on_its_fixture() {
    let problems = problems("fires", |id| BTreeSet::from([id]));
    assert_eq!(problems, "", "firing fixtures that do not show exactly their rule");
}

// Goal: every source rule in the registry has a passing fixture, and no rule at all fires on
// it, so each fixture shows code that is acceptable as it stands.
#[test]
fn every_source_rule_passes_on_its_fixture() {
    let problems = problems("passes", |_id| BTreeSet::new());
    assert_eq!(problems, "", "passing fixtures on which a rule fires");
}

// Goal: the fixture directory holds exactly the two fixtures of every source rule: no leftovers
// of removed rules and no misspelt names that no test would read.
#[test]
fn fixture_directory_matches_the_registry() {
    let expected: BTreeSet<String> = registry::rule_ids_of(RuleKind::Source)
        .flat_map(|id| OUTCOMES.map(|outcome| format!("{id}.{outcome}.fixture")))
        .collect();
    let found: BTreeSet<String> = fs::read_dir(fixture_directory())
        .expect("the fixture directory exists")
        .map(|entry| entry.expect("entry is readable").file_name().to_string_lossy().into_owned())
        .collect();
    let missing: Vec<&String> = expected.difference(&found).collect();
    let unexpected: Vec<&String> = found.difference(&expected).collect();
    assert!(missing.is_empty(), "source rules without a fixture: {missing:?}");
    assert!(unexpected.is_empty(), "fixture files that belong to no source rule: {unexpected:?}");
}

// Goal: the virtual path is read from each of the three comment styles, and a first line that
// does not name a path is rejected.
#[test]
fn virtual_paths_are_read_from_comments() {
    let path = |line: &str| virtual_path(line);
    assert_eq!(path("// path: crates/a/src/b.rs").as_deref(), Some(Path::new("crates/a/src/b.rs")));
    assert_eq!(
        path("# path: crates/a/Cargo.toml").as_deref(),
        Some(Path::new("crates/a/Cargo.toml"))
    );
    assert_eq!(path("<!-- path: docs/a.md -->").as_deref(), Some(Path::new("docs/a.md")));
    assert_eq!(path("path: crates/a/src/b.rs"), None, "the path must be in a comment");
    assert_eq!(path("// crates/a/src/b.rs"), None, "the marker is required");
    assert_eq!(path("// path: "), None, "the path must not be empty");
}
