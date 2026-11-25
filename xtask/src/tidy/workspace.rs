//! Rules about the workspace as a whole: the `#[expect]` budget, assertion density, snapshots.

use super::{AssertionStats, Violation};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// File holding the maximum number of `#[expect]` attributes. Only humans raise it (ADR-0006).
pub const EXPECT_BUDGET_FILE: &str = "xtask/expect-budget.txt";

/// Counts that only mean something once every file has been seen.
pub struct Tally {
    /// `#[expect]` attributes in the whole tree.
    pub expect_count: u32,
    /// Assertion statistics per nami crate, keyed by `crates/<name>`.
    pub stats_by_crate: BTreeMap<PathBuf, AssertionStats>,
}

impl Tally {
    /// A tally of no files.
    pub fn new() -> Self {
        Self { expect_count: 0, stats_by_crate: BTreeMap::new() }
    }

    /// Adds the counts of one Rust source file.
    pub fn add(&mut self, relative: &Path, expect_count: u32, stats: AssertionStats) {
        self.expect_count += expect_count;
        if let Some(crate_root) = crate_root(relative) {
            self.stats_by_crate.entry(crate_root).or_insert_with(AssertionStats::zero).add(stats);
        }
    }
}

/// Returns `crates/<name>` for files inside a nami crate.
pub fn crate_root(relative: &Path) -> Option<PathBuf> {
    let mut components = relative.components();
    let first = components.next()?;
    let second = components.next()?;
    (first.as_os_str() == "crates").then(|| Path::new(&first).join(second))
}

/// True for an insta snapshot that has not been reviewed.
pub fn is_snapshot_draft(relative: &Path) -> bool {
    relative.to_string_lossy().ends_with(".snap.new")
}

/// The violation for an unreviewed snapshot.
pub fn snapshot_draft_violation(relative: &Path) -> Violation {
    assert!(is_snapshot_draft(relative), "only snapshot drafts are reported as such");
    let message = "unreviewed insta snapshot; review it with `cargo insta review` or delete it, \
                   never accept snapshots blindly (ADR-0006)";
    Violation::new(relative, 0, "snapshot-draft", message)
}

/// The `#[expect]` count must equal the budget: higher needs a human, lower ratchets down.
/// The budget is read from `policy_root`, which is the tree itself unless tidy checks a tree
/// against another revision's policy.
pub fn check_expect_budget(policy_root: &Path, expect_count: u32) -> io::Result<Vec<Violation>> {
    let path = Path::new(EXPECT_BUDGET_FILE);
    let text = fs::read_to_string(policy_root.join(path))?;
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

/// Assertion density: at least two assertions per non-trivial function, per crate (ADR-0004).
pub fn density_violations(stats_by_crate: &BTreeMap<PathBuf, AssertionStats>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (crate_root, stats) in stats_by_crate {
        if !stats.meets_density() {
            let message = format!(
                "{} assertions across {} non-trivial functions; the minimum is 2 per function on \
                 average. Assert arguments, results and invariants (ADR-0004)",
                stats.assertions, stats.functions
            );
            violations.push(Violation::new(crate_root, 0, "assertion-density", message));
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::{Tally, check_expect_budget, crate_root, density_violations, is_snapshot_draft};
    use crate::tidy::AssertionStats;
    use crate::tidy::scratch::Scratch;
    use std::path::Path;

    // Goal: unreviewed insta snapshots are recognised by their extension.
    #[test]
    fn snapshot_drafts_are_recognised() {
        assert!(is_snapshot_draft(Path::new("tests/snapshots/a.snap.new")));
        assert!(!is_snapshot_draft(Path::new("tests/snapshots/a.snap")));
    }

    // Goal: crate roots are recognised from paths alone.
    #[test]
    fn crate_roots_are_recognised() {
        let lib = Path::new("crates/base/src/lib.rs");
        assert_eq!(crate_root(lib).as_deref(), Some(Path::new("crates/base")));
        assert_eq!(crate_root(Path::new("xtask/src/tidy.rs")), None);
        assert_eq!(crate_root(Path::new("crates")), None, "the directory itself is no crate");
    }

    // Goal: the tally sums per crate, ignores tooling for density and counts every `#[expect]`.
    #[test]
    fn tally_sums_per_crate() {
        let mut tally = Tally::new();
        let stats = AssertionStats { functions: 1, assertions: 1 };
        tally.add(Path::new("crates/a/src/lib.rs"), 1, stats);
        tally.add(Path::new("crates/a/src/b.rs"), 0, stats);
        tally.add(Path::new("xtask/src/main.rs"), 2, stats);
        assert_eq!(tally.expect_count, 3);
        let expected = AssertionStats { functions: 2, assertions: 2 };
        assert_eq!(tally.stats_by_crate.get(Path::new("crates/a")), Some(&expected));
        assert_eq!(tally.stats_by_crate.len(), 1, "tooling has no density requirement");
        let violations = density_violations(&tally.stats_by_crate);
        assert_eq!(violations.len(), 1, "one assertion per function is too few");
        assert_eq!(violations[0].path, Path::new("crates/a"));
    }

    // Goal: the count must equal the budget exactly: one above and one below both fail, and a
    // budget file that is not a number is itself a violation.
    #[test]
    fn expect_budget_is_an_exact_match() {
        let scratch = Scratch::new("expect-budget");
        scratch.write(super::EXPECT_BUDGET_FILE, "1\n");
        let rules = |count: u32| -> Vec<&'static str> {
            let violations = check_expect_budget(scratch.root(), count).unwrap();
            violations.iter().map(|violation| violation.rule).collect()
        };
        assert_eq!(rules(1), Vec::<&str>::new(), "the count equals the budget");
        assert_eq!(rules(2), ["expect-budget"], "one more needs a human");
        assert_eq!(rules(0), ["expect-budget"], "one fewer ratchets the budget down");
        scratch.write(super::EXPECT_BUDGET_FILE, "many\n");
        assert_eq!(rules(1), ["expect-budget"], "the budget must be a number");
    }
}
