//! The rule registry: every tidy rule, in one table.
//!
//! A rule exists once it has a row here. [`super::Violation::new`] refuses an id that is not
//! registered, and the fixture tests refuse a registered rule that has no firing and no passing
//! example (`xtask/fixtures/tidy/`). So a rule cannot be added without a description, and it
//! cannot go silent without a test failing.
//!
//! The tables are plain Rust constants, so that adding a rule needs no parser and the compiler
//! checks the rows.

/// How much of the tree a rule needs to see before it can decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleKind {
    /// Decided from one file, plus fixed context read from the root (such as the ADR index).
    /// The edit hook runs these rules on the file that was just edited.
    Source,
    /// Needs the whole tree: counts and comparisons across files.
    Workspace,
}

/// One tidy rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Identifier printed as `error[<id>]`, and the stem of the rule's fixture files.
    pub id: &'static str,
    /// How much of the tree the rule needs.
    pub kind: RuleKind,
    /// One line that says what the rule requires.
    pub summary: &'static str,
    /// Where the reason is written down: an ADR or a `TIGER_STYLE.md` section.
    pub reference: &'static str,
}

const fn source(id: &'static str, summary: &'static str, reference: &'static str) -> Rule {
    Rule { id, kind: RuleKind::Source, summary, reference }
}

const fn workspace(id: &'static str, summary: &'static str, reference: &'static str) -> Rule {
    Rule { id, kind: RuleKind::Workspace, summary, reference }
}

/// Every tidy rule that is written as code. Keep the rows sorted by id.
pub const RULES: &[Rule] = &[
    source("adr-index", "Every ADR is listed in `docs/design/README.md`.", "ADR-0000"),
    source("adr-section", "Every ADR has the six required sections.", "ADR-0000"),
    source("adr-status", "Every ADR has a valid status line.", "ADR-0000"),
    workspace(
        "assertion-density",
        "Each nami crate averages at least two assertions per non-trivial function.",
        "ADR-0004",
    ),
    source(
        "async",
        "No `async` functions or blocks: functions run to completion.",
        "TIGER_STYLE.md, Control flow",
    ),
    source(
        "debt-marker",
        "No unfinished-work markers in Rust sources; unfinished work is a task.",
        "TIGER_STYLE.md, Zero technical debt",
    ),
    source(
        "default",
        "No `Default` derives or impls in nami crates: every field is spelled out.",
        "TIGER_STYLE.md, External events and control flow",
    ),
    workspace(
        "expect-budget",
        "The number of `#[expect]` attributes equals `xtask/expect-budget.txt`.",
        "ADR-0006",
    ),
    source(
        "function-length",
        "Functions are at most 70 physical lines, closures included.",
        "TIGER_STYLE.md, Scope and size",
    ),
    source("ignore-reason", "`#[ignore]` carries a reason.", "ADR-0006"),
    source(
        "kernel-indirection",
        "No `dyn` trait objects and no function pointers in `crates/kernels`.",
        "ADR-0003",
    ),
    source(
        "line-length",
        "Lines in Rust sources are at most 100 columns.",
        "TIGER_STYLE.md, Style by the numbers",
    ),
    source("parse", "Every Rust source file parses.", "ADR-0006"),
    source(
        "runtime-dependency",
        "Runtime and build dependencies of nami crates are `libm` and workspace crates only.",
        "ADR-0001",
    ),
    source("snapshot-draft", "No unreviewed insta snapshots (`*.snap.new`).", "ADR-0006"),
    source(
        "test-goal",
        "Every test starts with a comment that states its goal.",
        "TIGER_STYLE.md, Comments, docs and commits",
    ),
    source(
        "unasserted-function",
        "A function in a nami crate longer than 10 lines contains an assertion.",
        "ADR-0004",
    ),
    source(
        "unbounded-loop",
        "No `loop` and no `while`: every loop has a fixed upper bound.",
        "TIGER_STYLE.md, Put a limit on everything",
    ),
    source("unsafe", "`unsafe` appears only in `crates/alloc-guard`.", "ADR-0014"),
    source(
        "usize",
        "`usize` appears in public signatures, fields and public items only in `base::index`.",
        "ADR-0002",
    ),
    source(
        "vec-field",
        "No `Vec` struct fields in nami crates: fixed-size storage is `Box<[T]>`.",
        "ADR-0003",
    ),
];

/// True if some table in this registry defines the tidy rule `id`.
#[must_use]
pub fn is_registered(id: &str) -> bool {
    rule_ids().any(|registered| registered == id)
}

/// The id of every tidy rule, from every table.
pub fn rule_ids() -> impl Iterator<Item = &'static str> {
    RULES.iter().map(|rule| rule.id)
}

/// The ids of the rules of one kind.
pub fn rule_ids_of(kind: RuleKind) -> impl Iterator<Item = &'static str> {
    RULES.iter().filter(move |rule| rule.kind == kind).map(|rule| rule.id)
}

#[cfg(test)]
mod tests {
    use super::{RULES, RuleKind, is_registered, rule_ids, rule_ids_of};

    // Goal: ids are unique and sorted, so that a rule has one row and the table reads like an
    // index.
    #[test]
    fn rule_ids_are_sorted_and_unique() {
        let ids: Vec<&str> = RULES.iter().map(|rule| rule.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted, "RULES must be sorted by id, without duplicates");
        let mut all: Vec<&str> = rule_ids().collect();
        let count = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), count, "a rule id appears in two tables");
    }

    // Goal: every row is complete: a lowercase dashed id, a summary sentence and a reference.
    #[test]
    fn rows_are_complete() {
        for rule in RULES {
            let is_dashed =
                rule.id.chars().all(|character| character.is_ascii_lowercase() || character == '-');
            assert!(is_dashed && !rule.id.is_empty(), "id `{}` must be lowercase-dashed", rule.id);
            assert!(rule.summary.ends_with('.'), "summary of `{}` must be a sentence", rule.id);
            let cites = rule.reference.starts_with("ADR-")
                || rule.reference.starts_with("TIGER_STYLE.md, ");
            assert!(cites, "reference of `{}` must be an ADR or a TIGER_STYLE section", rule.id);
        }
    }

    // Goal: lookups see exactly the registered ids, and kinds partition the table.
    #[test]
    fn lookups_follow_the_table() {
        assert!(is_registered("line-length"), "a registered rule is found");
        assert!(!is_registered("no-such-rule"), "an unknown id is rejected");
        assert!(!is_registered(""), "the empty id is rejected");
        let source = rule_ids_of(RuleKind::Source).count();
        let workspace = rule_ids_of(RuleKind::Workspace).count();
        assert_eq!(source + workspace, RULES.len());
        assert!(rule_ids_of(RuleKind::Workspace).any(|id| id == "expect-budget"));
        assert!(!rule_ids_of(RuleKind::Source).any(|id| id == "expect-budget"));
    }
}
