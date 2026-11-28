//! The two canonical additive diffs that ADR-0022 exists for: a new line rule dispatched from
//! `text::check`, and a new lint with its registration and a UI case (review of #57, rounds 8
//! and 9).
//!
//! Each diff is written against the frozen copies of the files it edits
//! ([`super::frozen`]), so that its anchors never drift. The unit tests here run both diffs
//! through the classifier and the mutant coverage on those copies, so that a change to the
//! ratchet that would stop them from passing fails in CI. The end-to-end tests in
//! `xtask/tests/ratchet.rs` apply the same diffs to a copy of the live tree and run the whole
//! ratchet with the real tools. They first check that each live file still equals its frozen
//! copy, and tell how to update the copy when it does not.

use std::collections::BTreeMap;

/// One replacement in a file: `from`, which must occur in it exactly once, becomes `to`.
#[derive(Clone, Copy, Debug)]
pub struct Edit {
    /// The file, relative to the root.
    pub path: &'static str,
    /// The anchor.
    pub from: &'static str,
    /// What replaces it; it keeps the anchor where the diff only inserts.
    pub to: &'static str,
}

/// A canonical diff: edits to existing files, applied in order, and new files.
#[derive(Clone, Copy, Debug)]
pub struct Diff {
    /// Its name, for scratch directories and messages.
    pub name: &'static str,
    /// The edits to files of the base.
    pub edits: &'static [Edit],
    /// The new files: path relative to the root, and text.
    pub files: &'static [(&'static str, &'static str)],
}

/// The tail of `text::check`, after which the new rule's dispatch call goes.
const TEXT_TAIL: &str = "    }\n    violations\n}\n\n/// True if the first line above";

/// A new line rule, `trailing-space`: a function dispatched at the tail of `text::check` with
/// a shared parameter passed as it is, its registry row, and a fixture on which it fires and
/// one on which it does not.
pub const TEXT_RULE: Diff = Diff {
    name: "text-rule",
    edits: &[
        Edit {
            path: "xtask/src/tidy/text.rs",
            from: TEXT_TAIL,
            to: "    }\n    violations.extend(trailing_space(path, &lines));\n    violations\n}\n\n\
                 /// True if the first line above",
        },
        Edit {
            path: "xtask/src/tidy/text.rs",
            from: "fn to_line_number(",
            to: "/// Reports every line that ends in a space or a tab.
fn trailing_space(path: &Path, lines: &[&str]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        if line.ends_with([' ', '\\t']) {
            let message = \"line ends in white space; remove it (TIGER_STYLE.md)\";
            let line_number = to_line_number(line_index);
            violations.push(Violation::new(path, line_number, \"trailing-space\", message));
        }
    }
    assert!(violations.len() <= lines.len(), \"a line is reported at most once\");
    violations
}

fn to_line_number(",
        },
        Edit {
            path: "xtask/src/tidy/registry.rs",
            from: "        reference: \"TIGER_STYLE.md, Comments, docs and commits\",\n    },\n",
            to: "        reference: \"TIGER_STYLE.md, Comments, docs and commits\",
    },
    Rule {
        id: \"trailing-space\",
        kind: RuleKind::Source,
        summary: \"No line ends in white space.\",
        detail: \"\",
        reference: \"TIGER_STYLE.md, Style by the numbers\",
    },
",
        },
    ],
    files: &[
        (
            "xtask/fixtures/tidy/trailing-space.fires.fixture",
            "// path: crates/solver/src/example.rs\n//! A space at the end. \n",
        ),
        (
            "xtask/fixtures/tidy/trailing-space.passes.fixture",
            "// path: crates/solver/src/example.rs\n//! No space at the end.\n",
        ),
    ],
};

/// The lint library, which the new lint's edits change.
const LIB: &str = "lints/src/lib.rs";

/// The registration of the base's last lint pass, after which the new one goes.
const LAST_PASS: &str =
    "    lint_store.register_late_lint_pass(Box::new(|_| Box::new(NamiFloatReduction)));\n";

/// A new lint, `nami_empty_match`, at `Deny`: its module, its declaration and pass, the lint
/// appended to the registration, the pass registered and implemented, and a UI case with the
/// output it expects.
pub const LINT: Diff = Diff {
    name: "lint",
    edits: &[
        Edit {
            path: LIB,
            from: "mod float_reduction;\n",
            to: "mod empty_match;\nmod float_reduction;\n",
        },
        Edit {
            path: LIB,
            from: "dylint_linting::declare_lint_pass!(NamiFloatReduction => \
                   [NAMI_FLOAT_REDUCTION]);\n",
            to: "dylint_linting::declare_lint_pass!(NamiFloatReduction => [NAMI_FLOAT_REDUCTION]);

dylint_linting::declare_lint! {
    /// Reports a `match` without arms.
    pub NAMI_EMPTY_MATCH,
    Deny,
    \"match without arms\"
}

dylint_linting::declare_lint_pass!(NamiEmptyMatch => [NAMI_EMPTY_MATCH]);
",
        },
        Edit {
            path: LIB,
            from: "NAMI_RECURSION, NAMI_FLOAT_REDUCTION]);\n",
            to: "NAMI_RECURSION, NAMI_FLOAT_REDUCTION, NAMI_EMPTY_MATCH]);\n",
        },
        Edit {
            path: LIB,
            from: LAST_PASS,
            to:
                "    lint_store.register_late_lint_pass(Box::new(|_| Box::new(NamiFloatReduction)));
    lint_store.register_late_lint_pass(Box::new(|_| Box::new(NamiEmptyMatch)));
",
        },
        Edit {
            path: LIB,
            from: "\n// Goal: each lint fires exactly",
            to: "
impl<'tcx> LateLintPass<'tcx> for NamiEmptyMatch {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expression: &'tcx Expr<'tcx>) {
        empty_match::check_expr(cx, NAMI_EMPTY_MATCH, expression);
    }
}

// Goal: each lint fires exactly",
        },
    ],
    files: &[
        (
            "lints/src/empty_match.rs",
            "//! `nami_empty_match`: no `match` without arms.

use clippy_utils::diagnostics::span_lint;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, Lint};

/// Reports `expression` if it is a `match` without arms.
pub fn check_expr(cx: &LateContext<'_>, lint: &'static Lint, expression: &Expr<'_>) {
    if let ExprKind::Match(_, arms, _) = expression.kind
        && arms.is_empty()
    {
        span_lint(cx, lint, expression.span, \"`match` without arms; say it with the type\");
    }
}
",
        ),
        (
            "lints/ui/empty_match.rs",
            "//! `nami_empty_match` fires on a `match` without arms only.

enum Never {}

fn absurd(never: Never) -> u32 {
    match never {}
}

fn plain(value: Option<u32>) -> u32 {
    match value {
        Some(number) => number,
        None => 0,
    }
}

fn main() {
    let _unused = (absurd as fn(Never) -> u32, plain(Some(1)));
}
",
        ),
        (
            "lints/ui/empty_match.stderr",
            "error: `match` without arms; say it with the type
  --> $DIR/empty_match.rs:6:5
   |
LL |     match never {}
   |     ^^^^^^^^^^^^^^
   |
   = note: `#[deny(nami_empty_match)]` on by default

error: aborting due to 1 previous error

",
        ),
    ],
};

/// Both canonical diffs.
pub const DIFFS: [Diff; 2] = [TEXT_RULE, LINT];

impl Diff {
    /// Every file the diff changes, relative to the root, with its base text, `None` for a new
    /// file, and its head text: the edits applied to the frozen copies, in order.
    ///
    /// # Errors
    ///
    /// Names the anchor that does not occur exactly once in its frozen copy.
    pub fn changes(&self) -> Result<Vec<(String, Option<String>, String)>, String> {
        let mut heads: BTreeMap<&str, (String, String)> = BTreeMap::new();
        for edit in self.edits {
            let (_, head) = heads.entry(edit.path).or_insert_with(|| {
                let frozen = super::frozen(edit.path);
                (frozen.clone(), frozen)
            });
            let found = head.matches(edit.from).count();
            if found != 1 {
                return Err(format!(
                    "the canonical diff `{}` anchors on `{}` in {}, which its frozen copy holds \
                     {found} times; fix the anchor",
                    self.name, edit.from, edit.path
                ));
            }
            *head = head.replacen(edit.from, edit.to, 1);
        }
        let edited = heads.len();
        let mut changed: Vec<(String, Option<String>, String)> = heads
            .into_iter()
            .map(|(path, (base, head))| (path.to_owned(), Some(base), head))
            .collect();
        for (path, text) in self.files {
            changed.push(((*path).to_owned(), None, (*text).to_owned()));
        }
        assert_eq!(changed.len(), edited + self.files.len(), "each new file once");
        assert!(changed.iter().all(|(_, base, head)| base.as_ref() != Some(head)), "all change");
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::{DIFFS, Diff};
    use std::collections::BTreeMap;

    /// The first line of every new function's body in `text`, where cargo-mutants puts the
    /// mutant that replaces the function: the end-to-end runs found one in each.
    fn function_bodies(text: &str, names: &[&str]) -> Vec<u32> {
        let lines: Vec<&str> = text.lines().collect();
        let mut starts = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if names.iter().any(|name| line.contains(&format!("fn {name}("))) {
                starts.push(u32::try_from(index + 2).expect("files have fewer than 2^32 lines"));
            }
        }
        starts
    }

    // Goal: round 9 of #57's review: both canonical diffs stay additive and fully covered as
    // the ratchet changes. On the frozen copies, the classifier calls each additive with no
    // edit, and with a mutant in each new function's body, no hunk of a changed source is left
    // without one: the dispatch calls, the declarations and the appended registration are
    // exempt. A diff that broke either fails here in CI, not only in the ignored end-to-end
    // runs.
    #[test]
    fn the_canonical_diffs_are_additive_and_covered() {
        for diff in DIFFS {
            assert_additive_and_covered(diff);
        }
    }

    /// Asserts that `diff`, on the frozen copies, is additive and leaves no hunk of a changed
    /// source without a mutant, with one in each new function's body.
    fn assert_additive_and_covered(diff: Diff) {
        let scratch = crate::tidy::scratch::Scratch::new(&format!("canonical-{}", diff.name));
        let changes = diff.changes().unwrap();
        for (path, base, head) in &changes {
            if let Some(text) = base {
                scratch.write(&format!("base/{path}"), text);
            }
            scratch.write(&format!("head/{path}"), head);
        }
        let (base, head) = (scratch.root().join("base"), scratch.root().join("head"));
        let paths: Vec<String> = changes.iter().map(|(path, _, _)| path.clone()).collect();
        let class = super::super::class::classify(&base, &head, &paths).unwrap();
        assert_eq!(class.edits, Vec::<String>::new(), "{} is additive", diff.name);
        assert!(!class.dispatch.is_empty(), "{} dispatches its rule", diff.name);
        let code = ["xtask/src/tidy/text.rs", "lints/src/lib.rs", "lints/src/empty_match.rs"];
        let sources = changes.iter().filter(|(path, _, _)| code.contains(&path.as_str()));
        let starts: BTreeMap<String, Vec<u32>> = sources
            .clone()
            .map(|(path, _, text)| {
                (path.clone(), function_bodies(text, &["trailing_space", "check_expr"]))
            })
            .collect();
        let work = scratch.root().join("work");
        for (path, _, _) in sources {
            let uncovered =
                super::super::mutants::uncovered_hunks(&base, &head, path, &starts, &work, &class)
                    .unwrap();
            assert_eq!(uncovered, Vec::<String>::new(), "{}: {path}", diff.name);
        }
    }

    // Goal: an anchor that its frozen copy does not hold exactly once is named, with what to
    // fix, rather than applied somewhere else.
    #[test]
    fn a_drifted_anchor_is_named() {
        let diff = super::Diff {
            name: "drifted",
            edits: &[super::Edit { path: "xtask/src/tidy/text.rs", from: "no such text", to: "" }],
            files: &[],
        };
        let error = diff.changes().unwrap_err();
        assert!(error.contains("anchors on `no such text`"), "{error}");
        assert!(error.contains("0 times; fix the anchor"), "{error}");
    }
}
