//! Rules about the workspace as a whole: dependencies, ADRs, the `#[expect]` budget, snapshots.

use super::Violation;
use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::Path;

/// File holding the maximum number of `#[expect]` attributes. Only humans raise it (ADR-0006).
const EXPECT_BUDGET_FILE: &str = "xtask/expect-budget.txt";

/// Directory of the ADRs and their index (ADR-0000).
const ADR_DIRECTORY: &str = "docs/design";

/// Sections every ADR must have (ADR-0000).
const ADR_SECTIONS: [&str; 6] = [
    "## Context",
    "## Decision",
    "## Enforcement",
    "## Consequences",
    "## Alternatives considered",
    "## Evidence",
];

/// Valid beginnings of an ADR status line (ADR-0000).
const ADR_STATUSES: [&str; 4] = [
    "- Status: Proposed",
    "- Status: Accepted",
    "- Status: Superseded by ADR-",
    "- Status: Deprecated",
];

/// True for an insta snapshot that has not been reviewed.
pub fn is_snapshot_draft(relative: &Path) -> bool {
    relative.to_string_lossy().ends_with(".snap.new")
}

/// The violation for an unreviewed snapshot.
pub fn snapshot_draft_violation(relative: &Path) -> Violation {
    let message = "unreviewed insta snapshot; review it with `cargo insta review` or delete it, \
                   never accept snapshots blindly (ADR-0006)";
    Violation::new(relative, 0, "snapshot-draft", message)
}

/// The `#[expect]` count must equal the budget: higher needs a human, lower ratchets down.
pub fn check_expect_budget(root: &Path, expect_count: u32) -> io::Result<Vec<Violation>> {
    let path = Path::new(EXPECT_BUDGET_FILE);
    let text = fs::read_to_string(root.join(path))?;
    let Ok(budget) = text.trim().parse::<u32>() else {
        let message = "the budget file must contain a single non-negative integer";
        return Ok(vec![Violation::new(path, 1, "expect-budget", message)]);
    };
    let message = match expect_count.cmp(&budget) {
        Ordering::Greater => format!(
            "{expect_count} `#[expect]` attributes, but the budget is {budget}; fix the code \
             instead of suppressing the lint. Only a human may raise the budget (ADR-0006)"
        ),
        Ordering::Less => {
            format!("only {expect_count} `#[expect]` attributes remain; lower the budget to match")
        }
        Ordering::Equal => return Ok(Vec::new()),
    };
    Ok(vec![Violation::new(path, 1, "expect-budget", message)])
}

/// Runtime dependencies of nami crates: only `libm` and other nami crates (ADR-0001).
pub fn check_manifests(root: &Path) -> io::Result<Vec<Violation>> {
    let mut violations = Vec::new();
    let mut crate_directories: Vec<_> = fs::read_dir(root.join("crates"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<_>>()?;
    crate_directories.sort();
    for directory in crate_directories {
        let manifest = directory.join("Cargo.toml");
        if manifest.is_file() {
            let relative = manifest.strip_prefix(root).expect("manifest is below the root");
            let text = fs::read_to_string(&manifest)?;
            violations.extend(check_manifest(relative, &text));
        }
    }
    Ok(violations)
}

/// Checks the normal and build dependencies of one manifest. Dev-dependencies are governed by
/// the cargo-deny allow-list instead.
pub fn check_manifest(relative: &Path, text: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut in_runtime_section = false;
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.starts_with('[') {
            in_runtime_section = is_runtime_dependency_section(line);
            continue;
        }
        let key = line.split(['=', '.']).next().unwrap_or_default().trim();
        if in_runtime_section
            && !key.is_empty()
            && !key.starts_with('#')
            && !is_allowed_runtime(key)
        {
            let line_number = u32::try_from(line_index + 1).expect("manifest has < 2^32 lines");
            let message = format!(
                "runtime dependency `{key}`; the only runtime dependency allowed is `libm`. \
                 Adding a crate needs an ADR approved by a human (ADR-0001)"
            );
            violations.push(Violation::new(relative, line_number, "runtime-dependency", message));
        }
    }
    violations
}

fn is_runtime_dependency_section(header: &str) -> bool {
    let name = header.trim_start_matches('[').trim_end_matches(']');
    let is_dependencies = name == "dependencies" || name.ends_with(".dependencies");
    let is_build = name == "build-dependencies" || name.ends_with(".build-dependencies");
    is_dependencies || is_build
}

fn is_allowed_runtime(key: &str) -> bool {
    key == "libm" || key.starts_with("nami-")
}

/// Every ADR is indexed, has a valid status and has the required sections (ADR-0000).
pub fn check_adrs(root: &Path) -> io::Result<Vec<Violation>> {
    let directory = root.join(ADR_DIRECTORY);
    let index = fs::read_to_string(directory.join("README.md"))?;
    let mut names: Vec<String> = fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()?;
    names.sort();
    let mut violations = Vec::new();
    for name in names.iter().filter(|name| is_adr_file_name(name)) {
        let relative = Path::new(ADR_DIRECTORY).join(name);
        let text = fs::read_to_string(root.join(&relative))?;
        violations.extend(check_adr(&relative, name, &text, &index));
    }
    Ok(violations)
}

/// Checks one ADR against the index.
pub fn check_adr(relative: &Path, name: &str, text: &str, index: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    if !index.contains(&format!("({name})")) {
        let message = "ADR is missing from docs/design/README.md; add it to the index (ADR-0000)";
        violations.push(Violation::new(relative, 0, "adr-index", message));
    }
    let has_status =
        text.lines().any(|line| ADR_STATUSES.iter().any(|status| line.starts_with(status)));
    if !has_status {
        let message = "ADR has no valid status line; use `- Status: Proposed`, `Accepted`, \
                       `Superseded by ADR-NNNN` or `Deprecated` (ADR-0000)";
        violations.push(Violation::new(relative, 0, "adr-status", message));
    }
    for section in ADR_SECTIONS.iter().filter(|section| !text.lines().any(|line| line == **section))
    {
        let message = format!("ADR is missing the `{section}` section (ADR-0000)");
        violations.push(Violation::new(relative, 0, "adr-section", message));
    }
    violations
}

fn is_adr_file_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let has_number = bytes.iter().take(4).filter(|byte| byte.is_ascii_digit()).count() == 4;
    let is_markdown = Path::new(name).extension().is_some_and(|extension| extension == "md");
    has_number && bytes.get(4) == Some(&b'-') && is_markdown
}

#[cfg(test)]
mod tests {
    use super::{check_adr, check_manifest, is_adr_file_name};
    use std::path::Path;

    fn rules(violations: &[super::Violation]) -> Vec<&'static str> {
        violations.iter().map(|violation| violation.rule).collect()
    }

    // Goal: only libm and nami crates may be runtime dependencies; dev-dependencies are exempt.
    #[test]
    fn runtime_dependency_rule_fires() {
        let manifest = "[package]\nname = \"x\"\n\n[dependencies]\nlibm.workspace = true\n\
                        nami-core = { path = \"../nami-core\" }\nserde = \"1\"\n\n\
                        [dev-dependencies]\nproptest = \"1\"\n";
        let violations = check_manifest(Path::new("Cargo.toml"), manifest);
        assert_eq!(rules(&violations), ["runtime-dependency"]);
        assert_eq!(violations[0].line, 7);
    }

    // Goal: build-dependencies and target-specific dependencies count as runtime.
    #[test]
    fn build_and_target_dependencies_are_runtime() {
        let manifest =
            "[build-dependencies]\ncc = \"1\"\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n";
        assert_eq!(rules(&check_manifest(Path::new("Cargo.toml"), manifest)).len(), 2);
    }

    // Goal: an ADR must be indexed, have a status and have every section.
    #[test]
    fn adr_rules_fire() {
        let sections = "## Context\n## Decision\n## Enforcement\n## Consequences\n\
                        ## Alternatives considered\n## Evidence\n";
        let good = format!("# ADR-0001: x\n\n- Status: Accepted\n\n{sections}");
        let index = "| [0001](0001-x.md) |";
        assert_eq!(
            rules(&check_adr(Path::new("a"), "0001-x.md", &good, index)),
            Vec::<&str>::new()
        );
        let bad = "# ADR-0002: y\n\n- Status: Maybe\n## Context\n";
        let violations = check_adr(Path::new("a"), "0002-y.md", bad, index);
        assert_eq!(rules(&violations)[..2], ["adr-index", "adr-status"]);
        assert_eq!(violations.len(), 7, "index, status and five missing sections");
    }

    // Goal: unreviewed insta snapshots are recognised by their extension.
    #[test]
    fn snapshot_drafts_are_recognised() {
        assert!(super::is_snapshot_draft(Path::new("tests/snapshots/a.snap.new")));
        assert!(!super::is_snapshot_draft(Path::new("tests/snapshots/a.snap")));
    }

    // Goal: ADR files are recognised by their four-digit prefix.
    #[test]
    fn adr_file_names_are_recognised() {
        assert!(is_adr_file_name("0013-lint-table-refinements.md"));
        assert!(!is_adr_file_name("README.md"));
        assert!(!is_adr_file_name("13-x.md"));
        assert!(!is_adr_file_name("0013_x.md"), "the number must be followed by a dash");
        assert!(!is_adr_file_name("abcd-x.md"), "the prefix must be digits");
        assert!(!is_adr_file_name("0013-x.txt"), "ADRs are Markdown");
    }
}
