//! The ratchet (ADR-0022) against fake heads: trees that loosen a rule in one way each, run
//! through stand-ins for the head's xtask and for cargo.
//!
//! The base is a small nami tree with real tidy fixtures, the real rule registry, a CODEOWNERS
//! and one fake lint. Its tidy is the code under test, as in CI. A head's xtask is the real one
//! behind a filter that drops some of what it reports, which is what a loosened rule looks like
//! from outside. cargo is a script whose "lint library" is a list of `line:column` places in
//! `lints/src/fake-lint.txt`, and whose mutation testing reads `fake-mutants.txt`.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use xtask::ratchet::observations::Observations;
use xtask::ratchet::review::{
    AUTHOR_APP, Actor, Pull, REVIEWER_APP, Review, Reviews, Thread, ThreadPage,
};
use xtask::ratchet::tree::Scratch;
use xtask::ratchet::{
    Check, Faults, ReviewSource, STEPS, Tools, Verdict, canonical, carry, check, check_all, judge,
    judge_all, judge_unobserved, observe, settled,
};

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
/// - `cargo mutants --list` lists the lines of `<dir>/src/fake-mutants.txt`, or by default one
///   mutant on the first added line of each hunk of the diff; a run writes `outcomes.json`, with
///   each mutant's place, and `missed.txt` from `<dir>/src/fake-missed.txt`, and leaves
///   `missed.txt` out while `<dir>/src/no-missed-file` exists. The mutants listed in
///   `<dir>/src/fake-timeout.txt` time out: they are counted and written to `timeout.txt`, and
///   the run exits 3, missed mutants or not, as cargo-mutants 27.1.0 does.
/// - `cargo dylint --path <tree>/lints ...` "lints" every `.rs` file of the package it runs in,
///   reporting each `line:column` of `<tree>/lints/src/fake-lint.txt` as JSON.
/// - `cargo dylint --all -- --workspace ...`, which the head's `xtask dylint` runs, reports
///   them in human form on the examples of the crates of the workspace it runs in.
/// - Both exit 101 if they reported something and 0 if not, or 0 whatever they reported while
///   `lints/src/exit-zero` exists.
/// - `cargo nextest ...` and `cargo test ...`, the null mutant's test runs, pass unless the tree
///   they run in has a `fake-pins.txt` of `path size` lines and a file there is not of its
///   size: a test that pins the source text.
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
            --in-diff) diff="$2"; shift ;;
            --list) list=yes ;;
        esac
        shift
    done
    listing=$(mktemp)
    awk '/^\+\+\+ /{file=substr($0,7); next} /^--- /{next}
        /^@@/{split($3,a,","); line=substr(a[1],2)+0; first=1; next}
        /^\+/{if (first) print file ":" line ":1: fake"; first=0; line++; next}
        /^ /{line++}' "$diff" > "$listing"
    [ -e "$dir/src/fake-mutants.txt" ] && cp "$dir/src/fake-mutants.txt" "$listing"
    if [ "$list" = yes ]; then cat "$listing"; rm "$listing"; exit 0; fi
    total=$(grep -c . "$listing")
    missed=0
    [ -s "$dir/src/fake-missed.txt" ] && missed=$(grep -c . "$dir/src/fake-missed.txt")
    timeouts=0
    [ -s "$dir/src/fake-timeout.txt" ] && timeouts=$(grep -c . "$dir/src/fake-timeout.txt")
    mkdir -p "$output/mutants.out"
    entries=""
    while IFS= read -r mutant; do
        [ -n "$mutant" ] || continue
        file=${mutant%%:*}; rest=${mutant#*:}; line=${rest%%:*}
        summary=CaughtMutant
        grep -qxF "$mutant" "$dir/src/fake-missed.txt" 2> /dev/null && summary=MissedMutant
        grep -qxF "$mutant" "$dir/src/fake-timeout.txt" 2> /dev/null && summary=Timeout
        span="{\"start\":{\"line\":$line,\"column\":1},\"end\":{\"line\":$line,\"column\":2}}"
        scenario="{\"Mutant\":{\"file\":\"$file\",\"span\":$span}}"
        entry="{\"scenario\":$scenario,\"summary\":\"$summary\"}"
        entries="${entries:+$entries,}$entry"
    done < "$listing"
    rm "$listing"
    counts='{"outcomes":[%s],"total_mutants":%s,"missed":%s,"caught":%s,"timeout":%s,"unviable":0}'
    out="$output/mutants.out"
    caught=$((total - missed - timeouts))
    printf "$counts" "$entries" "$total" "$missed" "$caught" "$timeouts" > "$out/outcomes.json"
    if [ ! -e "$dir/src/no-missed-file" ]; then
        : > "$out/missed.txt"
        [ -e "$dir/src/fake-missed.txt" ] && cp "$dir/src/fake-missed.txt" "$out/missed.txt"
    fi
    [ -e "$dir/src/fake-timeout.txt" ] && cp "$dir/src/fake-timeout.txt" "$out/timeout.txt"
    # cargo-mutants 27.1.0, `LabOutcome::exit_code` in src/outcome.rs: a timeout wins.
    [ "$timeouts" -gt 0 ] && exit 3
    [ "$missed" -gt 0 ] && exit 2
    exit 0
fi
if [ "$1" = nextest ] || [ "$1" = test ]; then
    if [ -e fake-pins.txt ]; then
        while read -r file size; do
            if [ "$(wc -c < "$file")" -ne "$size" ]; then
                echo "test failed: $file is not $size bytes" >&2
                exit 101
            fi
        done < fake-pins.txt
    fi
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
    [ -e "$3/src/exit-zero" ] && exit 0
    [ -s "$3/src/fake-lint.txt" ] && exit 101
    exit 0
fi
for file in crates/*/examples/*.rs; do
    [ -e "$file" ] || continue
    while IFS=: read -r line column; do
        printf 'error: a fake lint\n  --> %s:%s:%s\n\n' "$file" "$line" "$column" >&2
    done < lints/src/fake-lint.txt
done
[ -e lints/src/exit-zero ] && exit 0
[ -s lints/src/fake-lint.txt ] && exit 101
exit 0
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

/// Declares the file module `name` in the lint library of `root`, so that rustc compiles it.
fn declare_module(root: &Path, name: &str) {
    let library = root.join("lints/src/lib.rs");
    let text = fs::read_to_string(&library).expect("the lint library exists");
    fs::write(library, format!("mod {name};\n{text}")).expect("writable");
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

/// The frozen copy of the rule-path file `relative` (`xtask::ratchet::frozen`), never the live
/// file, so that an additive change to a rule file cannot break these tests.
fn real(relative: &str) -> String {
    xtask::ratchet::frozen(relative)
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
             fn register_lints(store: &mut Store) { store.register_lints(&[NAMI_FAKE]); }\n"
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
    let names = [
        "unbounded-loop.fires",
        "unbounded-loop.passes",
        "line-length.fires",
        "direct-libm.fires",
        "direct-libm.passes",
    ];
    for name in names {
        let fixture = format!("xtask/fixtures/tidy/{name}.fixture");
        write(root, &fixture, &real(&fixture));
    }
}

/// GitHub as the review check sees it: the head commit `head`, opened by the agents' app.
struct FakeReviews {
    reviewer: &'static str,
    commit: &'static str,
    /// The state of the reviewer's review: `COMMENTED` or `APPROVED`.
    state: &'static str,
    resolved_by: Option<&'static str>,
    /// An approval: who, and on which commit.
    approval: Option<(&'static str, &'static str)>,
}

/// The reviewer app reviewed the head and resolved its one thread.
const REVIEWED: FakeReviews = FakeReviews {
    reviewer: REVIEWER_APP,
    commit: "head",
    state: "COMMENTED",
    resolved_by: Some(REVIEWER_APP),
    approval: None,
};

/// The reviewer app approved the head and resolved its one thread: what an edit to existing
/// rule code needs.
const APPROVED: FakeReviews = FakeReviews { state: "APPROVED", ..REVIEWED };

/// An app for a login with `[bot]`, a user for any other.
fn actor(login: &str) -> Actor {
    if login.ends_with("[bot]") { Actor::app(login) } else { Actor::user(login) }
}

impl Reviews for FakeReviews {
    fn pull(&self, _pull: u32) -> Result<Pull, String> {
        Ok(Pull { head: "head".to_owned(), author: Actor::app(AUTHOR_APP) })
    }
    fn reviews(&self, _pull: u32) -> Result<Vec<Review>, String> {
        let state = self.state.to_owned();
        let commit = self.commit.to_owned();
        let mut reviews = vec![Review { id: 1, author: actor(self.reviewer), commit, state }];
        if let Some((author, approved_commit)) = self.approval {
            let approved = "APPROVED".to_owned();
            reviews.push(Review {
                id: 2,
                author: actor(author),
                commit: approved_commit.to_owned(),
                state: approved,
            });
        }
        Ok(reviews)
    }
    fn threads(&self, _pull: u32, _after: Option<&str>) -> Result<ThreadPage, String> {
        let resolved_by = self.resolved_by.map(actor);
        let path = "lints/src/lib.rs".to_owned();
        let author = Some(Actor::app(REVIEWER_APP));
        let thread = Thread { path, author, resolved: resolved_by.is_some(), resolved_by };
        Ok(ThreadPage::last(vec![thread]))
    }
    fn dismissed(&self, _pull: u32) -> Result<BTreeMap<u64, String>, String> {
        Ok(BTreeMap::new())
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
                head: "head".to_owned(),
                reviewer: Some(REVIEWER_APP.to_owned()),
            }),
            seed: Some(11),
            commits: None,
            pr_head: None,
            commit: None,
            carried: None,
            faults: Faults::default(),
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
// the reviewer app approved with its thread resolved give no finding, and every check ran.
// The fake lint library is a data file, so a changed one is an edit to existing rule code.
#[test]
fn a_pure_tightening_passes() {
    let case = Case::new("tightening");
    write(&case.head(), "lints/src/fake-lint.txt", "2:5\n4:1\n1:1\n");
    write(&case.head(), "xtask/fixtures/tidy/new-rule.fires.fixture", "// path: a.rs\n");
    let tools = case.tools(NOTHING, "any", NOTHING, &APPROVED);
    let verdict = check(&case.base(), &case.head(), &tools).unwrap();
    assert_eq!(found(&verdict), Vec::new(), "{}", verdict.summary());
    let all =
        [Check::RuleSet, Check::TidyCorpus, Check::LintCorpus, Check::EndToEnd, Check::Review];
    assert!(all.iter().all(|check| verdict.checks.contains(check)), "{:?}", verdict.checks);
    assert_eq!(verdict.line(), "ratchet: pass (edit)", "{}", verdict.summary());
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

// Goal: an edit to existing rule code, here a changed literal in a function of `text.rs`, is
// "no verdict (edit)" while the reviewer app has only commented, whatever mutation testing
// shows; the summary lists the edit and both verdict lines name the class. The reviewer app's
// approval of the head settles it, and stands in for a hunk without a mutant of its own; so
// does a code owner's approval. A hunk without a mutant is still unproven without them,
// though another hunk of the same workspace has one.
#[test]
fn an_edit_needs_the_reviewer_app_s_approval() {
    let case = Case::new("edit");
    let text = "xtask/src/tidy/text.rs";
    write(&case.base(), text, &real(text));
    for tree in [case.base(), case.head()] {
        for kind in ["fires", "passes"] {
            for id in ["debt-marker", "test-goal"] {
                let fixture = format!("xtask/fixtures/tidy/{id}.{kind}.fixture");
                write(&tree, &fixture, &real(&fixture));
            }
        }
    }
    let message = "expect(\"source files have fewer than 2^32 lines\")";
    let reworded = "expect(\"source files have far fewer than 2^32 lines\")";
    write(&case.head(), text, &replaced(&real(text), message, reworded));
    let line = real(text).lines().position(|line| line.contains(message)).unwrap() + 1;
    let commented =
        check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, &REVIEWED));
    let commented = commented.unwrap();
    assert_eq!(commented.unproven.len(), 1, "{}", commented.summary());
    let edit = "edit to existing rule code needs the reviewer app's approval or a code owner's";
    assert!(commented.unproven[0].starts_with(edit), "{}", commented.summary());
    assert!(commented.line().starts_with("ratchet: no verdict (edit), 1"), "{}", commented.line());
    let shown = commented.summary();
    assert!(shown.contains("- Class: edit (1 change(s)") && shown.contains("- **Edit:** "));
    assert!(shown.contains("after ADR-0022 (rule paths unowned): `ratchet: no verdict (edit)"));
    write(&case.head(), "src/fake-mutants.txt", "xtask/src/tidy/text.rs:1:1: elsewhere\n");
    write(&case.base(), "src/fake-mutants.txt", "xtask/src/tidy/text.rs:1:1: elsewhere\n");
    let uncovered = format!("text.rs:{line} contains no mutant");
    let tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    let verdict = check(&case.base(), &case.head(), &tools).unwrap();
    assert!(verdict.unproven.iter().any(|what| what.contains(&uncovered)), "{}", verdict.summary());
    let per_hunk = verdict.unproven.iter().all(|what| !what.contains("yields no mutant"));
    assert!(per_hunk, "a workspace with a mutant is judged per hunk: {}", verdict.summary());
    let approved =
        check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, &APPROVED));
    let approved = approved.unwrap();
    assert!(approved.unproven.is_empty(), "{}", approved.summary());
    assert_eq!(approved.line(), "ratchet: pass (edit)");
    assert!(approved.notes.iter().any(|note| note.contains("stands in for the 1 hunk(s)")));
    let owner = FakeReviews { approval: Some(("owner", "head")), ..REVIEWED };
    let owned = check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, &owner));
    assert_eq!(owned.unwrap().line(), "ratchet: pass (edit)", "a code owner's approval");
}

// Goal: a data-only change that the rule-set check covers passes without a mutant: a new rule
// with its ban row in the registry. Beside it, a code hunk without a mutant is unproven.
#[test]
fn a_pure_new_ban_row_passes_without_mutants() {
    let case = Case::new("data-only");
    let registry = "xtask/src/tidy/registry.rs";
    let rule = "    Rule {\n        id: \"adr-index\",";
    let new_rule = "    Rule {\n        id: \"a-new-ban\",\n        kind: \
                    RuleKind::Source,\n        summary: \"A new ban.\",\n        reference: \
                    \"ADR-0000\",\n    },\n";
    let ban = "pub const PATH_BANS: &[PathBan] = &[PathBan {";
    let new_ban = "pub const PATH_BANS: &[PathBan] = &[PathBan {\n    id: \"a-new-ban\",\n    \
                   segment: \"banned_crate\",\n    allowed: &[],\n    scope: \
                   BanScope::Everywhere,\n    message: \"a new ban\",\n}, PathBan {";
    let edited =
        replaced(&replaced(&real(registry), rule, &format!("{new_rule}{rule}")), ban, new_ban);
    write(&case.head(), registry, &edited);
    for tree in [case.base(), case.head()] {
        write(&tree, "src/fake-mutants.txt", "");
    }
    let data = case.run(NOTHING, "any");
    assert!(data.checks.contains(&Check::Mutants), "{}", data.summary());
    assert!(
        data.notes.iter().any(|note| note == "Mutants in xtask: 0, missed: 0."),
        "{:?}",
        data.notes
    );
    assert!(data.findings.is_empty() && data.unproven.is_empty(), "{}", data.summary());
    assert!(data.line().starts_with("ratchet: pass"), "{}", data.summary());
    write(&case.head(), "xtask/src/tidy/text.rs", "fn rule() -> bool { true }\n");
    let mixed = case.run(NOTHING, "any");
    assert_eq!(mixed.unproven.len(), 1, "{}", mixed.summary());
    let what = &mixed.unproven[0];
    assert!(what.contains("yields no mutant") && what.contains("text.rs:1 contains"), "{what}");
    assert!(mixed.line().starts_with("ratchet: no verdict"), "{}", mixed.summary());
}

// Goal: a registry change counts as covered by the data checks only field by field: a
// reworded ban message passes without a mutant, and a changed `mechanism` of a planned row,
// which no data check compares and which is more than documentation, is unproven.
#[test]
fn a_registry_change_is_covered_only_in_the_fields_the_checks_read() {
    let case = Case::new("row-fields");
    let registry = "xtask/src/tidy/registry.rs";
    for tree in [case.base(), case.head()] {
        write(&tree, "src/fake-mutants.txt", "");
    }
    let reworded = replaced(&real(registry), "direct use of `libm`;", "a direct use of `libm`;");
    write(&case.head(), registry, &reworded);
    let message = case.run(NOTHING, "any");
    assert!(message.findings.is_empty() && message.unproven.is_empty(), "{}", message.summary());
    assert!(message.line().starts_with("ratchet: pass"), "{}", message.summary());
    let alloc = "        \"alloc-after-init\",\n        \"test\",";
    let mechanism = replaced(&reworded, alloc, &alloc.replace("\"test\"", "\"ci\""));
    write(&case.head(), registry, &mechanism);
    let changed = case.run(NOTHING, "any");
    assert_eq!(changed.unproven.len(), 1, "{}", changed.summary());
    let what = &changed.unproven[0];
    assert!(what.contains("`mechanism` of `alloc-after-init` in `PLANNED`"), "{what}");
    assert!(changed.line().starts_with("ratchet: no verdict"), "{}", changed.summary());
}

// Goal: a ban narrowed in code outside the scope of its case fails: the review's exploit, which
// adds `crates/solver` to `direct-libm`'s exceptions through a private table, and the variant
// that returns early for `crates/solver`. Each case that fires is also planted in every other
// scope class of the tree, so the head meets a case in `crates/solver`, outside `kernels`.
#[test]
fn a_ban_narrowed_outside_the_scope_of_its_case_fails() {
    for (name, only) in [("solver-table", "any"), ("solver-return", "--root")] {
        let case = Case::new(name);
        write(&case.head(), "crates/kernels/src/lib.rs", "//! The kernels.\n");
        write(&case.base(), "crates/kernels/src/lib.rs", "//! The kernels.\n");
        let verdict = case.run("crates/solver/.*error\\[direct-libm\\]", only);
        let fixture = "xtask/fixtures/tidy/direct-libm.fires.fixture:";
        let narrowed = verdict.findings.iter().filter(|finding| finding.place.starts_with(fixture));
        assert!(narrowed.count() > 0, "{name}: {}", verdict.summary());
        assert!(verdict.line().starts_with("ratchet: fail"), "{name}");
    }
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
        registry.replacen("scope: BanScope::Everywhere", "scope: BanScope::NamiCrates", 1);
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
    let expected = "`Visitor::check_body` no longer calls `self.check_float_equality(..)`";
    assert!(call.message.contains(expected), "{}", call.message);
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
    let decoy = "declare_lint! { pub NAMI_FAKE, Deny, \"a\" }\n\
                 declare_lint! { pub NAMI_FAKE, Warn, \"b\" }\n";
    write(&case.head(), "lints/src/lib.rs", decoy);
    write(&case.head(), "lints/src/fake-lint.txt", "2:5\n4:1\n");
    let twice = case.run(NOTHING, "any");
    assert!(twice.findings.iter().any(|finding| finding.message.contains("declared 2 times")));
    let hidden = format!("#[cfg(any())]\nfn gone() {{}}\n{decoy}");
    write(&case.head(), "lints/src/lib.rs", &hidden);
    let tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    let error = check(&case.base(), &case.head(), &tools).expect_err("a cfg in the lint library");
    assert!(error.contains("`#[cfg]`"), "{error}");
}

// Goal: a dylint run whose exit code disagrees with what it printed is no verdict: an error
// diagnostic beside a clean exit, for the library and end to end alike.
#[test]
fn a_dylint_exit_that_disagrees_is_no_verdict() {
    let case = Case::new("lint-exit");
    write(&case.head(), "lints/src/exit-zero", "");
    let tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    let error = check(&case.base(), &case.head(), &tools).expect_err("a clean exit beside errors");
    assert!(error.contains("lint library exited 0 and printed 2 error"), "{error}");
    let observed = observe(&case.base(), &case.head(), &tools).unwrap();
    let mut library_agrees = observed.clone();
    for run in library_agrees.runs.iter_mut().filter(|run| run.mode == "library") {
        run.exit = Some(101);
    }
    let end_to_end = judge(&case.base(), &case.head(), &library_agrees, &tools).unwrap_err();
    assert!(end_to_end.contains("`xtask dylint` on lints/ui/main.rs exited 0"), "{end_to_end}");
}

// Goal: on a rule-path-only diff, the "after ADR-0022" verdict under today's CODEOWNERS, which
// owns every guardrail path, is exactly the verdict under the flipped CODEOWNERS, for an
// unproven change (no mutant) and for a proven one; and today's verdict differs where the
// owner's review stands in.
#[test]
fn the_after_line_is_the_verdict_after_the_flip() {
    let case = Case::new("flip");
    let today = "/xtask/ @owner\n/lints/ @owner\n/docs/ @owner\n/.github/ @owner\n";
    let flipped = xtask::ratchet::scope::flipped_codeowners(today);
    let run = |codeowners: &str| {
        for tree in [case.base(), case.head()] {
            write(&tree, ".github/CODEOWNERS", codeowners);
        }
        let reviews = FakeReviews { approval: None, ..REVIEWED };
        check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, &reviews)).unwrap()
    };
    write(&case.head(), "xtask/src/tidy/text.rs", "fn rule() -> bool { true }\n");
    for mutants in ["xtask/src/tidy/text.rs:1:1: fake\n", ""] {
        write(&case.head(), "src/fake-mutants.txt", mutants);
        write(&case.base(), "src/fake-mutants.txt", mutants);
        let shadow = run(today);
        let after = run(&flipped);
        assert!(shadow.owner_reviews && !after.owner_reviews, "{}", after.summary());
        assert_eq!(shadow.line_after(), after.line(), "{}", after.summary());
        assert_eq!(after.line_after(), after.line(), "the flip is idempotent");
    }
    assert!(run(today).line().starts_with("ratchet: pass"), "the owner reviews today");
    assert!(run(&flipped).line().starts_with("ratchet: no verdict"), "no mutant, no owner");
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
    declare_module(&case.head(), "extra");
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
        verdict.notes.iter().any(|note| note == "Mutants in lints: 2, missed: 1."),
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
    declare_module(&case.head(), "extra");
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

// Goal: round 10 of #57's review: in a merge group, the judge of an observation without
// mutants reads the PR's carried mutation evidence. The identical rule-path diff passes with a
// note; carried evidence without a mutation run, of another head tree, or of another rule-path
// diff gives no verdict.
#[test]
fn a_merge_group_carries_the_pr_s_mutation_evidence() {
    let case = Case::new("carried");
    let text = "xtask/src/tidy/text.rs";
    write(&case.head(), text, "fn rule() -> bool {\n    true\n}\n");
    let tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    let observed = observe(&case.base(), &case.head(), &tools).unwrap();
    let carried = case.place.path("carried");
    for (tree, from) in [("base", case.base()), ("head", case.head())] {
        let _copy = Scratch::copy_of(&from, &carried.join(tree)).expect("copied");
    }
    let keep = |observations: &Observations| {
        fs::write(carried.join(carry::OBSERVATIONS), observations.to_json()).expect("writable");
    };
    keep(&observed);
    let mut queue = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    queue.mutants = false;
    let queued = observe(&case.base(), &case.head(), &queue).unwrap();
    assert!(queued.runs.iter().all(|run| run.mode != "mutants"), "the queue observes no mutants");
    let judged = || {
        let mut with = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
        with.carried = Some(carry::Carried::read(&carried));
        judge(&case.base(), &case.head(), &queued, &with)
    };
    let verdict = judged().unwrap();
    assert_eq!(verdict.line(), "ratchet: pass (additive)", "{}", verdict.summary());
    assert!(
        verdict.notes.iter().any(|note| note.contains("carried over")),
        "{}",
        verdict.summary()
    );
    let mut stripped = observed.clone();
    stripped.runs.retain(|run| run.mode != "mutants");
    keep(&stripped);
    assert!(judged().unwrap_err().contains("no mutation run of xtask"), "no mutation run");
    keep(&observed);
    write(&carried.join("head"), "docs/later.md", "Written after the observation.\n");
    assert!(judged().unwrap_err().contains("another head tree"), "a mismatched fingerprint");
    write(&carried.join("head"), text, "fn rule() -> bool {\n    false\n}\n");
    let mut other = observed.clone();
    other.head = xtask::ratchet::tree::fingerprint(&carried.join("head")).expect("readable").0;
    keep(&other);
    assert!(judged().unwrap_err().contains("another rule-path diff"), "another diff");
}

// Goal: round 7 of #57's review: a new test that pins its file through an existing helper
// (`read_optional`), which names nothing the class refuses, is additive, but the null mutant
// shows that a test depends on the file's source text, so the mutant evidence proves nothing
// and the diff gets no verdict. Without the pin, the same diff passes.
#[test]
fn a_test_that_pins_its_file_gives_no_verdict() {
    let case = Case::new("null-mutant");
    let text = "xtask/src/tidy/text.rs";
    let source = "fn rule() -> bool {\n    true\n}\n\n#[cfg(test)]\nmod tests {\n    // Goal: \
                  pin.\n    #[test]\n    fn pinned() {\n        let text = \
                  crate::ratchet::source::read_optional(std::path::Path::new(\"..\"), \
                  \"xtask/src/tidy/text.rs\").unwrap().unwrap();\n        \
                  assert_eq!(text.len(), 1);\n    }\n}\n";
    write(&case.head(), text, source);
    let tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    let unpinned = check(&case.base(), &case.head(), &tools).unwrap();
    assert!(unpinned.unproven.is_empty(), "{}", unpinned.summary());
    write(&case.head(), "fake-pins.txt", &format!("{text} {}\n", source.len()));
    let pinned = check(&case.base(), &case.head(), &tools).unwrap();
    let expected = "a test depends on the source text of xtask/src/tidy/text.rs";
    assert!(pinned.unproven.iter().any(|what| what.contains(expected)), "{}", pinned.summary());
    assert!(pinned.line().starts_with("ratchet: no verdict (additive)"), "{}", pinned.line());
    // Review of #55, thread 2: for an edit, the reviewer app's approval does not stand in for a
    // failed null run; a code owner's does.
    let fixture = "xtask/fixtures/tidy/line-length.fires.fixture";
    write(&case.head(), fixture, &format!("{}\n", real(fixture)));
    let approved =
        check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, &APPROVED));
    let approved = approved.unwrap();
    assert!(approved.unproven.iter().any(|what| what.contains(expected)), "{}", approved.summary());
    assert!(approved.line().starts_with("ratchet: no verdict (edit)"), "{}", approved.line());
    let owner = FakeReviews { approval: Some(("owner", "head")), ..APPROVED };
    let owned = check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, &owner));
    let owned = owned.unwrap().line();
    assert!(owned.starts_with("ratchet: pass (edit)"), "a code owner's approval: {owned}");
    write(&case.head(), fixture, &real(fixture));
    let observed = observe(&case.base(), &case.head(), &tools).unwrap();
    let mut dropped = observed.clone();
    dropped.runs.retain(|run| run.mode != "null");
    let error = judge(&case.base(), &case.head(), &dropped, &tools).unwrap_err();
    assert!(error.contains("no null-mutant run of xtask"), "{error}");
}

// Goal: an additive diff merges on the ratchet's proof alone: it passes when the reviewer app
// has not reviewed the head, and when no app is set up; but a request for changes or an open
// thread by the app fails it, and so does a thread resolved by someone else. An edit needs the
// app's approval: the author's app does not count, and a code owner's approval stands in.
#[test]
fn an_additive_diff_needs_no_review_and_an_edit_the_app_s_approval() {
    let case = Case::new("reviewer");
    let run = |reviews: &FakeReviews| {
        check(&case.base(), &case.head(), &case.tools(NOTHING, "any", NOTHING, reviews)).unwrap()
    };
    let own = FakeReviews { reviewer: AUTHOR_APP, ..REVIEWED };
    let unreviewed = run(&own);
    assert_eq!(unreviewed.line(), "ratchet: pass (additive)", "{}", unreviewed.summary());
    assert!(unreviewed.notes.iter().any(|note| note.contains("has not reviewed the head")));
    let objections = [
        (FakeReviews { state: "CHANGES_REQUESTED", ..REVIEWED }, "requests changes"),
        (FakeReviews { resolved_by: None, ..REVIEWED }, "is not resolved"),
        (FakeReviews { resolved_by: Some(AUTHOR_APP), ..REVIEWED }, "resolved by nami-agent[bot]"),
    ];
    for (reviews, expected) in &objections {
        let objected = run(reviews);
        assert!(objected.findings[0].message.contains(expected), "{}", objected.summary());
        assert!(objected.line().starts_with("ratchet: fail (additive)"), "{}", objected.line());
    }
    let mut tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    tools.review =
        Some(ReviewSource { reviews: &REVIEWED, pull: 7, head: "head".to_owned(), reviewer: None });
    let unset = check(&case.base(), &case.head(), &tools).unwrap();
    assert_eq!(unset.line(), "ratchet: pass (additive)", "{}", unset.summary());
    assert!(unset.notes.iter().any(|note| note.contains("No reviewer app is configured")));
    let fixture = "xtask/fixtures/tidy/line-length.fires.fixture";
    write(&case.head(), fixture, &format!("{}\n", real(fixture)));
    let edit = run(&FakeReviews { state: "APPROVED", ..own });
    assert!(edit.line().starts_with("ratchet: no verdict (edit), 1"), "{}", edit.summary());
    let owner = run(&FakeReviews { approval: Some(("owner", "head")), ..own });
    assert_eq!(owner.line(), "ratchet: pass (edit)", "an owner's approval: {}", owner.summary());
    assert_eq!(check(&case.base(), &case.head(), &tools).unwrap().unproven.len(), 1, "no app");
}

// Goal: the observer and the judge read one head: a head with a symbolic link is no verdict,
// and so are observations of a head whose files changed before the judge read it.
#[test]
fn the_judge_reads_the_head_that_was_observed() {
    let case = Case::new("one-head");
    let tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    let observed = observe(&case.base(), &case.head(), &tools).unwrap();
    write(
        &case.head(),
        "xtask/src/tidy/registry.rs",
        &format!("{}\n", real("xtask/src/tidy/registry.rs")),
    );
    let error = judge(&case.base(), &case.head(), &observed, &tools).unwrap_err();
    assert!(error.contains("another head tree"), "{error}");
    std::os::unix::fs::symlink("/etc/hostname", case.head().join("lints/src/linked.rs")).unwrap();
    let linked = check(&case.base(), &case.head(), &tools).unwrap_err();
    assert!(linked.contains("1 symbolic link(s), such as `lints/src/linked.rs`"), "{linked}");
}

// Goal: review of #55: a code owner's approval of the head commit settles a no verdict, here a
// head with a symbolic link, into a pass that lists the reason; without it, or with an approval
// of an earlier commit, the no verdict stands; and a verdict, findings included, is left as it
// is, so findings still fail.
#[test]
fn a_code_owner_approval_settles_a_no_verdict() {
    let case = Case::new("settled");
    std::os::unix::fs::symlink("/etc/hostname", case.head().join("lints/src/linked.rs")).unwrap();
    let settle = |reviews: &FakeReviews| {
        let tools = case.tools(NOTHING, "any", NOTHING, reviews);
        settled(&case.base(), &tools, check_all(&case.base(), &case.head(), &tools))
    };
    let owner = FakeReviews { approval: Some(("owner", "head")), ..REVIEWED };
    let verdict = settle(&owner).expect("the owner's approval settles it");
    assert_eq!(verdict.line(), "ratchet: pass", "{}", verdict.summary());
    assert!(verdict.owner_approved, "the summary says the owner approved");
    assert_eq!(verdict.range, "the two trees given", "the summary names what was judged");
    let settling = verdict.notes.iter().find(|note| note.starts_with("No verdict:"));
    let note = settling.map_or("", String::as_str);
    assert!(note.contains("No verdict: scope: the head holds 1 symbolic link(s)"), "{note}");
    assert!(note.contains("A code owner approved the head commit"), "{note}");
    assert!(settle(&REVIEWED).unwrap_err().contains("symbolic link"), "no approval");
    let earlier = FakeReviews { approval: Some(("owner", "earlier")), ..REVIEWED };
    assert!(settle(&earlier).is_err(), "an approval of an earlier commit");
    fs::remove_file(case.head().join("lints/src/linked.rs")).unwrap();
    let tools = case.tools("unbounded-loop", "any", NOTHING, &owner);
    let failed = settled(&case.base(), &tools, check_all(&case.base(), &case.head(), &tools));
    let failed = failed.unwrap();
    assert!(failed.line().starts_with("ratchet: fail"), "findings still fail: {}", failed.line());
}

/// GitHub as [`FakeReviews`] answers it, but the review threads cannot be read.
struct BrokenThreads(FakeReviews);

impl Reviews for BrokenThreads {
    fn pull(&self, pull: u32) -> Result<Pull, String> {
        self.0.pull(pull)
    }
    fn reviews(&self, pull: u32) -> Result<Vec<Review>, String> {
        self.0.reviews(pull)
    }
    fn threads(&self, _pull: u32, _after: Option<&str>) -> Result<ThreadPage, String> {
        Err("GitHub answered 502 for the review threads".to_owned())
    }
    fn dismissed(&self, pull: u32) -> Result<BTreeMap<u64, String>, String> {
        self.0.dismissed(pull)
    }
}

/// The settled verdict of a head whose tidy drops `unbounded-loop`, so that the corpus finds
/// something, with the code owner's approval of the head and `reviews` as GitHub, and with
/// `carried` evidence if given.
fn settled_with_findings(
    case: &Case,
    reviews: &dyn Reviews,
    carried: Option<Result<carry::Carried, String>>,
) -> Verdict {
    let mut tools = case.tools("unbounded-loop", "any", NOTHING, &OWNER_APPROVED);
    tools.review = Some(ReviewSource {
        reviews,
        pull: 7,
        head: "head".to_owned(),
        reviewer: Some(REVIEWER_APP.to_owned()),
    });
    tools.carried = carried;
    let verdict = settled(&case.base(), &tools, check_all(&case.base(), &case.head(), &tools));
    verdict.expect("a verdict with findings is a verdict")
}

/// The reviewer app reviewed the head, and the code owner approved it.
const OWNER_APPROVED: FakeReviews = FakeReviews { approval: Some(("owner", "head")), ..REVIEWED };

/// Asserts that `verdict` fails on the corpus's findings, with `late` among what it could not
/// judge, although the code owner approved the head.
fn assert_fails_with(verdict: &Verdict, late: &str) {
    assert!(verdict.line().starts_with("ratchet: fail"), "{}", verdict.summary());
    let corpus = verdict.findings.iter().any(|finding| finding.check == Check::TidyCorpus);
    assert!(corpus, "the corpus's findings are kept: {}", verdict.summary());
    assert!(verdict.failed.iter().any(|why| why.contains(late)), "{late}: {:?}", verdict.failed);
    assert!(verdict.summary().contains("**Not judged (no verdict):**"), "{}", verdict.summary());
}

// Goal: round 11 of #57's review: findings followed by a late no verdict fail, even with the
// code owner's approval of the head, and show both the findings and what was not judged; for
// each late step: a mutant that timed out, carried evidence of another tree or that cannot be
// read (round 13), review threads that cannot be read, and a rule-path file that does not
// parse.
#[test]
fn findings_survive_a_late_no_verdict() {
    let case = Case::new("late");
    write(&case.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    declare_module(&case.head(), "extra");
    write(&case.head(), "lints/src/fake-timeout.txt", "src/extra.rs:1:1: fake\n");
    assert_fails_with(&settled_with_findings(&case, &OWNER_APPROVED, None), "timed out");
    fs::remove_file(case.head().join("lints/src/fake-timeout.txt")).unwrap();
    let carried = case.place.path("carried");
    for (tree, from) in [("base", case.base()), ("head", case.head())] {
        let _copy = Scratch::copy_of(&from, &carried.join(tree)).expect("copied");
    }
    let observations = Observations { head: "another tree".to_owned(), ..Observations::default() };
    fs::write(carried.join(carry::OBSERVATIONS), observations.to_json()).expect("writable");
    let evidence = carry::Carried::read(&carried);
    let mismatch = settled_with_findings(&case, &OWNER_APPROVED, Some(evidence));
    assert_fails_with(&mismatch, "another head tree");
    let unread = Some(Err("cannot read the carried evidence".to_owned()));
    assert_fails_with(&settled_with_findings(&case, &OWNER_APPROVED, unread), "carried evidence");
    let broken = BrokenThreads(OWNER_APPROVED);
    assert_fails_with(&settled_with_findings(&case, &broken, None), "502 for the review threads");
    write(&case.head(), "xtask/src/tidy/text.rs", "fn broken( {\n");
    let unparsed = settled_with_findings(&case, &OWNER_APPROVED, None);
    assert_fails_with(&unparsed, "does not parse");
}

// Goal: review of #55, round 3: a manifest in a rule path is an edit that only a code owner's
// approval settles; the reviewer app's approval of the head is not enough.
#[test]
fn a_manifest_in_a_rule_path_needs_the_code_owner() {
    let case = Case::new("manifest");
    write(&case.head(), "lints/ui/Cargo.toml", "[package]\nname = \"cases\"\n");
    let run = |reviews: &FakeReviews| {
        let tools = case.tools(NOTHING, "any", NOTHING, reviews);
        check(&case.base(), &case.head(), &tools).expect("a verdict")
    };
    let app = run(&APPROVED);
    assert!(app.line().starts_with("ratchet: no verdict (edit)"), "{}", app.summary());
    let reason = "lints/ui/Cargo.toml is a manifest or build file in the rule paths";
    assert!(app.unproven.iter().any(|what| what.contains(reason)), "{:?}", app.unproven);
    let owner = run(&FakeReviews { approval: Some(("owner", "head")), ..APPROVED });
    assert!(owner.line().starts_with("ratchet: pass (edit)"), "{}", owner.summary());
}

// Goal: round 11 of #57's review: missed mutants in one workspace stay findings when another
// workspace's mutants time out.
#[test]
fn one_workspace_s_timeout_keeps_another_s_misses() {
    let case = Case::new("timeout-misses");
    write(&case.head(), "xtask/src/tidy/text.rs", "fn rule() {}\n");
    write(&case.head(), "src/fake-missed.txt", "xtask/src/tidy/text.rs:1:1: fake\n");
    write(&case.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    declare_module(&case.head(), "extra");
    write(&case.head(), "lints/src/fake-timeout.txt", "src/extra.rs:1:1: fake\n");
    let tools = case.tools(NOTHING, "any", NOTHING, &OWNER_APPROVED);
    let verdict = settled(&case.base(), &tools, check_all(&case.base(), &case.head(), &tools));
    let verdict = verdict.expect("the misses are a verdict");
    assert_eq!(found(&verdict), vec![(Check::Mutants, "xtask/src/tidy/text.rs:1:1".to_owned())]);
    assert!(verdict.failed.iter().any(|why| why.contains("timed out")), "{:?}", verdict.failed);
    assert!(verdict.line().starts_with("ratchet: fail"), "{}", verdict.line());
}

/// Asserts that `verdict`, settled with the code owner's approval, fails on `finding` and
/// lists `late` among what it could not judge.
fn assert_fails_on(verdict: &Verdict, finding: (Check, &str), late: &str) {
    assert!(verdict.line().starts_with("ratchet: fail"), "{}", verdict.summary());
    let expected = (finding.0, finding.1.to_owned());
    assert!(found(verdict).contains(&expected), "{:?} in {}", expected, verdict.summary());
    assert!(verdict.failed.iter().any(|why| why.contains(late)), "{late}: {:?}", verdict.failed);
}

// Goal: round 12 of #57's review, two of the reviewer's probes: with the code owner's approval,
// each fails and shows what it found. A missed mutant and a timeout in one workspace (exit 3,
// as cargo-mutants 27.1.0 gives it) keep the miss, and a miss counts though a rule-path file
// does not parse, so the diff cannot be classified. The other probes are the next two tests.
#[test]
fn the_reviewer_s_round_12_probes_fail() {
    let case = Case::new("round-12-miss-timeout");
    let two = "fn a() -> bool { true }\nfn b() -> bool { false }\n";
    write(&case.head(), "lints/src/extra.rs", two);
    declare_module(&case.head(), "extra");
    let listing = "src/extra.rs:1:1: fake\nsrc/extra.rs:2:1: fake\n";
    write(&case.head(), "lints/src/fake-mutants.txt", listing);
    write(&case.head(), "lints/src/fake-missed.txt", "src/extra.rs:1:1: fake\n");
    write(&case.head(), "lints/src/fake-timeout.txt", "src/extra.rs:2:1: fake\n");
    let approved = case.tools(NOTHING, "any", NOTHING, &OWNER_APPROVED);
    let settle = |tools: &Tools<'_>| {
        let verdict = settled(&case.base(), tools, check_all(&case.base(), &case.head(), tools));
        verdict.expect("a verdict with a finding")
    };
    let both = settle(&approved);
    assert_fails_on(&both, (Check::Mutants, "lints/src/extra.rs:1:1"), "timed out");
    let undeclared = Case::new("round-12-classify");
    write(&undeclared.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    declare_module(&undeclared.head(), "extra");
    write(&undeclared.head(), "lints/src/fake-missed.txt", "src/extra.rs:1:1: fake\n");
    write(&undeclared.head(), "lints/src/undeclared.rs", "fn broken( {\n");
    let tools = undeclared.tools(NOTHING, "any", NOTHING, &OWNER_APPROVED);
    let raw = check_all(&undeclared.base(), &undeclared.head(), &tools);
    let unclassified = settled(&undeclared.base(), &tools, raw).expect("a verdict");
    assert_fails_on(&unclassified, (Check::Mutants, "lints/src/extra.rs:1:1"), "does not parse");
}

// Goal: round 12 of #57's review: the steps that need no observations run when the
// observations fail. A rule-set loosening fails a head whose xtask does not build, and the
// reviewer app's request for changes fails observations of another head tree, both with the
// code owner's approval.
#[test]
fn findings_without_observations_still_fail() {
    let case = Case::new("round-12-unobserved");
    let registry = real("xtask/src/tidy/registry.rs");
    let renamed = replaced(&registry, "id: \"line-length\"", "id: \"line-length-v2\"");
    write(&case.head(), "xtask/src/tidy/registry.rs", &renamed);
    write(&case.head(), "broken-build", "");
    let mut tools = case.tools(NOTHING, "any", NOTHING, &OWNER_APPROVED);
    tools.head_xtask = None;
    let unbuilt = settled(&case.base(), &tools, check_all(&case.base(), &case.head(), &tools));
    let unbuilt = unbuilt.expect("a verdict with a finding");
    assert!(unbuilt.line().starts_with("ratchet: fail"), "{}", unbuilt.summary());
    assert!(unbuilt.findings.iter().any(|finding| finding.check == Check::RuleSet));
    assert!(
        unbuilt.failed.iter().any(|why| why.contains("does not build")),
        "{:?}",
        unbuilt.failed
    );
    let other = Case::new("round-12-mismatch");
    let changes =
        FakeReviews { state: "CHANGES_REQUESTED", approval: Some(("owner", "head")), ..REVIEWED };
    let requested = other.tools(NOTHING, "any", NOTHING, &changes);
    let observed = observe(&other.base(), &other.head(), &requested).unwrap();
    write(&other.head(), "docs/later.md", "Written after the observation.\n");
    let raw = xtask::ratchet::judge_all(&other.base(), &other.head(), &observed, &requested);
    let mismatched = settled(&other.base(), &requested, raw).expect("a verdict with a finding");
    assert!(mismatched.line().starts_with("ratchet: fail"), "{}", mismatched.summary());
    assert!(mismatched.findings.iter().any(|finding| finding.check == Check::Review));
    assert!(mismatched.failed.iter().any(|why| why.contains("another head tree")));
}

/// GitHub as [`FakeReviews`] answers it, but the reviews cannot be read, so neither can the
/// code owner's approval.
struct BrokenReviews(FakeReviews);

impl Reviews for BrokenReviews {
    fn pull(&self, pull: u32) -> Result<Pull, String> {
        self.0.pull(pull)
    }
    fn reviews(&self, _pull: u32) -> Result<Vec<Review>, String> {
        Err("GitHub answered 502 for the reviews".to_owned())
    }
    fn threads(&self, pull: u32, after: Option<&str>) -> Result<ThreadPage, String> {
        self.0.threads(pull, after)
    }
    fn dismissed(&self, pull: u32) -> Result<BTreeMap<u64, String>, String> {
        self.0.dismissed(pull)
    }
}

// Goal: round 12 of #57's review: corpus findings fail when the code owner's approval itself
// cannot be read.
#[test]
fn findings_fail_when_the_approval_cannot_be_read() {
    let case = Case::new("round-12-approval");
    write(&case.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    declare_module(&case.head(), "extra");
    let broken = BrokenReviews(OWNER_APPROVED);
    assert_fails_with(&settled_with_findings(&case, &broken, None), "502 for the reviews");
}

// Goal: round 13 of #57's review: no failed step drops another step's finding, by
// construction. The judge enters every step of `STEPS`, in that order, with observations,
// without them and with carried evidence that cannot be read. And for every step S and every
// other step T, a judge in which T finds something as it starts and S fails as it starts fails
// with T's finding and lists S's failure, though the code owner approved the head. A step added
// to the judge is in `STEPS`, or the first check fails; once there, the matrix covers it.
#[test]
fn no_failed_step_drops_another_step_s_finding() {
    let case = Case::new("faults");
    write(&case.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    declare_module(&case.head(), "extra");
    let mut tools = case.tools(NOTHING, "any", NOTHING, &OWNER_APPROVED);
    let (base, head) = (case.base(), case.head());
    let observed = observe(&base, &head, &tools).expect("the head is observed");
    let clean = judge_all(&base, &head, &observed, &tools);
    assert_eq!(clean.visited, STEPS, "the judge enters every step, in order");
    assert!(clean.findings.is_empty() && clean.failed.is_empty(), "{}", clean.summary());
    let unobserved = judge_unobserved(&base, &head, &tools, "the observing job failed");
    assert_eq!(unobserved.visited, STEPS, "without observations");
    tools.carried = Some(Err("the carried evidence cannot be read".to_owned()));
    assert_eq!(judge_all(&base, &head, &observed, &tools).visited, STEPS, "carried evidence");
    tools.carried = None;
    for fail in STEPS {
        for find in STEPS.into_iter().filter(|step| *step != fail) {
            tools.faults = Faults { fail: Some(fail), find: Some(find) };
            let pair = format!("{} fails, {} finds", fail.name(), find.name());
            let raw = judge_all(&base, &head, &observed, &tools);
            assert_eq!(raw.visited, STEPS, "{pair}: every step is entered");
            let verdict = settled(&base, &tools, raw).expect("a verdict with a finding");
            assert!(verdict.line().starts_with("ratchet: fail"), "{pair}: {}", verdict.summary());
            let place = format!("injected/{}", find.name());
            let kept = verdict.findings.iter().any(|finding| finding.place == place);
            assert!(kept, "{pair}: the finding is kept: {}", verdict.summary());
            let failure = format!("{}: an injected failure", fail.name());
            assert!(verdict.failed.contains(&failure), "{pair}: {:?}", verdict.failed);
        }
    }
}

// Goal: the judge draws the placements again from the seed: observations without a drawn run,
// with a run nobody drew, drawn from another seed, or of another commit are no verdict; and no
// planted case keeps the `// path:` header that would let rule code recognise it.
#[test]
fn the_judge_draws_the_placements_again() {
    let case = Case::new("drawn");
    let mut tools = case.tools(NOTHING, "any", NOTHING, &REVIEWED);
    tools.seed = None;
    tools.commit = Some("a".repeat(40));
    let observed = observe(&case.base(), &case.head(), &tools).unwrap();
    assert!(judge(&case.base(), &case.head(), &observed, &tools).is_ok());
    let homes = ["crates/solver/src/example.rs", "xtask/src/example.rs"];
    let drawn = observed
        .runs
        .iter()
        .position(|run| run.mode == "file" && !homes.iter().any(|home| run.placement == *home))
        .expect("a drawn placement");
    let mut missing = observed.clone();
    let _gone = missing.runs.remove(drawn);
    let error = judge(&case.base(), &case.head(), &missing, &tools).unwrap_err();
    assert!(error.contains("which the seed draws"), "{error}");
    let mut extra = observed.clone();
    let mut stray = observed.runs[drawn].clone();
    stray.placement = "crates/solver/src/elsewhere.rs".to_owned();
    extra.runs.push(stray);
    assert!(
        judge(&case.base(), &case.head(), &extra, &tools).unwrap_err().contains("does not draw")
    );
    let reseeded =
        xtask::ratchet::observations::Observations { seed: observed.seed ^ 1, ..observed.clone() };
    assert!(judge(&case.base(), &case.head(), &reseeded, &tools).unwrap_err().contains("seed"));
    tools.commits = Some(format!("{}..{}", "b".repeat(40), "c".repeat(40)));
    let other = judge(&case.base(), &case.head(), &observed, &tools).unwrap_err();
    assert!(other.contains("judge the commit that was observed"), "{other}");
    let headers = observed
        .runs
        .iter()
        .filter(|run| run.stdout.contains("// path:") || run.stderr.contains("// path:"));
    assert_eq!(headers.count(), 0);
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
    assert_eq!(verdict.line(), "ratchet: pass (additive)", "{}", verdict.summary());
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
    assert!(line.starts_with("ratchet: pass (additive), outside the rule paths: 1"), "{line}");
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

// Goal: an approval of the head commit by a code owner turns what the ratchet cannot prove (a
// hunk without a mutant) into notes, so a rule-only PR needs the owner and never the bypass;
// an approval of an earlier commit, or by someone else, does not.
#[test]
fn a_code_owner_approval_stands_in_for_what_is_unproven() {
    let case = Case::new("approval");
    write(&case.head(), "lints/src/extra.rs", "fn extra() -> bool { true }\n");
    declare_module(&case.head(), "extra");
    write(&case.head(), "lints/src/fake-mutants.txt", "");
    let verdict = |approval: Option<(&'static str, &'static str)>| {
        let reviews = FakeReviews { approval, ..REVIEWED };
        let mut tools = case.tools(NOTHING, "any", NOTHING, &reviews);
        tools.review = Some(ReviewSource {
            reviews: &reviews,
            pull: 7,
            head: "head".to_owned(),
            reviewer: None,
        });
        check(&case.base(), &case.head(), &tools).unwrap()
    };
    let unapproved = verdict(None);
    assert_eq!(unapproved.unproven.len(), 1, "{}", unapproved.summary());
    let approved = verdict(Some(("owner", "head")));
    assert!(approved.owner_approved && approved.unproven.is_empty(), "{}", approved.summary());
    assert_eq!(approved.line(), "ratchet: pass (additive)");
    assert!(approved.notes.iter().any(|note| note.contains("approval of the head stands in")));
    assert_eq!(verdict(Some(("owner", "earlier"))).unproven.len(), 1, "an earlier commit");
    assert_eq!(verdict(Some(("someone", "head"))).unproven.len(), 1, "not a code owner");
}

/// GitHub before the reviewer app exists, on a pull request nobody has reviewed: what an
/// additive diff needs no more than.
struct Unreviewed;

impl Reviews for Unreviewed {
    fn pull(&self, _pull: u32) -> Result<Pull, String> {
        Ok(Pull { head: "head".to_owned(), author: Actor::app(AUTHOR_APP) })
    }
    fn reviews(&self, _pull: u32) -> Result<Vec<Review>, String> {
        Ok(Vec::new())
    }
    fn threads(&self, _pull: u32, _after: Option<&str>) -> Result<ThreadPage, String> {
        Ok(ThreadPage::default())
    }
    fn dismissed(&self, _pull: u32) -> Result<BTreeMap<u64, String>, String> {
        Ok(BTreeMap::new())
    }
}

/// Applies the canonical `diff` to the copy `head` of the checkout: the frozen copies with its
/// edits applied, and its new files. Each file it edits must still equal its frozen copy, or
/// its anchors may have drifted, so the test stops and says which copy to update.
fn apply(head: &Path, diff: &canonical::Diff) {
    let changes = diff.changes().expect("the canonical diff applies to the frozen copies");
    for (relative, base, text) in &changes {
        if let Some(frozen) = base {
            let live = fs::read_to_string(head.join(relative)).expect("the file exists");
            assert!(
                live == *frozen,
                "{relative} differs from its frozen copy xtask/fixtures/ratchet/{relative}.frozen: \
                 update the frozen copy to the live file, then fix the anchors of the canonical \
                 diff `{}` in xtask/src/ratchet/canonical.rs if `canonical` tests fail",
                diff.name
            );
        }
        write(head, relative, text);
    }
}

/// The tools of a canonical run: the real cargo, dylint and cargo-mutants, scratch output in
/// `work`, and the review of a pull request nobody reviewed, with no reviewer app set up.
fn real_tools(work: PathBuf, mutants: bool, carried: Option<carry::Carried>) -> Tools<'static> {
    Tools {
        head_xtask: None,
        cargo: "cargo".into(),
        work,
        mutants,
        review: Some(ReviewSource {
            reviews: &Unreviewed,
            pull: 1,
            head: "head".to_owned(),
            reviewer: None,
        }),
        seed: None,
        commits: None,
        pr_head: None,
        commit: None,
        carried: carried.map(Ok),
        faults: Faults::default(),
    }
}

/// What a canonical run gives: the pull request's verdict and the merge group's.
struct Canonical {
    pull_request: Verdict,
    merge_group: Verdict,
}

/// The full ratchet as CI runs it, with the real tools. The base is a copy of this checkout,
/// and the head a copy with the canonical `diff` applied and `docs/RULES.md` written by the
/// head's xtask. The pull request's run observes with mutation testing and the null mutant
/// and judges. The merge group's run then observes the queued merge without them, as
/// `guardrails.yml` does, and judges with the pull request's observation carried over; its
/// base has moved on outside the rule paths (review of #55, thread 7).
fn run_canonical(diff: &canonical::Diff) -> Canonical {
    let place = Temporary::new(&format!("canonical-{}", diff.name));
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask is a member");
    let copy = |tree: &str| {
        let scratch = Scratch::copy_of(checkout, &place.path(tree));
        scratch.expect("the checkout can be copied").root().to_path_buf()
    };
    let (base, head) = (copy("base"), copy("head"));
    apply(&head, diff);
    let rules = Command::new("cargo")
        .args(["run", "--locked", "--quiet", "--package", "xtask", "--", "rules", "--write"])
        .current_dir(&head)
        .env("CARGO_TARGET_DIR", place.path("rules-target"))
        .status()
        .expect("cargo runs");
    assert!(rules.success(), "the head's xtask writes docs/RULES.md");
    let tools = real_tools(place.path("work-pr"), true, None);
    let observed = observe(&base, &head, &tools).expect("the pull request is observed");
    let pull_request = judge(&base, &head, &observed, &tools).expect("a verdict on the PR");
    println!("{}", pull_request.summary());
    let carried = place.path("carried");
    for (tree, from) in [("carried/base", &base), ("carried/head", &head)] {
        let _copy = Scratch::copy_of(from, &place.path(tree)).expect("the trees can be copied");
    }
    fs::write(carried.join(carry::OBSERVATIONS), observed.to_json()).expect("writable");
    let (queue_base, queue_head) = (copy("queue-base"), copy("queue-head"));
    for tree in [&queue_base, &queue_head] {
        write(tree, "docs/queued-meanwhile.md", "Another PR merged ahead in the queue.\n");
    }
    apply(&queue_head, diff);
    let listed = fs::read_to_string(head.join("docs/RULES.md")).expect("the head's rule list");
    write(&queue_head, "docs/RULES.md", &listed);
    // The clock is no input of a test (ADR-0005); whoever measures the queue's cost timestamps
    // these two lines of the output.
    println!("canonical {}: the merge group starts", diff.name);
    let queue_observe = real_tools(place.path("work-queue"), false, None);
    let queued = observe(&queue_base, &queue_head, &queue_observe).expect("the merge is observed");
    let carried = carry::Carried::read(&carried).expect("the carried evidence reads");
    let queue_judge = real_tools(place.path("work-queue-judge"), true, Some(carried));
    let merge_group = judge(&queue_base, &queue_head, &queued, &queue_judge).expect("a verdict");
    println!("canonical {}: the merge group ends\n{}", diff.name, merge_group.summary());
    Canonical { pull_request, merge_group }
}

/// Asserts what a canonical run must show: both verdicts additive passes, with mutation
/// testing, the merge group's from the carried evidence.
fn assert_canonical(run: &Canonical) {
    for verdict in [&run.pull_request, &run.merge_group] {
        assert_eq!(verdict.line_after(), "ratchet: pass (additive)", "{}", verdict.summary());
        assert!(verdict.checks.contains(&Check::Mutants), "mutants: {}", verdict.summary());
    }
    let carried = run.merge_group.notes.iter().any(|note| note.contains("carried over"));
    assert!(carried, "the merge group reads the PR's evidence: {}", run.merge_group.summary());
}

// Goal: round 8 of #57's review: the ratchet is complete for a canonical new text rule. The
// full ratchet, with real tools and mutants, on this checkout plus the rule, its dispatch call,
// its registry row and its two fixtures, passes as additive with no review; and so does the
// merge group's run, with the PR's mutation evidence carried over. The clock is no input of a
// test (ADR-0005), so the queue's time is measured from the timestamps of the output lines.
#[test]
#[ignore = "runs the full ratchet with real cargo, dylint and cargo-mutants for many minutes, \
            past nextest's limit; run it with `cargo test --locked -p xtask --test ratchet -- \
            --ignored canonical`"]
fn canonical_new_text_rule_passes_as_additive() {
    assert_canonical(&run_canonical(&canonical::TEXT_RULE));
}

// Goal: round 8 of #57's review: the ratchet is complete for a canonical new lint. The full
// ratchet, with real tools and mutants, on this checkout plus the lint, its registration and a
// UI case, passes as additive with no review; and so does the merge group's run, with the PR's
// mutation evidence carried over; the time is measured outside, as for the text rule.
#[test]
#[ignore = "runs the full ratchet with real cargo, dylint and cargo-mutants for many minutes, \
            past nextest's limit; run it with `cargo test --locked -p xtask --test ratchet -- \
            --ignored canonical`"]
fn canonical_new_lint_passes_as_additive() {
    assert_canonical(&run_canonical(&canonical::LINT));
}
