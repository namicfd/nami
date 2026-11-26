//! Rules about the workspace as a whole: the `#[expect]` budget, assertion density, snapshots.
//!
//! The counting that the budget adds up lives here too, with its own token walk, not with the
//! token rules: workspace rules have no fixtures, so the ratchet (pending, #57) could not prove
//! that a change to them only tightens, and their code stays with the code owner (ADR-0022,
//! pending in #55).

use super::{AssertionStats, Violation};
use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// File holding the maximum number of `#[expect]` attributes. Only humans raise it (ADR-0006).
pub const EXPECT_BUDGET_FILE: &str = "xtask/expect-budget.txt";

/// The attribute argument that suppresses a lint: `expect(lint, reason = "...")`.
const EXPECT: &str = "expect";

/// Upper bound on the token groups in one file, so the walk is bounded.
const TOKEN_GROUPS_MAX: u32 = 4_000_000;

/// Counts the lint suppressions in one file: every `expect(...)` inside an attribute, inner or
/// outer, at any depth. That includes `#[cfg_attr(.., expect(..))]` and attributes inside macro
/// bodies. A file that does not lex counts as zero; the `parse` rule reports it.
#[must_use]
pub fn expect_count(text: &str) -> u32 {
    let Ok(stream) = text.parse::<TokenStream>() else {
        return 0;
    };
    let mut count: u32 = 0;
    for sequence in sequences(stream) {
        for attribute in attributes(&sequence) {
            count += expect_calls(attribute.stream());
        }
    }
    count
}

/// The bracket groups of the attributes in one sequence: `#[...]` and `#![...]`.
fn attributes(sequence: &[TokenTree]) -> Vec<&Group> {
    let mut found = Vec::new();
    let mut after_hash = false;
    for tree in sequence {
        let is_bang = after_hash && is_punct(tree, '!');
        if let TokenTree::Group(group) = tree
            && after_hash
            && group.delimiter() == Delimiter::Bracket
        {
            found.push(group);
        }
        after_hash = is_punct(tree, '#') || is_bang;
    }
    found
}

/// Counts `expect(...)` in the tokens of one attribute, at any depth.
fn expect_calls(attribute: TokenStream) -> u32 {
    let mut count: u32 = 0;
    for sequence in sequences(attribute) {
        for pair in sequence.windows(2) {
            if let [TokenTree::Ident(ident), TokenTree::Group(group)] = pair
                && ident == EXPECT
                && group.delimiter() == Delimiter::Parenthesis
            {
                count += 1;
            }
        }
    }
    count
}

/// Every token sequence in `stream`: the stream itself and the content of every group inside
/// it, at any depth, with an explicit work list (no recursion, ADR-0006).
fn sequences(stream: TokenStream) -> Vec<Vec<TokenTree>> {
    let mut pending = vec![stream];
    let mut found = Vec::new();
    for _ in 0..TOKEN_GROUPS_MAX {
        let Some(next) = pending.pop() else {
            assert!(!found.is_empty(), "the stream itself is a sequence");
            return found;
        };
        let sequence: Vec<TokenTree> = next.into_iter().collect();
        for tree in &sequence {
            if let TokenTree::Group(group) = tree {
                pending.push(group.stream());
            }
        }
        found.push(sequence);
    }
    panic!("more than {TOKEN_GROUPS_MAX} token groups in one stream");
}

fn is_punct(tree: &TokenTree, character: char) -> bool {
    matches!(tree, TokenTree::Punct(punct) if punct.as_char() == character)
}

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
    use super::is_snapshot_draft;
    use super::{Tally, check_expect_budget, crate_root, density_violations, expect_count};
    use crate::tidy::AssertionStats;
    use crate::tidy::scratch::Scratch;
    use std::path::Path;

    // Goal: suppressions are counted as tokens: plain, inner, inside `cfg_attr`, several in one
    // attribute, and inside a macro body.
    #[test]
    fn expect_attributes_are_counted_on_tokens() {
        assert_eq!(expect_count("#[expect(dead_code, reason = \"x\")]\nfn a() {}"), 1);
        assert_eq!(expect_count("#![expect(dead_code, reason = \"x\")]"), 1);
        assert_eq!(expect_count("# [ expect ( dead_code ) ]\nfn a() {}"), 1, "spacing is free");
        let hidden = "#[cfg_attr(all(), expect(clippy::indexing_slicing, reason = \"x\"))]";
        assert_eq!(expect_count(hidden), 1, "cfg_attr does not hide a suppression");
        let two = "#[cfg_attr(test, expect(a, reason = \"x\"), expect(b, reason = \"y\"))]";
        assert_eq!(expect_count(two), 2, "each suppression counts");
        let in_macro = "macro_rules! m { () => { #[expect(dead_code)] fn a() {} }; }";
        assert_eq!(expect_count(in_macro), 1, "macro bodies are counted");
        let deep = "mod tests { impl A { fn b() { { #[cfg_attr(all(), cfg_attr(all(), \
                    expect(dead_code)))] let _c = 1; } } } }";
        assert_eq!(expect_count(deep), 1, "any depth");
    }

    // Goal: only suppressions count: the method `expect`, text in strings and comments, other
    // attributes and a file that does not lex all count as zero.
    #[test]
    fn expect_count_ignores_everything_else() {
        assert_eq!(expect_count("fn a() { b().expect(\"reason\"); }"), 0, "a method call");
        assert_eq!(expect_count("const A: &str = \"#[expect(dead_code)]\";"), 0, "a string");
        assert_eq!(expect_count("/// #[expect(dead_code)]\nfn a() {}"), 0, "a doc comment");
        assert_eq!(expect_count("// #[expect(dead_code)]\nfn a() {}"), 0, "a comment");
        assert_eq!(expect_count("#[should_panic(expected = \"x\")]\nfn a() {}"), 0, "expected");
        assert_eq!(expect_count("#[derive(Debug)]\nstruct A;"), 0, "another attribute");
        assert_eq!(expect_count("#[expect]\nfn a() {}"), 0, "no parenthesised lint");
        assert_eq!(expect_count("fn a() { expect(1); }"), 0, "a function outside an attribute");
        assert_eq!(expect_count("fn a() { m![expect(1)]; }"), 0, "a macro's brackets");
        let after_attribute = "#[derive(Debug)] static A: [u8; expect(3)] = [0; 3];";
        assert_eq!(expect_count(after_attribute), 0, "brackets later in the same sequence");
        assert_eq!(expect_count("fn a( {"), 0, "a file that does not lex");
    }

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
