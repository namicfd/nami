//! Which paths the ratchet vouches for: the rule paths (ADR-0022).
//!
//! A rule path holds code or data that decides *what a rule reports on given input*: the tidy
//! rule tables and the modules that implement source rules, the dylint lints, their fixtures and
//! UI cases, and the generated `docs/RULES.md`. Everything that decides *which files are
//! checked, how results become exit codes, or how CI and hooks run* is the trusted core, and so
//! is the code of the workspace rules, which the corpus does not cover.
//!
//! ADR-0022 proposes that `.github/CODEOWNERS` leave exactly these paths without an owner. Until
//! then they stay owned, and the ratchet runs in shadow mode.

use crate::pr::Pattern;

/// The rule paths, in CODEOWNERS form: `/file` or `/directory/`.
pub const RULE_PATHS: [&str; 12] = [
    "/docs/RULES.md",
    "/lints/src/",
    "/lints/ui/",
    "/xtask/fixtures/tidy/",
    "/xtask/src/tidy/docs.rs",
    "/xtask/src/tidy/lints.rs",
    "/xtask/src/tidy/manifest.rs",
    "/xtask/src/tidy/registry.rs",
    "/xtask/src/tidy/rust_source.rs",
    "/xtask/src/tidy/style_guide.rs",
    "/xtask/src/tidy/text.rs",
    "/xtask/src/tidy/tokens.rs",
];

/// Directories whose changes make the ratchet run its checks, even when no rule path changed:
/// a change to the core of the tooling can loosen a rule as well, and the owner who reviews it
/// gets the ratchet's findings.
const TOOLING_DIRECTORIES: [&str; 2] = ["xtask/", "lints/"];

/// Changes below this directory, or to the dylint wrapper, make the ratchet run the lint checks.
const LINT_DIRECTORY: &str = "lints/";

/// The xtask module that runs dylint over the workspace.
const DYLINT_WRAPPER: &str = "xtask/src/dylint.rs";

/// The rule paths as patterns.
#[must_use]
pub fn rule_patterns() -> Vec<Pattern> {
    let patterns: Vec<Pattern> = RULE_PATHS
        .iter()
        .map(|path| crate::pr::parse_pattern(path).expect("rule paths are in CODEOWNERS form"))
        .collect();
    assert_eq!(patterns.len(), RULE_PATHS.len(), "every rule path is a pattern");
    patterns
}

/// True if `path`, relative to the root with `/` separators, is a rule path.
#[must_use]
pub fn is_rule_path(path: &str) -> bool {
    assert!(!path.starts_with('/'), "paths are relative to the root");
    rule_patterns().iter().any(|pattern| pattern.matches(path))
}

/// True if a change to `path` can change what tidy or dylint reports.
#[must_use]
pub fn is_tooling(path: &str) -> bool {
    is_rule_path(path) || TOOLING_DIRECTORIES.iter().any(|directory| path.starts_with(directory))
}

/// True if a change to `path` can change what the dylint lints report.
#[must_use]
pub fn is_lint_tooling(path: &str) -> bool {
    path.starts_with(LINT_DIRECTORY) || path == DYLINT_WRAPPER
}

#[cfg(test)]
mod tests {
    use super::{RULE_PATHS, is_lint_tooling, is_rule_path, is_tooling, rule_patterns};

    // Goal: the rule paths are what decides what a rule reports; the walk, the command line,
    // the fixture harness, the workspace rules and the lint library's manifests are not.
    #[test]
    fn rule_paths_are_the_rule_definitions() {
        let rules = [
            "docs/RULES.md",
            "lints/src/lib.rs",
            "lints/src/float_reduction.rs",
            "lints/ui/main.rs",
            "lints/ui/main.stderr",
            "xtask/fixtures/tidy/usize.fires.fixture",
            "xtask/src/tidy/registry.rs",
            "xtask/src/tidy/rust_source.rs",
            "xtask/src/tidy/tokens.rs",
        ];
        for path in rules {
            assert!(is_rule_path(path), "{path} is a rule path");
        }
        let core = [
            "xtask/src/tidy.rs",
            "xtask/src/tidy/cli.rs",
            "xtask/src/tidy/fixtures.rs",
            "xtask/src/tidy/workspace.rs",
            "xtask/src/tidy/rules_doc.rs",
            "xtask/src/ratchet.rs",
            "xtask/src/dylint.rs",
            "xtask/expect-budget.txt",
            "lints/Cargo.toml",
            "lints/rust-toolchain.toml",
            "docs/TIGER_STYLE.md",
            ".github/CODEOWNERS",
            "xtask/fixtures/other.fixture",
        ];
        for path in core {
            assert!(!is_rule_path(path), "{path} is trusted core");
        }
        assert!(RULE_PATHS.is_sorted(), "keep the rule paths sorted");
        assert_eq!(rule_patterns().len(), RULE_PATHS.len());
    }

    // Goal: the checks run for any change to the tooling, and the lint checks for a change to
    // the lint library or the wrapper that runs it, and for nothing else.
    #[test]
    fn tooling_changes_select_the_checks() {
        assert!(is_tooling("docs/RULES.md") && is_tooling("xtask/src/hook.rs"));
        assert!(is_tooling("lints/Cargo.lock"));
        assert!(!is_tooling("crates/kernels/src/lib.rs") && !is_tooling("docs/design/README.md"));
        assert!(is_lint_tooling("lints/ui/main.rs") && is_lint_tooling("xtask/src/dylint.rs"));
        assert!(!is_lint_tooling("xtask/src/tidy/registry.rs"));
    }
}
