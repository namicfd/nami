//! The rule registry: every tidy rule, in one place.
//!
//! A rule exists once it has a row here. [`super::Violation::new`] refuses an id that is not
//! registered, and the fixture tests refuse a registered rule that has no firing and no passing
//! example (`xtask/fixtures/tidy/`). So a rule cannot be added without a description, and it
//! cannot go silent without a test failing.
//!
//! There are three tables of rules:
//!
//! - [`RULES`]: rules written as code somewhere in `tidy`;
//! - [`PATH_BANS`] and [`MACRO_BANS`]: declarative bans. A new ban is one row plus two fixtures,
//!   with no other code.
//!
//! [`PLANNED`] lists the mechanisms that the documents promise and nobody has built. The style
//! guide tags a rule `[planned:<id>]` instead of claiming a check that does not exist, and each
//! entry has a task, so the backlog is visible and cannot be forgotten.
//!
//! The tables are plain Rust constants, so that adding a rule needs no parser and the compiler
//! checks the rows. `docs/RULES.md` is generated from them (`cargo xtask rules --write`).

use super::Scope;
use std::path::Path;

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
    /// What the rule sees and what it misses: its exact reach, its exemptions and its known
    /// gaps, or empty if the summary says it all. This text lives here and in `docs/RULES.md`,
    /// not in `docs/TIGER_STYLE.md`, which states only the rule, its reason and its tag, so that
    /// tightening a rule changes only the rule paths (ADR-0022; issue #53).
    pub detail: &'static str,
    /// Where the reason is written down: an ADR, a `TIGER_STYLE.md` section or a document.
    pub reference: &'static str,
}

/// Every tidy rule that is written as code. Keep the rows sorted by id. The rows are plain
/// literals, so that the ratchet can read them from a pull request's source without running
/// it (ADR-0022).
pub const RULES: &[Rule] = &[
    Rule {
        id: "adr-id",
        kind: RuleKind::Workspace,
        summary: "Every file in `docs/design/` but the index is named `YYYYMMDD-HHMM-<slug>.md`, \
                  after a UTC minute, or carries the number of an ADR written before IDs were \
                  minutes.",
        detail: "The minute must exist in the calendar, from 2026 on. The legacy numbers are \
                 0000 to 0043 without the gaps 0016 and 0024 to 0031. The slug is lower-case \
                 words joined by single dashes.",
        reference: "ADR-20261128-1856",
    },
    Rule {
        id: "adr-index",
        kind: RuleKind::Source,
        summary: "Every ADR is listed in `docs/design/README.md`.",
        detail: "",
        reference: "ADR-0000",
    },
    Rule {
        id: "adr-index-doc",
        kind: RuleKind::Workspace,
        summary: "`docs/design/README.md` is what `cargo xtask adr index --write` generates from \
                  the ADRs.",
        detail: "Skipped by a foreign gate, like `rules-doc`: another revision's generator \
                 writes another index.",
        reference: "ADR-20261128-1856",
    },
    Rule {
        id: "adr-number",
        kind: RuleKind::Source,
        summary: "No two ADR files share an ID, and an ADR's title repeats the ID of its file \
                  name.",
        detail: "Same-minute clashes between branches show here once both are merged; the \
                 `adr clashes` check on each pull request fails them earlier.",
        reference: "ADR-0000",
    },
    Rule {
        id: "adr-reference",
        kind: RuleKind::Workspace,
        summary: "Every `ADR-` followed by a digit names an ADR in `docs/design/`.",
        detail: "Reads the sources, documents and configuration (by extension, and `justfile` \
                 and `CODEOWNERS`) that tidy walks, below `docs/` and in the root `.claude/` \
                 without its worktrees. Tidy's fixtures and scratch files such as CI's \
                 `pr.diff` are not read. `ADR-NNNN` and other placeholders without a digit are \
                 no reference.",
        reference: "ADR-20261128-1856",
    },
    Rule {
        id: "adr-section",
        kind: RuleKind::Source,
        summary: "Every ADR has the six required sections.",
        detail: "",
        reference: "ADR-0000",
    },
    Rule {
        id: "adr-status",
        kind: RuleKind::Source,
        summary: "Every ADR has a valid status line.",
        detail: "",
        reference: "ADR-0000",
    },
    Rule {
        id: "adr-summary",
        kind: RuleKind::Workspace,
        summary: "Every ADR has a `- Summary:` line in its header: the one line its index row \
                  shows.",
        detail: "The placeholder summary that `cargo xtask adr new` writes is rejected too, so \
                 an unedited template cannot merge.",
        reference: "ADR-20261128-1856",
    },
    Rule {
        id: "assertion-density",
        kind: RuleKind::Workspace,
        summary: "Each nami crate averages at least two assertions per non-trivial function.",
        detail: "",
        reference: "ADR-0004",
    },
    Rule {
        id: "async",
        kind: RuleKind::Source,
        summary: "No `async` functions or blocks: functions run to completion.",
        detail: "",
        reference: "TIGER_STYLE.md, Control flow",
    },
    Rule {
        id: "debt-marker",
        kind: RuleKind::Source,
        summary: "No unfinished-work markers in Rust sources; unfinished work is a task.",
        detail: "",
        reference: "TIGER_STYLE.md, Zero technical debt",
    },
    Rule {
        id: "default",
        kind: RuleKind::Source,
        summary: "No `Default` derives or impls in nami crates: every field is spelled out.",
        detail: "",
        reference: "TIGER_STYLE.md, External events and control flow",
    },
    Rule {
        id: "dependency-override",
        kind: RuleKind::Source,
        summary: "The workspace manifest has no `[patch]` or `[replace]`: no dependency is swapped \
                  for other source code under its own name.",
        detail: "`cargo deny` rejects a git override but not a path override.",
        reference: "ADR-0001",
    },
    Rule {
        id: "doc-tag",
        kind: RuleKind::Source,
        summary: "Every mechanism tag in `docs/TIGER_STYLE.md` has a known form and names a tidy \
                  rule, a lint that is switched on, or a planned mechanism that exists.",
        detail: "",
        reference: "TIGER_STYLE.md, How rules are enforced",
    },
    Rule {
        id: "doc-tool-name",
        kind: RuleKind::Source,
        summary: "The rules of `docs/TIGER_STYLE.md` do not name a checking tool in their prose: \
                  the tag names the mechanism, and what it sees and misses is in the registry and \
                  in this document.",
        detail: "Read from the first `## ` heading on, outside fenced code blocks, line by \
                 line and ignoring case. It reports the words `tidy`, `clippy`, `dylint`, \
                 `rustfmt`, `rustc`, `lints`, `nextest`, `miri`, `cargo-deny` and \
                 `cargo-mutants`, and the phrases `cargo deny`, `cargo mutants` and `the lint`, \
                 as whole words in the prose outside code spans. A dash ends a word, so \
                 `clippy-clean` is reported. A code span counts when it runs that tool: the \
                 name alone or followed by arguments, after any `VAR=value` words, and after a \
                 runner (`just`, `cargo` or `cargo xtask`) and the options that precede the tool \
                 (`+nightly`, `--locked`), such as `cargo xtask tidy` or `just dylint`; so a tag, \
                 `clippy.toml` and `clippy::float_cmp` are not reported. `just` alone is left \
                 out because it is an everyday English word. A rule line with an odd number of \
                 backticks is reported too, because a code span that runs across lines would \
                 swap code and prose. Known false positives, which have to be reworded: tidy as \
                 an English adjective, and lints as a verb. Not seen: a phrase broken across two \
                 lines, and prose that describes a check without naming its tool, such as a \
                 list of the syntax a rule reports. A foreign gate (`--policy`) runs this rule \
                 too, because it reads only the style guide's text.",
        reference: "TIGER_STYLE.md, How rules are enforced",
    },
    Rule {
        id: "expect-budget",
        kind: RuleKind::Workspace,
        summary: "The number of `expect(...)` lint suppressions equals \
                  `xtask/expect-budget.txt`.",
        detail: "Counted on tokens, so that `cfg_attr` and macro bodies are included.",
        reference: "ADR-0006",
    },
    Rule {
        id: "float-eq-in-eq-fn",
        kind: RuleKind::Source,
        summary: "In nami crates outside tests, no `==` or `!=` with a float operand in a function \
                  whose name contains `eq` or is `ne` or `is_nan`, where clippy's `float_cmp` is \
                  silent.",
        detail: "Without types, a float operand is a float literal, an `f32`/`f64` constant such \
                 as `f64::NAN`, a cast to `f32`/`f64`, or a parameter, `let` or closure parameter \
                 declared `f32`/`f64` or an array or slice of them (or a reference to one), \
                 indexed or not, until a `let`, closure, `match` arm or `for` rebinds the name. \
                 Stricter than clippy on purpose: an exact `== 0.0` is reported too. Not seen: \
                 fields, `Option<f64>`, untyped locals, call results, macro arguments. A float \
                 parameter rebound by `if let` or `while let` is a false positive.",
        reference: "ADR-0006",
    },
    Rule {
        id: "function-length",
        kind: RuleKind::Source,
        summary: "Functions are at most 70 physical lines, closures included.",
        detail: "",
        reference: "TIGER_STYLE.md, Scope and size",
    },
    Rule {
        id: "ignore-reason",
        kind: RuleKind::Source,
        summary: "`#[ignore]` carries a reason.",
        detail: "",
        reference: "ADR-0006",
    },
    Rule {
        id: "kernel-indirection",
        kind: RuleKind::Source,
        summary: "No `dyn` trait objects and no function pointers in `crates/kernels`.",
        detail: "",
        reference: "ADR-0003",
    },
    Rule {
        id: "line-length",
        kind: RuleKind::Source,
        summary: "Lines in Rust sources are at most 100 columns.",
        detail: "rustfmt cannot wrap long string literals or comments, so tidy checks the limit.",
        reference: "TIGER_STYLE.md, Style by the numbers",
    },
    Rule {
        id: "magic-tolerance",
        kind: RuleKind::Source,
        summary: "In nami crates outside tests, a float literal in (0, 1e-6] appears only in the \
                  initialiser of a `const` item that has a doc comment.",
        detail: "",
        reference: "TIGER_STYLE.md, Floating point",
    },
    Rule {
        id: "parse",
        kind: RuleKind::Source,
        summary: "Every Rust source file parses.",
        detail: "",
        reference: "ADR-0006",
    },
    Rule {
        id: "plan-files",
        kind: RuleKind::Workspace,
        summary: "Every plan directory under `docs/exec-plans/` holds both `plan.md` and \
                  `tasks.json`.",
        detail: "",
        reference: "ADR-0011",
    },
    Rule {
        id: "plan-section",
        kind: RuleKind::Workspace,
        summary: "Every task has a `### <id>` section in its plan's `plan.md`.",
        detail: "",
        reference: "ADR-0011",
    },
    Rule {
        id: "plan-stage",
        kind: RuleKind::Workspace,
        summary: "A plan under `completed/` has no unfinished task.",
        detail: "",
        reference: "ADR-0011",
    },
    Rule {
        id: "planned-task",
        kind: RuleKind::Workspace,
        summary: "Every planned mechanism has a task with the same id in a `tasks.json` under \
                  `docs/exec-plans/active/`.",
        detail: "",
        reference: "ADR-0018",
    },
    Rule {
        id: "rules-doc",
        kind: RuleKind::Workspace,
        summary: "`docs/RULES.md` matches what `cargo xtask rules` generates from the registry.",
        detail: "",
        reference: "ADR-0018",
    },
    Rule {
        id: "runtime-dependency",
        kind: RuleKind::Source,
        summary: "Runtime and build dependencies of nami crates are `libm` and workspace crates \
                  only, and none is renamed with `package = \"...\"`.",
        detail: "",
        reference: "ADR-0001",
    },
    Rule {
        id: "snapshot-draft",
        kind: RuleKind::Source,
        summary: "No unreviewed insta snapshots (`*.snap.new`).",
        detail: "",
        reference: "ADR-0006",
    },
    Rule {
        id: "task-graph",
        kind: RuleKind::Workspace,
        summary: "Task ids are unique, every dependency exists, no task passes before its \
                  dependencies, and the dependencies form no cycle.",
        detail: "",
        reference: "ADR-0011",
    },
    Rule {
        id: "task-owner-paths",
        kind: RuleKind::Workspace,
        summary: "Two unfinished tasks that can run at the same time own no common path.",
        detail: "",
        reference: "ADR-0011",
    },
    Rule {
        id: "tasks-json",
        kind: RuleKind::Workspace,
        summary: "Every `tasks.json` is valid JSON with one `tasks` array, and every task has \
                  exactly the six fields of the schema.",
        detail: "",
        reference: "ADR-0011",
    },
    Rule {
        id: "test-goal",
        kind: RuleKind::Source,
        summary: "Every test starts with a comment that states its goal.",
        detail: "",
        reference: "TIGER_STYLE.md, Comments, docs and commits",
    },
    Rule {
        id: "unasserted-function",
        kind: RuleKind::Source,
        summary: "A function in a nami crate longer than 10 lines contains an assertion.",
        detail: "",
        reference: "ADR-0004",
    },
    Rule {
        id: "unbounded-loop",
        kind: RuleKind::Source,
        summary: "No `loop`, no `while`, and no `for` over a range without an end or an endless \
                  `core::iter` source: every loop has a fixed upper bound.",
        detail: "Every `loop` and `while` is reported. A `for` is reported when its iterable, \
                 under parentheses, references and any adaptor, starts from a range without an \
                 end (`start..`), from `cycle()` (also as `Iterator::cycle`), or from `repeat`, \
                 `repeat_with`, `from_fn` or `successors` called by a bare name or through a path \
                 that ends in `iter::`, and no `take` follows that source. A `zip` is endless only \
                 if both sides are, a `chain` if either side is. A bare `from_fn` imported from \
                 elsewhere, such as `core::array::from_fn`, is reported too: call it by a \
                 qualified path. An iterable that takes more than 256 steps to walk, counting each \
                 expression and each combine, is reported. Not \
                 seen, and for review: any other iterator that never ends, such as a custom one, \
                 one held in a variable, or one returned by a function.",
        reference: "TIGER_STYLE.md, Put a limit on everything",
    },
    Rule {
        id: "unsafe",
        kind: RuleKind::Source,
        summary: "`unsafe` appears only in `crates/alloc-guard`.",
        detail: "",
        reference: "ADR-0014",
    },
    Rule {
        id: "usize",
        kind: RuleKind::Source,
        summary: "In nami crates, `usize` appears in function signatures, fields, public constants \
                  and statics, and type aliases only in `base::index`.",
        detail: "Trait impls and const generics are exempt.",
        reference: "ADR-0002",
    },
    Rule {
        id: "vec-field",
        kind: RuleKind::Source,
        summary: "No `Vec` anywhere inside a field type in nami crates: fixed-size storage is \
                  `Box<[T]>`.",
        detail: "",
        reference: "ADR-0003",
    },
];

/// Which files a ban applies to, before its `allowed` list is consulted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BanScope {
    /// Every Rust source file that tidy walks, tooling included.
    Everywhere,
    /// Only nami's own crates under `crates/`.
    NamiCrates,
}

impl BanScope {
    /// True if a ban with this scope applies to a file in `scope`.
    #[must_use]
    pub fn includes(self, scope: Scope) -> bool {
        self == Self::Everywhere || scope.is_nami()
    }
}

/// A ban on every path that starts with one segment: `libm::sqrt(x)`, `use libm::sqrt;`, a type
/// `libm::Foo`. It also applies inside macro invocations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathBan {
    /// Rule identifier, as for [`Rule::id`].
    pub id: &'static str,
    /// The banned leading path segment.
    pub segment: &'static str,
    /// Files or directories, relative to the root, where the path is allowed.
    pub allowed: &'static [&'static str],
    /// Which files the ban applies to.
    pub scope: BanScope,
    /// The violation message: what is wrong, how to fix it, and where the reason is.
    pub message: &'static str,
}

/// A ban on invoking macros by name. It also applies inside other macro invocations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacroBan {
    /// Rule identifier, as for [`Rule::id`].
    pub id: &'static str,
    /// The banned macro names, without the `!`.
    pub names: &'static [&'static str],
    /// Files or directories, relative to the root, where the macros are allowed.
    pub allowed: &'static [&'static str],
    /// Which files the ban applies to.
    pub scope: BanScope,
    /// The violation message: what is wrong, how to fix it, and where the reason is.
    pub message: &'static str,
}

/// Banned leading path segments. Keep the rows sorted by id.
pub const PATH_BANS: &[PathBan] = &[PathBan {
    id: "direct-libm",
    segment: "libm",
    allowed: &["crates/kernels/src/math.rs"],
    scope: BanScope::Everywhere,
    message: "direct use of `libm`; call a wrapper in `kernels::math`, or add one there: the \
              wrapper asserts the domain, so an invalid argument fails loudly (ADR-0005)",
}];

/// Banned macros. Keep the rows sorted by id.
pub const MACRO_BANS: &[MacroBan] = &[MacroBan {
    id: "debug-assert-placement",
    names: &["debug_assert", "debug_assert_eq", "debug_assert_ne"],
    allowed: &["crates/kernels"],
    scope: BanScope::Everywhere,
    message: "`debug_assert!` outside `kernels`; use `assert!`. `debug_assert!` is only for \
              per-cell checks inside hot loops in `kernels` (ADR-0004)",
}];

/// A mechanism that the documents promise and nobody has built yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    /// Identifier used in `[planned:<id>]` tags and as the id of the task that will build it.
    pub id: &'static str,
    /// The intended mechanism: `tidy`, `dylint`, `test`, `ci`, `codeowners` or `docs`.
    pub mechanism: &'static str,
    /// One line that says what the mechanism will check.
    pub description: &'static str,
    /// Where the promise is written down: an ADR or a `TIGER_STYLE.md` section.
    pub reference: &'static str,
}

const fn planned(
    id: &'static str,
    mechanism: &'static str,
    description: &'static str,
    reference: &'static str,
) -> Planned {
    Planned { id, mechanism, description, reference }
}

/// Mechanisms that are promised but not built. Keep the rows sorted by id. When one is built,
/// delete its row, retag the style guide and move its task to `docs/exec-plans/completed/`.
pub const PLANNED: &[Planned] = &[
    planned(
        "alloc-after-init",
        "test",
        "A counting-allocator test around the time loop proves that nothing allocates after init. \
         The counting allocator (`crates/alloc-guard`) already fails any test that allocates \
         inside `assert_no_allocations`; wrapping the time loop in it is still to do.",
        "ADR-0003",
    ),
    planned(
        "clippy-fixtures",
        "test",
        "One planted violation per clippy ban and configured lint proves that each still fires \
         after a toolchain bump.",
        "ADR-0006",
    ),
    planned(
        "determinism-hash",
        "ci",
        "CI hashes the output of a canonical problem set and compares Linux, macOS and a \
         same-seed rerun.",
        "ADR-0005",
    ),
    planned(
        "doc-example-sync",
        "test",
        "The code blocks in `docs/TIGER_STYLE.md` are compiled and linted, so the examples cannot \
         drift from the rules.",
        "ADR-0013",
    ),
    planned(
        "floor-activation-count",
        "test",
        "A floor or limiter that is part of a scheme counts its activations, and a reference \
         test fails on an unexpected count.",
        "TIGER_STYLE.md, Physical admissibility",
    ),
    planned(
        "glossary",
        "docs",
        "`docs/glossary.md` records the one meaning of each word that could be overloaded.",
        "TIGER_STYLE.md, Naming",
    ),
    planned(
        "recursion-cross-crate",
        "dylint",
        "Detect call cycles that cross crates through a generic callback; `nami_recursion` \
         checks each crate on its own.",
        "ADR-0006",
    ),
    planned(
        "reference-suite",
        "test",
        "The reference crate holds the tiered tests: invariants, symmetry, convergence order, \
         exact solutions and one reference per boundary-condition type.",
        "ADR-0007",
    ),
    planned(
        "thresholds-file",
        "codeowners",
        "`reference/thresholds.toml` is the one human-owned file that holds every test tolerance.",
        "ADR-0007",
    ),
    planned(
        "tier-tag",
        "tidy",
        "Every test under `tests/` carries its reference tier.",
        "ADR-0007",
    ),
    planned(
        "usize-alias",
        "dylint",
        "A type-aware `usize` rule, so that an alias such as `type Idx = usize;` or a local \
         variable cannot get around the tidy rule.",
        "ADR-0002",
    ),
];

/// The tidy rules that compare the tree's documents with this binary's own registry, or with
/// what this binary generates. A tree from another revision, whose registry or generator
/// differs, fails them by construction, so they are skipped when tidy judges such a tree
/// (`cargo xtask tidy --root <dir> --policy <dir>`).
pub const REGISTRY_BOUND_RULES: [&str; 4] =
    ["adr-index-doc", "doc-tag", "planned-task", "rules-doc"];

/// True if `relative` is one of the `allowed` files or lies inside one of the directories.
#[must_use]
pub fn is_allowed(allowed: &[&str], relative: &Path) -> bool {
    allowed.iter().any(|prefix| relative.starts_with(prefix))
}

/// True if `id` is a planned mechanism.
#[must_use]
pub fn is_planned(id: &str) -> bool {
    PLANNED.iter().any(|planned| planned.id == id)
}

/// True if some table in this registry defines the tidy rule `id`.
#[must_use]
pub fn is_registered(id: &str) -> bool {
    rule_ids().any(|registered| registered == id)
}

/// The id of every tidy rule, from every table.
pub fn rule_ids() -> impl Iterator<Item = &'static str> {
    RULES.iter().map(|rule| rule.id).chain(ban_ids())
}

/// The ids of the rules of one kind. Bans are source rules.
pub fn rule_ids_of(kind: RuleKind) -> impl Iterator<Item = &'static str> {
    let coded = RULES.iter().filter(move |rule| rule.kind == kind).map(|rule| rule.id);
    coded.chain(ban_ids().filter(move |_| kind == RuleKind::Source))
}

fn ban_ids() -> impl Iterator<Item = &'static str> {
    PATH_BANS.iter().map(|ban| ban.id).chain(MACRO_BANS.iter().map(|ban| ban.id))
}

#[cfg(test)]
mod tests {
    use super::{BanScope, MACRO_BANS, PATH_BANS, PLANNED, REGISTRY_BOUND_RULES, RULES, RuleKind};
    use super::{is_allowed, is_planned, is_registered, rule_ids, rule_ids_of};
    use crate::tidy::Scope;
    use std::path::Path;

    fn is_sorted_and_unique(ids: &[&str]) -> bool {
        ids.is_sorted_by(|a, b| a < b)
    }

    fn is_dashed(id: &str) -> bool {
        !id.is_empty()
            && id.chars().all(|character| character.is_ascii_lowercase() || character == '-')
    }

    // Goal: ids are unique across the tables and sorted within each, so that a rule has one row
    // and each table reads like an index.
    #[test]
    fn rule_ids_are_sorted_and_unique() {
        let coded: Vec<&str> = RULES.iter().map(|rule| rule.id).collect();
        assert!(is_sorted_and_unique(&coded), "RULES must be sorted by id, without duplicates");
        let paths: Vec<&str> = PATH_BANS.iter().map(|ban| ban.id).collect();
        assert!(is_sorted_and_unique(&paths), "PATH_BANS must be sorted by id");
        let macros: Vec<&str> = MACRO_BANS.iter().map(|ban| ban.id).collect();
        assert!(is_sorted_and_unique(&macros), "MACRO_BANS must be sorted by id");
        let mut all: Vec<&str> = rule_ids().collect();
        assert_eq!(all.len(), coded.len() + paths.len() + macros.len());
        all.sort_unstable();
        assert!(is_sorted_and_unique(&all), "a rule id appears in two tables");
        assert!(!is_sorted_and_unique(&["b", "a"]), "unsorted ids are detected");
        assert!(!is_sorted_and_unique(&["a", "a"]), "duplicate ids are detected");
    }

    // Goal: every row is complete: a lowercase dashed id, a summary sentence, a detail that is
    // empty or sentences, and a reference.
    #[test]
    fn rule_rows_are_complete() {
        for rule in RULES {
            assert!(is_dashed(rule.id), "id `{}` must be lowercase-dashed", rule.id);
            assert!(rule.summary.ends_with('.'), "summary of `{}` must be a sentence", rule.id);
            let detail_is_prose = rule.detail.is_empty() || rule.detail.ends_with('.');
            assert!(detail_is_prose, "detail of `{}` must be empty or sentences", rule.id);
            let cites = ["ADR-", "TIGER_STYLE.md, ", "docs/"]
                .iter()
                .any(|prefix| rule.reference.starts_with(prefix));
            assert!(cites, "reference of `{}` must be an ADR, a section or a document", rule.id);
        }
        assert!(is_dashed("line-length") && !is_dashed("Line_Length") && !is_dashed(""));
    }

    // Goal: every ban row is complete: its message says what to do and cites its reason, and it
    // names something to ban and somewhere it is allowed.
    #[test]
    fn ban_rows_are_complete() {
        for ban in PATH_BANS {
            assert!(is_dashed(ban.id), "id `{}` must be lowercase-dashed", ban.id);
            assert!(!ban.segment.is_empty(), "`{}` must ban a segment", ban.id);
            assert!(ban.message.contains("; "), "message of `{}` must name the fix", ban.id);
            assert!(ban.message.ends_with(')'), "message of `{}` must cite its reason", ban.id);
        }
        for ban in MACRO_BANS {
            assert!(is_dashed(ban.id), "id `{}` must be lowercase-dashed", ban.id);
            assert!(!ban.names.is_empty(), "`{}` must ban a macro", ban.id);
            assert!(ban.message.contains("; "), "message of `{}` must name the fix", ban.id);
            assert!(ban.message.ends_with(')'), "message of `{}` must cite its reason", ban.id);
        }
    }

    // Goal: planned mechanisms are sorted, complete, and distinct from the rules that exist: a
    // mechanism is either planned or built, never both.
    #[test]
    fn planned_rows_are_complete() {
        let ids: Vec<&str> = PLANNED.iter().map(|planned| planned.id).collect();
        assert!(is_sorted_and_unique(&ids), "PLANNED must be sorted by id, without duplicates");
        for planned in PLANNED {
            assert!(is_dashed(planned.id), "id `{}` must be lowercase-dashed", planned.id);
            assert!(!is_registered(planned.id), "`{}` is both planned and built", planned.id);
            assert!(is_planned(planned.id), "`{}` is found by its id", planned.id);
            assert!(is_dashed(planned.mechanism), "mechanism of `{}` is one word", planned.id);
            assert!(planned.description.ends_with('.'), "`{}` needs a sentence", planned.id);
            let cites = ["ADR-", "TIGER_STYLE.md, "]
                .iter()
                .any(|prefix| planned.reference.starts_with(prefix));
            assert!(cites, "reference of `{}` must be an ADR or a section", planned.id);
        }
        assert!(!is_planned("line-length"), "a built rule is not planned");
        assert!(!is_planned(""), "the empty id is not planned");
    }

    // Goal: the rules skipped by a foreign gate exist, so the list cannot go stale silently.
    #[test]
    fn registry_bound_rules_are_registered() {
        for id in REGISTRY_BOUND_RULES {
            assert!(is_registered(id), "`{id}` must be a registered rule");
        }
        assert!(REGISTRY_BOUND_RULES.is_sorted(), "keep the list sorted");
    }

    // Goal: lookups see exactly the registered ids, and bans count as source rules.
    #[test]
    fn lookups_follow_the_tables() {
        assert!(is_registered("line-length"), "a coded rule is found");
        assert!(is_registered("direct-libm"), "a path ban is found");
        assert!(is_registered("debug-assert-placement"), "a macro ban is found");
        assert!(!is_registered("no-such-rule"), "an unknown id is rejected");
        assert!(!is_registered(""), "the empty id is rejected");
        let source: Vec<&str> = rule_ids_of(RuleKind::Source).collect();
        let workspace: Vec<&str> = rule_ids_of(RuleKind::Workspace).collect();
        assert_eq!(source.len() + workspace.len(), rule_ids().count());
        assert!(workspace.contains(&"expect-budget") && !source.contains(&"expect-budget"));
        assert!(source.contains(&"direct-libm") && !workspace.contains(&"direct-libm"));
        assert!(source.contains(&"debug-assert-placement"), "macro bans are source rules");
    }

    // Goal: a ban's scope and its allowed list decide where it applies: files and whole
    // directories match by path component, not by text prefix.
    #[test]
    fn ban_scopes_and_allowed_paths() {
        assert!(BanScope::Everywhere.includes(Scope::Tooling), "everywhere includes tooling");
        assert!(BanScope::Everywhere.includes(Scope::Kernels), "everywhere includes crates");
        assert!(BanScope::NamiCrates.includes(Scope::Crate), "nami crates are included");
        assert!(!BanScope::NamiCrates.includes(Scope::Tooling), "tooling is not a nami crate");
        let allowed = ["crates/kernels", "crates/base/src/index.rs"];
        assert!(is_allowed(&allowed, Path::new("crates/kernels/src/math.rs")), "inside");
        assert!(is_allowed(&allowed, Path::new("crates/base/src/index.rs")), "the file itself");
        assert!(!is_allowed(&allowed, Path::new("crates/kernels-extra/src/lib.rs")), "a sibling");
        assert!(!is_allowed(&allowed, Path::new("crates/base/src/lib.rs")), "another file");
        assert!(!is_allowed(&[], Path::new("crates/kernels/src/math.rs")), "nothing is allowed");
    }
}
