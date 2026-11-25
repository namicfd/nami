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
    /// Where the reason is written down: an ADR, a `TIGER_STYLE.md` section or a document.
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
    source(
        "doc-tag",
        "Every mechanism tag in `docs/TIGER_STYLE.md` has a known form and names a tidy rule, a \
         lint that is switched on, or a planned mechanism that exists.",
        "TIGER_STYLE.md, How rules are enforced",
    ),
    workspace(
        "expect-budget",
        "The number of `expect(...)` lint suppressions, counted on tokens so that `cfg_attr` and \
         macro bodies are included, equals `xtask/expect-budget.txt`.",
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
        "learning-format",
        "Every file in `docs/learnings/` is named `YYYY-MM-DD-<slug>.md` and has a status line \
         and the sections `## What happened` and `## Proposed guardrail`.",
        "docs/learnings/README.md",
    ),
    source(
        "line-length",
        "Lines in Rust sources are at most 100 columns.",
        "TIGER_STYLE.md, Style by the numbers",
    ),
    source(
        "magic-tolerance",
        "In nami crates outside tests, a float literal in (0, 1e-6] appears only in the \
         initialiser of a `const` item that has a doc comment.",
        "TIGER_STYLE.md, Floating point",
    ),
    source("parse", "Every Rust source file parses.", "ADR-0006"),
    workspace(
        "planned-task",
        "Every planned mechanism has a task with the same id in a `tasks.json` under \
         `docs/exec-plans/active/`.",
        "ADR-0011",
    ),
    workspace(
        "rules-doc",
        "`docs/RULES.md` matches what `cargo xtask rules` generates from the registry.",
        "ADR-0006",
    ),
    source(
        "runtime-dependency",
        "Runtime and build dependencies of nami crates are `libm` and workspace crates only, \
         and none is renamed with `package = \"...\"`.",
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
        "No `loop`, no `while`, and no `for` over a range without an end unless `take` or `zip` \
         bounds it: every loop has a fixed upper bound.",
        "TIGER_STYLE.md, Put a limit on everything",
    ),
    source("unsafe", "`unsafe` appears only in `crates/alloc-guard`.", "ADR-0014"),
    source(
        "usize",
        "In nami crates, `usize` appears in function signatures, fields, public constants and \
         statics, and type aliases only in `base::index`. Trait impls and const generics are \
         exempt.",
        "ADR-0002",
    ),
    source(
        "vec-field",
        "No `Vec` anywhere inside a field type in nami crates: fixed-size storage is `Box<[T]>`.",
        "ADR-0003",
    ),
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
        "A counting-allocator test around the time loop proves that nothing allocates after init.",
        "ADR-0003",
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
        "float-eq-in-eq-fn",
        "tidy",
        "Flag `==` and `!=` between floats in functions whose name contains `eq`, which clippy's \
         `float_cmp` skips.",
        "ADR-0006",
    ),
    planned(
        "float-reduction",
        "dylint",
        "Flag naive float reductions (`.sum::<f64>()`, a `fold` with `+`) and name `sum_fixed`.",
        "ADR-0005",
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
        "Detect call cycles that cross crate boundaries; `nami_recursion` checks each crate on \
         its own.",
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
        "tasks-schema",
        "ci",
        "CI validates every `tasks.json` against the schema and rejects a PR in which an agent \
         sets `passes`.",
        "ADR-0011",
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

/// The tidy rules that compare the tree's documents with this binary's own registry. A tree
/// from another revision, whose registry differs, fails them by construction, so they are
/// skipped when tidy judges such a tree (`cargo xtask tidy --root <dir> --policy <dir>`).
pub const REGISTRY_BOUND_RULES: [&str; 3] = ["doc-tag", "planned-task", "rules-doc"];

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

    // Goal: every row is complete: a lowercase dashed id, a summary sentence and a reference.
    #[test]
    fn rule_rows_are_complete() {
        for rule in RULES {
            assert!(is_dashed(rule.id), "id `{}` must be lowercase-dashed", rule.id);
            assert!(rule.summary.ends_with('.'), "summary of `{}` must be a sentence", rule.id);
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
