//! The ratchet (ADR-0022) against fake heads: trees that loosen a rule in one way each, run
//! through stand-ins for the head's xtask and for cargo.
//!
//! The base is a small nami tree with real tidy fixtures, the real rule registry, a CODEOWNERS
//! and one fake lint. Its tidy is the code under test, as in CI. A head's xtask is the real one
//! behind a filter that drops some of what it reports, which is what a loosened rule looks like
//! from outside. cargo is a script whose "lint library" is a list of `line:column` places in
//! `lints/src/fake-lint.txt`, and whose mutation testing reads `fake-mutants.txt`.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::ratchet::review::{AUTHOR_APP, Pull, REVIEWER_APP, Review, Reviews, Thread};
use xtask::ratchet::{Check, ReviewSource, Tools, Verdict, check, judge, observe};

/// A head's xtask: runs the real one and drops every output line that matches `@DROP@`, for the
/// subcommand option `@ONLY@` (`--file`, `--root`), or for all when it is `any`. A tidy run left
/// without a violation exits 0, as the real one would. `tidy --file` on a file that contains
/// `@ADD@` reports `unbounded-loop` on its first line as well.
const FAKE_XTASK: &str = r#"#!/bin/sh
work=$(mktemp -d)
"@REAL@" "$@" > "$work/out" 2> "$work/err"
code=$?
if [ "@ONLY@" = any ] || [ "$2" = "@ONLY@" ]; then
    grep -v -e '@DROP@' "$work/out" > "$work/out.kept"
    grep -v -e '@DROP@' "$work/err" > "$work/err.kept"
else
    cp "$work/out" "$work/out.kept"
    cp "$work/err" "$work/err.kept"
fi
cat "$work/out.kept"
cat "$work/err.kept" >&2
if [ "$code" -ne 0 ] && [ "$1" = tidy ] && ! grep -q 'error\[' "$work/err.kept"; then
    code=0
fi
if [ "$2" = --file ] && grep -q -e '@ADD@' "$3"; then
    echo "${3#"$PWD"/}:1: error[unbounded-loop]: fires on correct code" >&2
    code=2
fi
rm -rf "$work"
exit "$code"
"#;

/// A stand-in for cargo.
/// - `cargo build` leaves a head xtask that is the real one, or fails while `broken-build`
///   exists.
/// - `cargo mutants --list` lists the lines of `<dir>/src/fake-mutants.txt` (one mutant by
///   default); a run writes `outcomes.json` and `missed.txt` from `<dir>/src/fake-missed.txt`, and
///   leaves `missed.txt` out while `<dir>/src/no-missed-file` exists.
/// - `cargo dylint --path <tree>/lints ...` "lints" every `.rs` file of the package it runs in,
///   reporting each `line:column` of `<tree>/lints/src/fake-lint.txt` as JSON.
/// - `cargo dylint --all -- --workspace ...`, which the head's `xtask dylint` runs, reports
///   them in human form on the examples of the crates of the workspace it runs in.
const FAKE_CARGO: &str = r#"#!/bin/sh
if [ "$1" = build ]; then
    if [ -e broken-build ]; then
        echo "error: could not compile xtask" >&2
        exit 101
    fi
    mkdir -p target/debug
    printf '#!/bin/sh\nexec "@REAL@" "$@"\n' > target/debug/xtask
    chmod +x target/debug/xtask
    exit 0
fi
if [ "$1" = mutants ]; then
    list=no
    while [ $# -gt 0 ]; do
        case "$1" in
            --output) output="$2"; shift ;;
            --dir) dir="$2"; shift ;;
            --list) list=yes ;;
        esac
        shift
    done
    listing=$(mktemp)
    echo "src/a.rs:1:1: fake" > "$listing"
    [ -e "$dir/src/fake-mutants.txt" ] && cp "$dir/src/fake-mutants.txt" "$listing"
    if [ "$list" = yes ]; then cat "$listing"; rm "$listing"; exit 0; fi
    total=$(grep -c . "$listing")
    rm "$listing"
    missed=0
    [ -s "$dir/src/fake-missed.txt" ] && missed=$(grep -c . "$dir/src/fake-missed.txt")
    mkdir -p "$output/mutants.out"
    counts='{"total_mutants":%s,"missed":%s,"caught":%s,"timeout":0,"unviable":0}'
    out="$output/mutants.out"
    printf "$counts" "$total" "$missed" $((total - missed)) > "$out/outcomes.json"
    if [ ! -e "$dir/src/no-missed-file" ]; then
        : > "$out/missed.txt"
        [ -e "$dir/src/fake-missed.txt" ] && cp "$dir/src/fake-missed.txt" "$out/missed.txt"
    fi
    [ "$missed" -gt 0 ] && exit 2
    exit 0
fi
if [ "$1" != dylint ]; then
    echo "fake cargo: unexpected $*" >&2
    exit 101
fi
if [ "$2" = --path ]; then
    message='"reason":"compiler-message","message":{"code":{"code":"nami_fake"},"level":"error"'
    for file in *.rs; do
        while IFS=: read -r line column; do
            span="\"is_primary\":true,\"file_name\":\"$file\""
            span="$span,\"line_start\":$line,\"column_start\":$column"
            echo "{$message,\"spans\":[{$span}]}}"
        done < "$3/src/fake-lint.txt"
    done
    exit 101
fi
for file in crates/*/examples/*.rs; do
    [ -e "$file" ] || continue
    while IFS=: read -r line column; do
        printf 'error: a fake lint\n  --> %s:%s:%s\n\n' "$file" "$line" "$column" >&2
    done < lints/src/fake-lint.txt
done
exit 101
"#;

/// A temporary directory, removed when the test ends.
struct Temporary {
    root: PathBuf,
}

impl Temporary {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nami-ratchet-{name}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).expect("a stale directory can be removed");
        }
        fs::create_dir_all(&root).expect("the temporary directory is writable");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.root);
    }
}

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("files have a parent")).expect("writable");
    fs::write(path, text).expect("writable");
}

fn executable(path: &Path, text: &str) -> PathBuf {
    fs::write(path, text).expect("writable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path.to_path_buf()
}

fn real(relative: &str) -> String {
    fs::read_to_string(xtask::workspace_root().join(relative)).expect("the real file exists")
}

/// A CODEOWNERS that owns a core file and `.github/`, and leaves the rule paths unowned, as the
/// policy of ADR-0022 would.
fn codeowners() -> String {
    "/xtask/expect-budget.txt @owner\n/.github/ @owner\n".to_owned()
}

/// A small nami tree that the ratchet can judge: real fixtures for two rules, the real registry,
/// a CODEOWNERS, a fake lint with one UI case, and what `tidy --root` and `xtask dylint` need.
fn nami_tree(root: &Path) {
    let files = [
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n".to_owned()),
        ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.100.0\"\n".to_owned()),
        (".github/CODEOWNERS", codeowners()),
        ("xtask/Cargo.toml", "[package]\nname = \"xtask\"\n".to_owned()),
        ("xtask/expect-budget.txt", "0\n".to_owned()),
        ("xtask/src/tidy/registry.rs", real("xtask/src/tidy/registry.rs")),
        ("docs/design/README.md", "No decisions yet.\n".to_owned()),
        ("docs/TIGER_STYLE.md", "## Rules\n\n- A rule. `[review]`\n".to_owned()),
        ("crates/solver/Cargo.toml", "[package]\nname = \"solver\"\n".to_owned()),
        ("crates/solver/src/lib.rs", "//! A crate.\n".to_owned()),
        ("crates/solver/src/step.rs", "//! A second file.\n".to_owned()),
        ("lints/Cargo.toml", "[package]\nname = \"nami_lints\"\n".to_owned()),
        ("lints/Cargo.lock", "version = 4\n".to_owned()),
        ("lints/rust-toolchain.toml", "[toolchain]\n".to_owned()),
        (
            "lints/src/lib.rs",
            "declare_lint! { pub NAMI_FAKE, Deny, \"a fake lint\" }\n\
             fn register(store: &mut Store) { store.register_lints(&[NAMI_FAKE]); }\n"
                .to_owned(),
        ),
        ("lints/src/fake-lint.txt", "2:5\n4:1\n".to_owned()),
        ("lints/ui/main.rs", "fn main() {\n    loop {}\n}\n// The end.\n".to_owned()),
        (
            "lints/ui/main.stderr",
            "error: a fake lint\n  --> $DIR/main.rs:2:5\n\nerror: a fake lint\n  --> \
             $DIR/main.rs:4:1\n\nerror: aborting due to 2 previous errors\n"
                .to_owned(),
        ),
    ];
    for (relative, text) in files {
        write(root, relative, &text);
    }
    for name in ["unbounded-loop.fires", "unbounded-loop.passes", "line-length.fires"] {
        let fixture = format!("xtask/fixtures/tidy/{name}.fixture");
        write(root, &fixture, &real(&fixture));
    }
}

/// GitHub as the review check sees it: the head commit `head`, opened by the agents' app.
struct FakeReviews {
    reviewer: &'static str,
    commit: &'static str,
    resolved_by: Option<&'static str>,
    /// An approval: who, and on which commit.
    approval: Option<(&'static str, &'static str)>,
}

/// The reviewer app reviewed the head and resolved its one thread.
const REVIEWED: FakeReviews = FakeReviews {
    reviewer: REVIEWER_APP,
    commit: "head",
    resolved_by: Some(REVIEWER_APP),
    approval: None,
};

impl Reviews for FakeReviews {
    fn pull(&self, _pull: u32) -> Result<Pull, String> {
        Ok(Pull { head: "head".to_owned(), author: AUTHOR_APP.to_owned() })
    }
    fn reviews(&self, _pull: u32) -> Result<Vec<Review>, String> {
        let state = "COMMENTED".to_owned();
        let mut reviews = vec![Review {
            author: self.reviewer.to_owned(),
            commit: self.commit.to_owned(),
            state,
        }];
        if let Some((author, commit)) = self.approval {
            let approved = "APPROVED".to_owned();
            reviews.push(Review {
                author: author.to_owned(),
                commit: commit.to_owned(),
                state: approved,
            });
        }
        Ok(reviews)
    }
    fn threads(&self, _pull: u32) -> Result<Vec<Thread>, String> {
        let resolved_by = self.resolved_by.map(str::to_owned);
        let path = "lints/src/lib.rs".to_owned();
        Ok(vec![Thread { path, author: REVIEWER_APP.to_owned(), resolved_by }])
    }
}

/// A base, a head that starts as a copy of it, and the fake tools.
struct Case {
    place: Temporary,
}

impl Case {
    fn new(name: &str) -> Self {
        let place = Temporary::new(name);
        nami_tree(&place.path("base"));
        nami_tree(&place.path("head"));
        // Every head changes a rule path, so that the checks run.
        write(&place.path("head"), "docs/RULES.md", "A regenerated rule list.\n");
        Self { place }
    }

    fn base(&self) -> PathBuf {
        self.place.path("base")
    }

    fn head(&self) -> PathBuf {
        self.place.path("head")
    }

    /// The tools: a head xtask that drops `drop` from the output of `only` and adds a report
    /// to files containing `add`, the fake cargo, and `reviews` as GitHub, with the reviewer
    /// app set up.
    fn tools<'a>(&self, drop: &str, only: &str, add: &str, reviews: &'a FakeReviews) -> Tools<'a> {
        let script = fake_xtask(drop, only, add);
        Tools {
            head_xtask: Some(executable(&self.place.path("xtask.sh"), &script)),
            cargo: fake_cargo(&self.place.path("cargo.sh")).into_os_string(),
            work: self.place.path("work"),
            mutants: true,
            review: Some(ReviewSource {
                reviews,
                pull: 7,
                reviewer: Some(REVIEWER_APP.to_owned()),
            }),
            seed: Some(11),
        }
    }

    /// Runs the ratchet with a head xtask that drops `drop` from the output of `only`.
    fn run(&self, drop: &str, only: &str) -> Verdict {
        let tools = self.tools(drop, only, NOTHING, &REVIEWED);
        check(&self.base(), &self.head(), &tools).expect("the ratchet reaches a verdict")
    }
}

/// Writes the stand-in for cargo at `path`; see [`FAKE_CARGO`].
fn fake_cargo(path: &Path) -> PathBuf {
    let real = std::env::var("CARGO_BIN_EXE_xtask").expect("cargo sets CARGO_BIN_EXE_xtask");
    executable(path, &FAKE_CARGO.replace("@REAL@", &real))
}

/// The script of a head's xtask; see [`FAKE_XTASK`].
fn fake_xtask(drop: &str, only: &str, add: &str) -> String {
    let real = std::env::var("CARGO_BIN_EXE_xtask").expect("cargo sets CARGO_BIN_EXE_xtask");
    let script = FAKE_XTASK.replace("@REAL@", &real).replace("@DROP@", drop);
    script.replace("@ONLY@", only).replace("@ADD@", add)
}

/// The check and place of every finding, for comparisons.
fn found(verdict: &Verdict) -> Vec<(Check, String)> {
    verdict.findings.iter().map(|finding| (finding.check, finding.place.clone())).collect()
}

/// A pattern that matches nothing: an honest head.
const NOTHING: &str = "^never matches$";

/// `text` with `from` replaced by `to`, which must occur in it.
fn replaced(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "`{from}` is in the text");
    text.replace(from, to)
}

// Goal: a head that only tightens passes: an extra lint place, a new fixture, and a head that
// the reviewer app reviewed with its thread resolved give no finding, and every check ran.
#[test]
fn a_pure_tightening_passes() {
    let case = Case::new("tightening");
    write(&case.head(), "lints/src/fake-lint.txt", "2:5\n4:1\n1:1\n");
    write(&case.head(), "xtask/fixtures/tidy/new-rule.fires.fixture", "// path: a.rs\n");
    let verdict = case.run(NOTHING, "any");
    assert_eq!(found(&verdict), Vec::new(), "{}", verdict.summary());
    let all =
        [Check::RuleSet, Check::TidyCorpus, Check::LintCorpus, Check::EndToEnd, Check::Review];
    assert!(all.iter().all(|check| verdict.checks.contains(check)), "{:?}", verdict.checks);
    assert_eq!(verdict.line(), "ratchet: pass", "{}", verdict.summary());
}

// Goal: a head whose tidy no longer reports a rule on the base's firing fixture fails, in the
// corpus and end to end, at every placement, including those drawn from the head tree.
#[test]
fn a_loosened_tidy_rule_fails() {
    let case = Case::new("loosened");
    let verdict = case.run("error\\[unbounded-loop\\]", "any");
    let checks: Vec<Check> = found(&verdict).into_iter().map(|(check, _)| check).collect();
    assert!(checks.contains(&Check::TidyCorpus) && checks.contains(&Check::EndToEnd), "{checks:?}");
    let fixture = "xtask/fixtures/tidy/unbounded-loop.fires.fixture:";
    assert!(verdict.findings.iter().all(|finding| finding.place.starts_with(fixture)));
    let drawn = verdict.findings.iter().any(|finding| !finding.message.contains("example.rs"));
    assert!(drawn, "a placement drawn from the tree: {}", verdict.summary());
    assert!(verdict.line().starts_with("ratchet: fail"));
}

// Goal: a head whose tidy fires on the base's passing fixture fails: the rule would stall every
// agent on correct code.
#[test]
fn a_rule_that_fires_on_correct_code_fails() {
    let case = Case::new("fires-on-correct");
    let tools = case.tools(NOTHING, "any", "STEPS_MAX", &REVIEWED);
    let verdict = check(&case.base(), &case.head(), &tools).unwrap();
    let passing = "xtask/fixtures/tidy/unbounded-loop.passes.fixture:1";
    assert!(
        verdict.findings.iter().all(|finding| finding.place == passing),
        "{}",
        verdict.summary()
    );
    assert!(verdict.findings[0].message.contains("fire on correct code"));
}

// Goal: a head whose walk skips `crates/` passes the corpus, because `tidy --file` still sees
// the file, and fails end to end, because `tidy --root` never reports it.
#[test]
fn a_walk_that_skips_crates_fails_end_to_end() {
    let case = Case::new("walk");
    let verdict = case.run("^crates/", "--root");
    let checks: Vec<Check> = found(&verdict).into_iter().map(|(check, _)| check).collect();
    assert!(
        !checks.is_empty() && checks.iter().all(|check| *check == Check::EndToEnd),
        "{checks:?}"
    );
    assert!(verdict.findings[0].message.contains("changes which files tidy checks"));
}

// Goal: the review's exploits on data fail the rule set, read from the head's source without
// running it: a removed rule, `direct-libm` narrowed to nami crates, an exception added to
// `debug-assert-placement`.
#[test]
fn loosened_rule_tables_fail_the_rule_set() {
    let case = Case::new("tables");
    let registry = real("xtask/src/tidy/registry.rs");
    let narrowed =
        replaced(&registry, "scope: BanScope::Everywhere", "scope: BanScope::NamiCrates");
    let excepted = replaced(
        &narrowed,
        "allowed: &[\"crates/kernels\"]",
        "allowed: &[\"crates/kernels\", \"xtask\"]",
    );
    let removed = replaced(&excepted, "id: \"line-length\"", "id: \"line-length-v2\"");
    write(&case.head(), "xtask/src/tidy/registry.rs", &removed);
    let verdict = case.run(NOTHING, "any");
    let messages: Vec<&str> = verdict
        .findings
        .iter()
        .filter(|finding| finding.check == Check::RuleSet)
        .map(|finding| finding.message.as_str())
        .collect();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert!(messages.iter().any(|message| message.contains("narrows its scope from Everywhere")));
    assert!(messages.iter().any(|message| message.contains("is allowed in [\"xtask\"] as well")));
    assert!(
        messages.iter().any(|message| message.contains("tidy rule `line-length` is registered"))
    );
}

// Goal: a deleted `check_*` call in a rule module fails the rule set, though nothing runs it.
#[test]
fn a_deleted_check_call_fails_the_rule_set() {
    let case = Case::new("deleted-call");
    let module = "xtask/src/tidy/rust_source.rs";
    let text = real(module);
    write(&case.base(), module, &text);
    write(&case.head(), module, &replaced(&text, "self.check_float_equality(function);", ""));
    let verdict = case.run(NOTHING, "any");
    let call = verdict.findings.iter().find(|finding| finding.place == module).expect("a finding");
    assert!(call.message.contains("`check_body` no longer calls `check_float_equality`"));
}

// Goal: a head whose lint drops a base diagnostic fails in the lint corpus and end to end; a
// lint lowered to a warning, declared a second time, or no longer registered fails the rule set.
#[test]
fn a_dropped_lint_diagnostic_fails() {
    let case = Case::new("lint");
    write(&case.head(), "lints/src/fake-lint.txt", "2:5\n");
    write(
        &case.head(),
        "lints/src/lib.rs",
        "declare_lint! { pub NAMI_FAKE, Warn, \"a fake lint\" }\n",
    );
    let verdict = case.run(NOTHING, "any");
    let expected = vec![
        (Check::RuleSet, "lints/src/lib.rs".to_owned()),
        (Check::RuleSet, "lints/src/lib.rs".to_owned()),
        (Check::LintCorpus, "lints/ui/main.rs:4:1".to_owned()),
        (Check::EndToEnd, "lints/ui/main.rs:4:1".to_owned()),
    ];
    assert_eq!(found(&verdict), expected, "{}", verdict.summary());
    let decoy = "declare_lint! { pub NAMI_FAKE, Deny, \"a\" }\n#[cfg(any())]\n\
                 declare_lint! { pub NAMI_FAKE, Warn, \"b\" }\n";
    write(&case.head(), "lints/src/lib.rs", decoy);
    write(&case.head(), "lints/src/fake-lint.txt", "2:5\n4:1\n");
    let twice = case.run(NOTHING, "any");
    assert!(twice.findings.iter().any(|finding| finding.message.contains("declared 2 times")));
}

// Goal: a change outside the rule paths is reported, not failed: with no tooling change nothing
// runs; with a rule path changed as well, the checks still run.
#[test]
fn changes_outside_the_rule_paths_are_neutral() {
    let case = Case::new("outside");
    fs::remove_file(case.head().join("docs/RULES.md")).unwrap();
    write(&case.head(), "crates/solver/src/lib.rs", "//! Changed.\n");
    let unrelated = case.run(NOTHING, "any");
    assert_eq!((unrelated.checks.len(), unrelated.findings.len()), (0, 0));
    assert_eq!(unrelated.outside, ["crates/solver/src/lib.rs"]);
    assert!(unrelated.line().starts_with("ratchet: pass, outside the rule paths: 1"));
    write(&case.head(), "docs/TIGER_STYLE.md", "## Rules\n\n- Changed. `[review]`\n");
    write(&case.head(), "lints/ui/main.rs", "fn main() {\n    loop {}\n}\n// Changed.\n");
    let mixed = case.run(NOTHING, "any");
    assert_eq!(mixed.rule_paths, ["lints/ui/main.rs"]);
    assert_eq!(mixed.outside, ["crates/solver/src/lib.rs", "docs/TIGER_STYLE.md"]);
    assert!(mixed.checks.contains(&Check::LintCorpus) && mixed.findings.is_empty());
}

// Goal: mutation testing sees the changed Rust sources of the rule paths only, per workspace,
// as a diff whose paths are relative to that workspace; a missed mutant is a finding, and the
// summary counts the mutants.
#[test]
fn mutants_see_the_rule_path_diff_per_workspace() {
    let case = Case::new("diff");
    write(&case.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    write(&case.head(), "lints/src/notes.md", "Not Rust.\n");
    let declaration = fs::read_to_string(case.head().join("lints/src/lib.rs")).unwrap();
    write(&case.head(), "lints/src/lib.rs", &format!("{declaration}// A comment.\n"));
    write(&case.head(), "xtask/src/tidy/text.rs", "fn rule() {}\n");
    write(
        &case.head(),
        "lints/src/fake-missed.txt",
        "src/extra.rs:1:22: replace extra -> bool with false\n",
    );
    let verdict = case.run(NOTHING, "any");
    assert_eq!(found(&verdict), vec![(Check::Mutants, "lints/src/extra.rs:1:22".to_owned())]);
    assert!(
        verdict.notes.iter().any(|note| note == "Mutants in lints: 1, missed: 1."),
        "{:?}",
        verdict.notes
    );
    let lints = fs::read_to_string(case.place.path("work/mutants-lints.diff")).unwrap();
    assert!(lints.contains("--- /dev/null\n+++ b/src/extra.rs\n"), "{lints}");
    assert!(lints.contains("--- a/src/lib.rs\n+++ b/src/lib.rs\n"), "{lints}");
    assert!(lints.contains("+// A comment.") && !lints.contains("notes.md"), "{lints}");
    let xtask = fs::read_to_string(case.place.path("work/mutants-xtask.diff")).unwrap();
    assert!(xtask.starts_with("diff --git a/xtask/src/tidy/text.rs"), "{xtask}");
}

// Goal: mutation results are read strictly: a missing `missed.txt` is no verdict, and a change
// that yields no mutant is unproven, unless the code owner reviews the PR anyway.
#[test]
fn mutation_results_are_read_strictly() {
    let case = Case::new("strict-mutants");
    write(&case.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    write(&case.head(), "lints/src/no-missed-file", "");
    let tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    let error = check(&case.base(), &case.head(), &tools).expect_err("no missed.txt");
    assert!(error.contains("left no missed.txt"), "{error}");
    fs::remove_file(case.head().join("lints/src/no-missed-file")).unwrap();
    write(&case.head(), "lints/src/fake-mutants.txt", "");
    let none = check(&case.base(), &case.head(), &tools).unwrap();
    assert!(none.unproven[0].contains("yields no mutant"), "{}", none.summary());
    assert!(none.line().starts_with("ratchet: no verdict"));
    write(&case.head(), "xtask/expect-budget.txt", "1\n");
    let owned = check(&case.base(), &case.head(), &tools).unwrap();
    assert!(owned.owner_reviews && owned.unproven.is_empty(), "{}", owned.summary());
}

// Goal: the review gate counts only the reviewer app: a review by the author's app, a thread
// resolved by someone else, or no reviewer app set up at all is no proof.
#[test]
fn only_the_reviewer_app_counts() {
    let case = Case::new("reviewer");
    let own = FakeReviews {
        approval: None,
        reviewer: AUTHOR_APP,
        commit: "head",
        resolved_by: Some(REVIEWER_APP),
    };
    let verdict =
        check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, &own)).unwrap();
    assert_eq!(found(&verdict), vec![(Check::Review, "pull request #7".to_owned())]);
    let resolved = FakeReviews {
        approval: None,
        reviewer: REVIEWER_APP,
        commit: "head",
        resolved_by: Some(AUTHOR_APP),
    };
    let by_other =
        check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, &resolved)).unwrap();
    assert!(by_other.findings[0].message.contains("resolved by nami-agent[bot]"));
    let mut tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    tools.review = Some(ReviewSource { reviews: &REVIEWED, pull: 7, reviewer: None });
    let unset = check(&case.base(), &case.head(), &tools).unwrap();
    assert!(unset.unproven[0].contains("no reviewer app is set up"), "{}", unset.summary());
}

// Goal: the judge reads the base's corpus from its own checkout: observations made against a
// base whose fixtures the head job deleted are no verdict, and so is an empty base corpus.
#[test]
fn a_corpus_changed_in_the_head_job_gets_no_verdict() {
    let case = Case::new("corpus");
    let tampered = case.place.path("tampered");
    nami_tree(&tampered);
    for entry in fs::read_dir(tampered.join("xtask/fixtures/tidy")).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    let observed = observe(&tampered, &case.head(), &tools).unwrap();
    let error = judge(&case.base(), &case.head(), &observed, &tools).expect_err("another corpus");
    assert!(error.contains("do not cover the base's corpus"), "{error}");
    let honest = observe(&case.base(), &case.head(), &tools).unwrap();
    let mut away = honest.clone();
    let at_home = "crates/solver/src/example.rs";
    away.runs.retain(|run| {
        !(run.subject.ends_with("unbounded-loop.fires.fixture") && run.placement == at_home)
    });
    let moved = judge(&case.base(), &case.head(), &away, &tools).expect_err("no run at home");
    assert!(moved.contains("at crates/solver/src/example.rs"), "{moved}");
    let mut thinned = honest.clone();
    thinned.runs.retain(|run| !run.subject.ends_with("line-length.fires.fixture"));
    let missing = judge(&case.base(), &case.head(), &thinned, &tools).expect_err("a missing run");
    assert!(
        missing.contains("no `tidy` file run of xtask/fixtures/tidy/line-length.fires.fixture"),
        "{missing}"
    );
    assert!(judge(&case.base(), &case.head(), &honest, &tools).is_ok());
}

// Goal: without a prebuilt binary, the ratchet builds the head's xtask with cargo and runs it;
// a head whose xtask does not build gets no verdict, never a pass.
#[test]
fn the_head_xtask_is_built_from_the_head() {
    let case = Case::new("build");
    let mut tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    tools.head_xtask = None;
    let verdict = check(&case.base(), &case.head(), &tools).expect("a verdict");
    assert_eq!(verdict.line(), "ratchet: pass", "{}", verdict.summary());
    assert!(case.head().join("target/debug/xtask").is_file(), "built in the head");
    write(&case.head(), "broken-build", "");
    let error = check(&case.base(), &case.head(), &tools).expect_err("no verdict");
    assert!(error.contains("the head's xtask does not build"), "{error}");
}

// Goal: the command line refuses a base other than its own checkout and bad options (exit 2),
// exits 1 for a loosening and 0 for a pass that the code owner reviews anyway, and `observe`
// then `judge` give the same verdict as one run, with the verdict as the last line of stdout.
#[test]
fn the_command_line_reports_the_verdict_in_its_exit_code() {
    let case = Case::new("command-line");
    // A core change: the owner reviews, so no review and no mutants are needed for a verdict.
    write(&case.head(), "xtask/expect-budget.txt", "0\n\n");
    let real = std::env::var("CARGO_BIN_EXE_xtask").expect("cargo sets CARGO_BIN_EXE_xtask");
    let honest = executable(&case.place.path("honest.sh"), &fake_xtask(NOTHING, "any", NOTHING));
    let loosened = fake_xtask("error\\[line-length\\]", "any", NOTHING);
    let loose = executable(&case.place.path("loose.sh"), &loosened);
    let base = case.base();
    let run = |directory: &Path, arguments: &[&str]| {
        let output = Command::new(&real)
            .arg("ratchet")
            .args(arguments)
            .arg("--base")
            .arg(&base)
            .arg("--head")
            .arg(case.head())
            .args(["--no-mutants", "--seed", "5", "--work"])
            .arg(case.place.path("work"))
            .current_dir(directory)
            .output()
            .expect("xtask runs");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        (output.status.code(), stdout.lines().last().unwrap_or_default().to_owned())
    };
    let honest_path = honest.display().to_string();
    let loose_path = loose.display().to_string();
    let honest_args = ["--no-review", "--head-xtask", honest_path.as_str()];
    let loose_args = ["--no-review", "--head-xtask", loose_path.as_str()];
    let (code, line) = run(&base, &honest_args);
    assert_eq!(code, Some(0), "{line}");
    assert!(line.starts_with("ratchet: pass, outside the rule paths: 1"), "{line}");
    let (loose_code, loose_line) = run(&base, &loose_args);
    assert_eq!(loose_code, Some(1), "{loose_line}");
    let observations = case.place.path("observations.json");
    let out = observations.display().to_string();
    let observe_args = ["observe", "--out", out.as_str(), "--head-xtask", loose_args[2]];
    assert_eq!(run(&base, &observe_args).0, Some(0));
    let judge_args = ["judge", "--observations", out.as_str(), "--no-review"];
    let (judged_code, judged_line) = run(&base, &judge_args);
    assert_eq!(
        (judged_code, judged_line),
        (Some(1), loose_line),
        "the split gives the same verdict"
    );
    let (refused, why) = run(&case.head(), &honest_args);
    assert_eq!(refused, Some(2), "{why}");
    assert!(why.contains("`--base` must be the checkout whose ratchet runs"), "{why}");
    let bad = Command::new(&real).args(["ratchet", "--base", "."]).current_dir(&base).output();
    assert_eq!(bad.expect("xtask runs").status.code(), Some(2), "no `--head`");
}

// Goal: an approval of the head commit by a code owner turns what the ratchet cannot prove (no
// reviewer app, no mutant) into notes, so a rule-only PR needs the owner and never the bypass;
// an approval of an earlier commit, or by someone else, does not.
#[test]
fn a_code_owner_approval_stands_in_for_what_is_unproven() {
    let case = Case::new("approval");
    write(&case.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    write(&case.head(), "lints/src/fake-mutants.txt", "");
    let verdict = |approval: Option<(&'static str, &'static str)>| {
        let reviews = FakeReviews { approval, ..REVIEWED };
        let mut tools = case.tools(NOTHING, "any", NOTHING, &reviews);
        tools.review = Some(ReviewSource { reviews: &reviews, pull: 7, reviewer: None });
        check(&case.base(), &case.head(), &tools).unwrap()
    };
    let unapproved = verdict(None);
    assert_eq!(unapproved.unproven.len(), 2, "{}", unapproved.summary());
    let approved = verdict(Some(("owner", "head")));
    assert!(approved.owner_approved && approved.unproven.is_empty(), "{}", approved.summary());
    assert_eq!(approved.line(), "ratchet: pass");
    assert!(approved.notes.iter().any(|note| note.contains("approval of the head stands in")));
    assert_eq!(verdict(Some(("owner", "earlier"))).unproven.len(), 2, "an earlier commit");
    assert_eq!(verdict(Some(("someone", "head"))).unproven.len(), 2, "not a code owner");
}
