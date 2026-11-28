# Strict Rust: one way to write each thing

## Goal

Remove the choices that the toolchain leaves open and that agents make differently in every
session. The decisions and their reasons are in ADR-0033 (Proposed). The plan does four things:

- switches on the lints that pick one form and ban the other, with every firing fixed;
- adds bans and tidy rules that turn `[review]` rules of `docs/TIGER_STYLE.md` into checks;
- guards the tests against being weakened to make a change pass;
- adds the CI checks that test what the ADRs claim: rustdoc, a bare-metal build, Miri and a wider
  determinism matrix.

**Who approves.** Since ADR-0022, a PR that changes only rule paths and only adds to them merges
on the `ratchet` check's proof, without the code owner. The tidy and dylint tasks are written to
fit that class wherever their rule's fixes allow; the rest touch the trusted core and wait for the
owner (ADR-0008, ADR-0015). The table's last column says which, and why. A task that turns out
to be an edit says so in its PR and asks for the owner's review, as the playbook says.

## Rules for every task

- **A lint or rule lands with its firings fixed,** in the same PR, in every crate it applies to,
  `xtask/` included (AGENTS.md, When a check is missing). Never narrow it to make it pass.
- **A new clippy lint or ban gets a planted violation** in the harness of guardrails task
  `clippy-fixtures`, and the task's acceptance test names it.
- **A new tidy rule** follows `docs/playbooks/add-a-guardrail.md`: a row in the registry, two
  fixtures (or a case in `broken_workspace()` for a workspace rule), `docs/RULES.md`
  regenerated.
- **A source rule is written additively** in the existing module that parses its input
  (`rust_source.rs`, `tokens.rs`, `text.rs`, `docs.rs`): new functions beside the old code, one
  dispatch call, no new constant and no `use` added to an existing file (ADR-0022, point 2). It
  is not tagged in `docs/TIGER_STYLE.md`, which is core: `retag-style-guide` tags them all in one
  owner-reviewed PR, unless #77 has moved the tags out of the style guide by then.
- **The lint tasks touch the core anyway** (`Cargo.toml`, `clippy.toml`), so they retag the style
  guide in the same PR.
- **A CI task's acceptance test reads the workflow** it changes, in `xtask/tests/workflows.rs`,
  which the first such task creates. CI sets `passes` only from tests that exist and run, so a
  job name is never an acceptance name.
- **A rule is stricter than the style guide's current wording only where ADR-0033 says so.**
  Anything else that a task finds it needs goes through a new ADR.

## Order

The tasks form one chain, inserted into the guardrails chain between `doc-example-sync` and
`usize-alias`. The last task, `determinism-matrix`, hangs off the end of the guardrails chain.

- **Why a chain.** Nearly every task edits `Cargo.toml`, `clippy.toml`,
  `xtask/src/tidy/registry.rs`, `docs/RULES.md` or `docs/TIGER_STYLE.md`, and the lint tasks edit
  most of `xtask/src`. Unfinished tasks that may run at the same time own disjoint paths
  (ADR-0011).
- **Why after `doc-example-sync`.** The lint tasks need its two harnesses: planted violations for
  clippy (`clippy-fixtures`), and style-guide examples that are compiled under the lints, so that
  an example cannot drift from a new lint (`doc-example-sync`).
- **Why before `usize-alias`.** The lints are cheap while `crates/` is small. Every line that the
  first equation-system plan writes before them is a line that a lint task has to fix. The two
  dylint gaps and the glossary cost the same whenever they land.
- **This plan goes before the M0 plan (#49)** (maintainer, 2026-11-28). So M0's `m0-scaffold`
  depends on `miri`, the last task of this chain. Tasks all along the chain own paths in
  `crates/` (the lint fixes, `kernel-shape`, `rustdoc-gate`, `miri`), so no earlier task ends the
  overlap. The validator reports the overlapping `owner_paths` until #49 adds that dependency.

The Approval column comes from a scan of the tree on 2026-11-28 for what each rule would report.
"Ratchet" means the task's rule fires nowhere outside the rule paths and unowned files of
`crates/`, so its PR can be additive. A rule whose fixes reach core files is "owner", and those
files are in its `owner_paths`. If a task finds firings that the scan missed, it fixes them, adds
their files to the PR, and asks for the owner; it never narrows the rule.

| # | Task | Mechanism | Approval |
|---|---|---|---|
| 1 | `rustfmt-nightly` | rustfmt, hook | owner |
| 2 | `lints-zero-cost` | lint table | owner |
| 3 | `crate-header` | tidy rule | owner (its headers touch `crates/reference`) |
| 4 | `lints-shadowing` | lint table | owner |
| 5 | `lints-visibility` | lint table | owner |
| 6 | `lints-paths` | lint table | owner |
| 7 | `lints-docs` | lint table | owner |
| 8 | `lints-signatures` | lint table, `clippy.toml` | owner |
| 9 | `lints-integer-division` | lint table, `base` helpers | owner |
| 10 | `lints-ident-length` | `clippy.toml` | owner |
| 11 | `float-minmax-ban` | `clippy.toml`, `kernels::math` | owner |
| 12 | `escape-hatch-bans` | `clippy.toml`, tidy ban tables | owner |
| 13 | `tidy-naming` | tidy rules | owner (11 abbreviated names in `xtask/` and `lints/`) |
| 14 | `kernel-shape` | tidy rule | ratchet |
| 15 | `assertion-shape` | tidy rule | owner (about 140 assertions in `xtask/`) |
| 16 | `literal-and-cfg-forms` | tidy rules | ratchet |
| 17 | `comment-sentence` | tidy rule | owner (2 comments in `xtask/`) |
| 18 | `markdown-title` | tidy rule | ratchet |
| 19 | `cross-ref` | tidy rule | owner (needs the tree as context) |
| 20 | `repo-hygiene` | workspace rules | owner |
| 21 | `orphan-modules` | workspace rule | owner |
| 22 | `expect-cites-record` | workspace rule | owner |
| 23 | `same-type-params` | dylint lint | ratchet |
| 24 | `test-cheat-lints` | dylint lints | ratchet |
| 25 | `retag-style-guide` | style guide | owner |
| 26 | `review-tag-ratchet` | tidy rule | ratchet |
| 27 | `test-integrity` | xtask command, CI | owner |
| 28 | `way-out-messages` | hook, tidy output | owner |
| 29 | `mutants-required` | CI | owner |
| 30 | `rustdoc-gate` | CI, crate headers | owner |
| 31 | `bare-metal-build` | CI | owner |
| 32 | `miri` | CI | owner |
| 33 | `determinism-matrix` | CI (after `determinism-hash`) | owner |

## Tasks

### rustfmt-nightly

Format with the nightly rustfmt of `lints/rust-toolchain.toml` (ADR-0033, point 6), and
reformat the tree in the same PR.

- `rustfmt.toml`: keep the current options and add `style_edition = "2024"`,
  `imports_granularity = "Module"`, `group_imports = "StdExternalCrate"`,
  `reorder_impl_items = true`, `wrap_comments = true`, `comment_width = 100`,
  `format_code_in_doc_comments = true`, `normalize_doc_attributes = true`,
  `format_macro_matchers = true`, `hex_literal_case = "Upper"`, `error_on_line_overflow = true`
  and `error_on_unformatted = true`. Replace the stable-only comment with a reference to ADR-0033.
- `just fmt` and `just fmt-check`, the edit hook (`cargo xtask hook post-edit`) and CI call
  `cargo +<pinned nightly> fmt`. Read the pin from `lints/rust-toolchain.toml`; never write it
  twice.
- `cargo xtask doctor` reports a missing nightly rustfmt with the command that installs it.
- The reformat is one commit of its own with no other change, so that `git blame` can skip it:
  list it in a new `.git-blame-ignore-revs`.

Acceptance: the hook test shows that the edit hook formats with the pinned nightly, and the
doctor test that a missing one is reported.

Out of scope: options that change code rather than layout (`use_try_shorthand` stays as it is).

### lints-zero-cost

Switch on the lints of ADR-0033, point 2, that fire at most a few times:
`unseparated_literal_suffix`, `mod_module_files`, `ambiguous_negative_literals`,
`else_if_without_else`, `impl_trait_in_params`, `doc_paragraphs_missing_punctuation`,
`unused_qualifications`, `shadow_same`, and the rustc lints `missing_debug_implementations`,
`redundant_lifetimes` and `unused_lifetimes`. Fix the firings: 7 `else if` chains, 3 `impl Trait`
parameters (one in `crates/alloc-guard`), 4 qualified paths and 2 `shadow_same`.

Add the comment block of ADR-0033, point 4, to the lint table: each lint that is kept off, and
why. Change the `TIGER_STYLE.md` examples that use `impl Trait` in a parameter.

### crate-header

Tidy rule `crate-header` (ADR-0033, point 1). Every nami crate root (`crates/*/src/lib.rs`, and
`main.rs` where there is one) starts with the inner attributes fixed for its kind, in a fixed
order, after the crate doc comment:

- `base`, `kernels`: `#![cfg_attr(not(test), no_std)]`, then the nami-only lints;
- every other nami crate, `alloc-guard` included: the nami-only lints. `GlobalAlloc` and
  `Layout` come from `core::alloc`; `std::alloc::System` has no `core` counterpart, so the lint
  does not report it.

The nami-only lints are `#![warn(clippy::print_stdout, clippy::print_stderr,
clippy::std_instead_of_core, clippy::std_instead_of_alloc)]`. `extern crate alloc` anywhere in
`base` or `kernels` is a finding. Write the headers inside the rule's function, not as a new
constant, so that the rule stays additive; `rustdoc-gate` later extends them. Fix the 4
`std_instead_of_core` firings in `alloc-guard`.

`TIGER_STYLE.md`'s `no_std` rule rests on `[compiler]` alone today, although nothing stops an
agent deleting the attribute. `retag-style-guide` adds this rule's tag.

### lints-shadowing

Switch on `shadow_reuse` and fix its 157 firings in `xtask/` by naming each stage of a value.
Rewrite `TIGER_STYLE.md`, Scope and size: no shadowing at all (ADR-0033, point 2), tagged with
the three `shadow_*` lints.

### lints-visibility

Switch on `unreachable_pub` and fix its 49 firings in `xtask/`: an item that is reachable only
inside its crate is `pub(crate)`. Items that the integration tests in `xtask/tests` use stay
`pub`.

### lints-paths

Switch on `absolute_paths` with the default `absolute-paths-max-segments` and fix its 223
firings in `xtask/`: import the item, or its module, and write the short name. Prefer importing
the module where the item name alone is ambiguous (`fs::read`, `io::Error`).

### lints-docs

Switch on `missing_docs_in_private_items` and `too_long_first_doc_paragraph`, and fix their
firings: 3 in the nami crates, 140 in `xtask/`. A doc comment says what the item is for, in one
sentence first. Rewrite `TIGER_STYLE.md`'s "every public item has a doc comment" to "every
item", tagged with both lints.

### lints-signatures

`clippy.toml`: `max-fn-params-bools = 0` and `too-many-arguments-threshold = 4`. Workspace lints:
`fn_params_excessive_bools`, `let_underscore_untyped` and `renamed_function_params`. Fix the
firings: 12 `bool` parameters (an enum with two named variants instead), 8 functions with 5 or 6
parameters (a parameter struct), 40 untyped `let _ =` and 43 renamed trait parameters. Retag
`TIGER_STYLE.md`'s "pass options explicitly" and "make arguments impossible to mix up" rules
where the lints now cover them.

### lints-integer-division

Switch on `integer_division_remainder_used` (ADR-0033, point 2). Add `div_exact` and
`div_ceil` to `crates/base` for the count types, each asserting its contract: `div_exact`
asserts a zero remainder, and both assert a non-zero divisor. Fix the 7 firings in `xtask/` with
`div_euclid`, `rem_euclid` or `div_ceil`, whichever states the rounding that the code means.
Rewrite `TIGER_STYLE.md`, Off-by-one errors, and tag it with the lint.

### lints-ident-length

`clippy.toml`: `min-ident-chars-threshold = 2`, with `allowed-idents-below-min-chars` set to
exactly the single-letter equation symbols that `TIGER_STYLE.md`, Naming, lists (`u`, `p`, `e`,
`c`, `q`, `f`) and the coordinates `x`, `y`, `z`, `t`. Extend `disallowed-names` with the
abbreviations the style guide forbids: `tmp`, `temp`, `val`, `idx`, `buf`, `ctx`, `cfg`, `src`,
`dst`, `cnt`, `res`, `ret`, `data`, `info`. Switch on `min_ident_chars`; fix about 100 firings,
8 of them in `crates/kernels/src/reduce.rs`. Closure parameters are names too: `|x|` over cells
becomes `|cell|`.

Out of scope: two-letter abbreviations such as `dt` and `dx`, which are equation symbols. The
`tidy-naming` task handles abbreviations in longer names.

### tidy-naming

Two tidy rules for `TIGER_STYLE.md`, Naming:

- `qualifier-last`: a name that a nami crate declares (a binding, parameter, field, function or
  constant) does not start with a qualifier: `max_`, `min_`, `num_`, `n_`, `total_`, `count_`,
  `first_`, `last_`. The message gives the rewritten name (`wave_speed_max`). Test functions are
  exempt, because their names are sentences. Today it fires once, on `count_allocation` in
  `crates/alloc-guard/src/lib.rs`.
- `abbreviation`: a segment of a name that the code declares, split at `_` and at case changes,
  is not one of the abbreviations of `lints-ident-length`, in any crate. Names the code only uses
  (`cfg`, `cfg_attr`, `BufReader`) are not its to choose, so they are exempt. Today it fires 11
  times, in `xtask/src` (the ratchet, `task/passes.rs`, tidy's registry, assertions and
  `rust_source.rs`), `xtask/tests/ratchet.rs` and `lints/src/recursion.rs`.

Also check that a closure or `Fn` parameter is the last in its function's parameter list
(`callbacks-last`). `retag-style-guide` tags the three rules. The fixes in `xtask/` are core
files, so this task waits for the owner.

### float-minmax-ban

Ban `f64::min`, `f64::max`, `f64::clamp`, `f64::minimum`, `f64::maximum` and `f64::is_nan` in
`clippy.toml` (ADR-0033, point 5). Add `kernels::math::min` and `kernels::math::max`, which
assert finite arguments and return the first argument when the two compare equal, so that the
sign of a zero result is fixed. Document that in their doc comments, with a test for `+0.0` and
`-0.0`. Fix any firings, and point the "no silent floors" rule in `TIGER_STYLE.md` at the
wrappers.

Out of scope: `f32`, which nami does not use; if it appears, the same bans apply.

### escape-hatch-bans

- `clippy.toml`, everywhere: `std::process::exit`, `std::process::abort`, `std::mem::forget`,
  `std::boxed::Box::leak` and `std::thread::spawn`, each with a reason that names what to do
  instead.
- Tidy, nami crates only: the types `Rc`, `Arc`, `Cell`, `RefCell` and `Mutex`. `PATH_BANS`
  matches only the leading path segment, so add a third declarative table that matches an
  identifier anywhere in a path or type, with the same row shape. `macro_rules!` is a new row
  in `MACRO_BANS`, allowed outside `crates/` only.
- `Cell` is allowed in `crates/alloc-guard`, whose counter is a thread-local `Cell` (ADR-0033,
  point 5). Nothing else in the nami crates uses these types today.
- Add the new table to the ratchet's `TABLES` (`xtask/src/ratchet/dialect.rs`), so that the
  ratchet proves its rows only tighten, as it does for the other ban tables.

Tooling keeps `RefCell` in its test doubles; that is why the type bans are scoped.

### kernel-shape

Tidy rule `kernel-shape` for `crates/kernels`, checking the template of
`crates/kernels/src/update.rs` (`TIGER_STYLE.md`, Numerical code):

- a public function takes no `self`;
- a public function asserts on each of its arguments before its first loop;
- a function with a `&mut [f64]` parameter asserts, after its last loop, that the slice is all
  finite (a call to `all_finite` on that parameter).

`update_pencil` in `crates/kernels/src/update.rs` passes all three, and the scan found no
firing elsewhere in `kernels`. Say in the rule's detail what a syntactic check cannot see: a
helper that asserts on the caller's behalf is not credited.

### assertion-shape

Tidy rule `assertion-shape`, everywhere: `assert!(a == b)` and `assert!(a != b)` are
`assert_eq!` and `assert_ne!`, which print both sides, and `assert!(a && b)` is two assertions,
which say which part failed (`TIGER_STYLE.md`, Assertions). Comparisons of floats stay in
`assert!`, because `float_cmp` already covers them; the rule skips an operand that is a float
literal. It applies to every Rust file that tidy walks, except `lints/ui/`, whose files are
compiler test input. Today it fires about 110 times on `&&` and 34 times on `==` or `!=`, in 29
files of `xtask/src` and `xtask/tests`, so this task waits for the owner.

### cross-ref

Tidy rule `cross-ref`: every reference in a Rust comment, a Markdown file or an error-message
string names something that exists:

- `ADR-NNNN` or `ADR-YYYYMMDD-HHMM`: a file in `docs/design/` (tidy's `adr-reference` checks
  this one already, in every file, ADR-20261128-1856);
- a repository path in backticks or a link: an existing file or directory;
- `TIGER_STYLE.md, <Section>`: a heading of the style guide;
- `error[<id>]` and `tidy rule <id>`: a registered rule;
- `cargo xtask <command>`: a command that `xtask/src/main.rs` dispatches.

References inside `docs/research/` and to other repositories are exempt. Agents invent
citations, and this codebase cites constantly.

### repo-hygiene

Workspace rules ported from TigerBeetle's `tidy.zig`. They read file modes, sizes and the whole
file list, so they live in tidy's core (`workspace.rs`) and wait for the owner:

- `file-kind`: every tracked file has an extension on an allow-list, or a name on a list
  (`justfile`, `LICENSE`, `CODEOWNERS`);
- `executable`: only files on a list have the executable bit;
- `text-bytes`: no tab, no carriage return and no trailing whitespace in a text file, and a
  final newline;
- `file-size`: no tracked file over 256 KiB, unless listed.

### markdown-title

Tidy rule `markdown-title`, in `docs.rs`: a Markdown document has exactly one H1, on its first
line. Lines inside fenced code blocks are not headings. Fixtures that are Markdown carry their
`<!-- path: .. -->` line first, so the rule reads the H1 after it.

It applies to the Markdown files under `docs/` and at the root, but not to:

- files that start with YAML frontmatter (the agents and skills under `.claude/`);
- GitHub templates under `.github/`;
- `CLAUDE.md`, which only includes `AGENTS.md`.

With code blocks skipped, it fires nowhere today.

### orphan-modules

Tidy rule `orphan-module`: every `.rs` file under `crates/`, `xtask/` and `lints/` is reached
from a crate root through `mod` items, or is itself a root that Cargo finds (`src/main.rs`,
`src/bin/`, `tests/`, `benches/`, `examples/`, `build.rs`), or is a fixture. Rust silently
ignores any other file, so its tests never run and its code is never checked.

### literal-and-cfg-forms

Two tidy rules:

- `float-literal-form`: a float literal with an exponent has a mantissa with a fractional part
  (`1.0e-12`, not `1e-12` or `1.e-12`), and none is written with a `_f64` suffix where its type
  is already fixed by an annotation. Decide in the task whether the second half is decidable
  without types; drop it if not, and say so in the rule's detail.
- `target-cfg`: `cfg(target_*)`, in an attribute or in `cfg!`, appears only in
  `crates/kernels/src/math.rs`, the one place where platform differences may be handled
  (ADR-0005). This is rust-lang's `pal` check.

### comment-sentence

Tidy rule `comment-sentence`, in `text.rs`: a `//` comment on a line of its own, and every
comment block, starts with a capital letter or a code span and ends with `.`, `:`, `?` or a code
block (`TIGER_STYLE.md`, Comments). End-of-line comments are exempt, as the style guide says, and
so are `lints/ui/` and the `// path:` line of fixtures. Today it fires on 2 comments, in
`xtask/src/ratchet/mutants.rs` and `xtask/src/task/passes.rs`, which are core files.

Out of scope: a limit on file length. ADR-0033, point 10, says why it was left out.

### retag-style-guide

One PR to the core: tag in `docs/TIGER_STYLE.md` every rule that the ratchet-merged tasks above
now check (`crate-header`, `tidy-naming`, `kernel-shape`, `assertion-shape`,
`literal-and-cfg-forms`, `comment-sentence`, `markdown-title`, `same-type-params`,
`test-cheat-lints`), replacing `[review]` where the check covers the whole rule. Add a style-guide
test that names each of these rules and requires its tag. If #77 has moved the tags into the
registry by then, tag the rows there instead, and the PR may merge on the ratchet.

### review-tag-ratchet

Tidy rule `review-ratchet`: the number of `[review]` tags in `docs/TIGER_STYLE.md` is at most the
number written in the rule's function, after `retag-style-guide` has lowered it. A PR that
mechanises a rule lowers the number. A PR that adds a review-only rule has to raise it, which is
an edit of the rule, so the code owner sees it.

### test-integrity

`cargo xtask test-integrity --base <ref>` (ADR-0033, point 7). It reads the test items of the
base and of the head with `syn` and fails if the head:

- removes a `#[test]` function, or a file under a `tests/` directory;
- has fewer assertion macros in a test that exists in both;
- adds `#[ignore]` or `#[should_panic]`;
- changes a float literal inside a test, or the value of a constant whose name ends in
  `_TOLERANCE` or `_THRESHOLD`.

A test is identified by its crate, module path and name, so a renamed test counts as removed. A
`changes-tests` label lifts the check only if a code owner added it; reuse the trust rule of
`xtask/src/issue/trust.rs`. CI runs it on every PR as a required check, and the failure message
lists each finding and says how to ask for the label.

CI runs the base branch's `xtask` on the PR's tree, as the base gate does (ADR-0020), so that a
PR cannot weaken the check and a test in the same change.

This task creates `xtask/tests/workflows.rs`: tests that read `.github/workflows/` and check that
a job runs a command, so that CI tasks have acceptance tests that CI can run. Its first test
checks that the CI gate runs `cargo xtask test-integrity`.

### way-out-messages

The stop hook, tidy's summary line, `just check` and `cargo xtask test-integrity` end every
failure with one sentence, defined once in `xtask` (ADR-0033, point 8): "If the check, the test
or the reference is wrong, do not work around it: stop, and file it with `cargo xtask issue file
--kind bug`." Add the same sentence to AGENTS.md, When a check fails, in place of a longer one.

Out of scope: the stop hook's number of retries. ImpossibleBench suggests fewer, but that is a
decision for after the sentence has been measured.

### expect-cites-record

Tidy, in its existing walk over `#[expect]`: a `reason` contains an ADR ID (`ADR-NNNN` or
`ADR-YYYYMMDD-HHMM`) or `#<n>`, and the ADR exists (ADR-0033, point 9). Fix the existing attributes.

### mutants-required

ADR-0009 makes `mutants (diff)` a required check once it reliably fits the time budget. Measure
its duration over the PRs merged since it was added, and record the numbers in this plan. If
the 95th percentile fits, drop `continue-on-error`, rename the job, and update the runbook step
that lists the required checks. If it does not, make it fit first: narrow `--in-diff` to the
crates in `crates/` and `xtask/src/tidy`, or shard it.

### same-type-params

Dylint lint `nami_same_type_params`: a function in a nami crate with two adjacent parameters of
the same scalar type (`f64`, an integer type, `bool`) or the same newtype is reported
(`TIGER_STYLE.md`, Naming, "make arguments impossible to mix up"). Slices are out of scope: the
kernels take several `&[f64]` of different lengths, which their assertions check. With that
scope, it fires nowhere today.

A symmetric function such as `minmod(a, b)` opts out with a `# Symmetric` section in its doc
comment, which says why swapping the arguments gives the same result. An `#[expect]` would use up
the human-owned budget and need a record to cite (`expect-cites-record`).

Declare it at `Deny`, so that the PR is additive. Its acceptance tests are the UI test and an
xtask test that it is declared at `Deny`, a test that cannot pass before the lint exists.
`retag-style-guide` tags the rule.

### test-cheat-lints

Two dylint lints against known ways of passing a test without doing the work:

- `nami_cfg_test_in_body`: `cfg!(test)` or a `#[cfg(test)]` statement inside the body of a
  function that is not a test, so that code behaves differently under test;
- `nami_constant_eq`: a hand-written `PartialEq::eq` (or `PartialOrd`) whose body returns a
  constant.

Both are declared at `Deny`, and as for `same-type-params`, an xtask test checks that.

### rustdoc-gate

`just check` runs `cargo doc --workspace --no-deps --document-private-items` with
`RUSTDOCFLAGS="-D warnings"`. The workspace lint table gains `rustdoc::missing_crate_level_docs`,
`rustdoc::unescaped_backticks` and `rustdoc::private_doc_tests`. `crate-header` adds
`#![doc(test(attr(deny(warnings))))]` to every nami crate's header, so that a doctest that
compiles with warnings fails. Fix the firings.

### bare-metal-build

CI builds `base` and `kernels` for `x86_64-unknown-none`, a target with no `std`. A dependency or
a `cfg` that pulls in `std` then fails the build, whatever the `no_std` attribute says. Add the
target to `rust-toolchain.toml`, a step to the CI gate and a `just` recipe that runs the same
build.

### miri

A post-merge job runs the tests of `base`, `kernels` and `alloc-guard` under Miri on the pinned
nightly, with several seeds. `alloc-guard` holds the repository's only `unsafe` (ADR-0014). Tests
that Miri cannot run (time limits, I/O) are skipped with `cfg_attr(miri, ignore)` and a reason.

Out of scope: a canonical solver run under Miri. It belongs to `determinism-matrix`, once a
solver exists.

### determinism-matrix

Widen guardrails task `determinism-hash` once it exists. The canonical problem set gives the same
output hash for:

- `target-cpu=x86-64` and `target-cpu=x86-64-v3`, where FMA contraction would show;
- the `dev` and `release` profiles;
- x86-64 and aarch64 runners;
- a run under Miri with float non-determinism randomised, on the smallest canonical problem.

A mismatch fails the post-merge job, and the job names the pair that differs.
