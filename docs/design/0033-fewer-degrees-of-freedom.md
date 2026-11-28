# ADR-0033: Fewer degrees of freedom: one strict lint level, nightly rustfmt, and tests that agents cannot quietly weaken

- Status: Proposed
- Date: 2026-11-28
- Deciders: maintainer
- Amends: ADR-0006, ADR-0008, ADR-0013, ADR-0019

## Context

nami is written mostly by agents, and agents copy what they find. Every choice that the toolchain
leaves open is a choice that agents make differently from one session to the next: whether to
shadow, whether to write `std::fs::read` inline or import it, `pub` or `pub(crate)`, `1e-12` or
`1.0e-12`. Review catches some of these and misses the rest. `docs/TIGER_STYLE.md` has 50 `[review]`
tags, and 36 of its rules are held by review alone.

An audit on 2026-11-28 enabled clippy's restriction, nursery and cargo groups and 20 rustc lints
that are allowed by default, and counted what fired (Evidence). Most lints that remove a choice
fire 0 to 3 times in the nami crates. Nearly all firings are in `xtask/`. So the choices are cheap
to close in `crates/` now, before the first equation-system plan writes most of the code.

Two further gaps are specific to agents:

- **Weakening a test is the usual way agents cheat.** In ImpossibleBench (2025), most of the
  cheating by Claude models was editing tests. Making tests read-only stopped it without hurting
  legitimate work. The same study found that a stated way out ("if the task is impossible, say so")
  cut one model's cheating from 54% to 9%, and that more attempts led to more cheating.
- **The tooling is written by agents too.** Tidy, the hooks and the gate live in `xtask/`. A
  looser lint level for the code that checks everything else is the wrong way round.

## Decision

1. **One lint level for every crate, `xtask/` included.** The workspace lint table applies to
   all crates. Lints that encode a rule of nami's runtime, and that would be wrong for a CLI, are
   inner attributes in each nami crate's root file instead: `print_stdout`, `print_stderr`,
   `std_instead_of_core` and `std_instead_of_alloc`. Cargo cannot override workspace lints per
   crate, so this is the only place they can live. A tidy rule, `crate-header`, requires each nami
   crate root to start with the fixed header for its kind: `base` and `kernels` with
   `#![cfg_attr(not(test), no_std)]` and no `extern crate alloc`, every nami crate with the
   nami-only lints. Removing a line fails tidy.
2. **Pick one form and ban the other.** The workspace lint table adds, with every firing fixed
   in the same PR:
   - shadowing: `shadow_reuse` and `shadow_same`, next to `shadow_unrelated`. No name is
     shadowed, not even to refine the same quantity. This replaces the exception in
     `TIGER_STYLE.md`, Scope and size;
   - paths and visibility: `absolute_paths`, `unused_qualifications`, `unreachable_pub`;
   - forms: `unseparated_literal_suffix`, `mod_module_files`, `else_if_without_else`,
     `impl_trait_in_params`, `let_underscore_untyped`, `renamed_function_params`,
     `ambiguous_negative_literals`;
   - docs: `missing_docs_in_private_items`, `too_long_first_doc_paragraph`,
     `doc_paragraphs_missing_punctuation`;
   - rustc: `missing_debug_implementations`, `redundant_lifetimes`, `unused_lifetimes`;
   - integer division: `integer_division_remainder_used`. `/` and `%` on integers are replaced
     by functions whose name states the rounding: `base` gains `div_exact`, which asserts a zero
     remainder, and `div_ceil`; tooling uses `div_euclid` and `div_ceil`. This makes
     `TIGER_STYLE.md`'s "show your intent in integer division" mechanical, and drops its
     `/`-with-an-assertion form.
3. **Tighter `clippy.toml`:** `max-fn-params-bools = 0` (an enum instead of a `bool` parameter),
   `too-many-arguments-threshold = 4` (a parameter struct beyond that), `min_ident_chars` with the
   equation symbols of `TIGER_STYLE.md`, Naming, as the only short names, and a longer
   `disallowed-names` list of the abbreviations that the style guide already forbids.
4. **Lints considered and kept off**, each with its reason in a comment in the lint table, so
   that they are not proposed again:
   - `suboptimal_flops` and `imprecise_flops`: their suggestions rewrite arithmetic into `mul_add`
     and friends, which changes output bits (ADR-0005);
   - `float_arithmetic`, `implicit_return`, `question_mark_used`, `single_call_fn`,
     `missing_trait_methods`, `pattern_type_mismatch` and the `exhaustive_*` lints: they ban the
     ordinary way of writing the code, not a second way;
   - `arithmetic_side_effects`: overflow checks are on in every profile (ADR-0013);
   - `tests_outside_test_module`: it reports every integration test.
5. **New bans:**
   - `f64::min`, `f64::max`, `f64::clamp`, `f64::minimum`, `f64::maximum` and `f64::is_nan`
     everywhere, with wrappers in `kernels::math`. `NaN.max(1.0)` is `1.0`, so a NaN vanishes
     silently, and the standard library documents that either zero may be returned for `+0.0`
     and `-0.0`. The wrappers assert finite arguments and fix the result for equal arguments.
     Code tests for NaN only through `is_finite`;
   - `std::process::exit`, `std::process::abort`, `std::mem::forget`, `Box::leak` and
     `std::thread::spawn` everywhere. Threads need an ADR of their own;
   - in nami crates: `Rc`, `Arc`, `Cell`, `RefCell`, `Mutex` and `macro_rules!`. Shared mutable
     state and declarative macros hide control flow and data flow from the reader and from tidy.
     `alloc-guard` is exempt from the `Cell` ban only: its counter is a thread-local `Cell`, the
     one piece of shared state that a counting allocator needs, next to its `unsafe`
     (ADR-0014).
6. **Formatting uses the pinned nightly rustfmt.** It is the nightly that `lints/` already pins
   for dylint. The options that remove the most freedom are all unstable: `imports_granularity`,
   `group_imports`, `reorder_impl_items`, `wrap_comments`, `format_code_in_doc_comments`,
   `normalize_doc_attributes`, `error_on_line_overflow`. This reverses the stable-only choice
   recorded in `rustfmt.toml`. The edit hook, `just fmt`, `just fmt-check` and CI all format with
   that nightly, and `cargo xtask doctor` checks that it is installed.
7. **Tests change only with a human's consent.** A check that compares a PR with its base fails
   if the PR removes a `#[test]`, removes an assertion from a test that still exists, adds
   `#[ignore]` or `#[should_panic]`, or changes a float literal in a test or in a constant whose
   name ends in `_TOLERANCE` or `_THRESHOLD`. A `changes-tests` label lifts it only if a code
   owner added the label. The trust rule is the one that `needs-decision` uses (ADR-0021).
8. **Every gate failure names the way out.** The stop hook and the failure summaries of tidy,
   `just check` and the test-integrity check end with the same sentence: if the check, the test
   or the reference is wrong, stop and file it as an issue rather than work around it.
9. **Every `#[expect]` reason cites a record:** an ADR or a GitHub issue. A waiver then always
   points to the decision that allows it.
10. **Checks of the source and the repository** that no lint provides. The rules of
    `TIGER_STYLE.md` that they enforce are already written down; these are new:
    - `target-cfg`: `cfg(target_*)` only in `kernels::math`, the one place where platform
      differences may be handled (ADR-0005);
    - `float-literal-form`: an exponent follows a fractional part (`1.0e-12`), so that float
      literals have one form;
    - `markdown-title`: a Markdown document has one H1, on its first line, so that every document
      names itself the same way;
    - `file-kind`, `executable`, `text-bytes` and `file-size`: an allow-list of file kinds and
      executables, no tabs, carriage returns or trailing whitespace, and no large files, so that
      nothing enters the tree that no check reads;
    - `orphan-module`: every `.rs` file is reached by a `mod` item or is a root that Cargo finds,
      because Rust silently ignores any other file;
    - `cross-ref`: every ADR, path, style-guide section, rule id and `cargo xtask` command that a
      file cites exists, because agents invent citations;
    - `kernel-shape`: a kernel asserts each argument before its first loop and checks its output
      slices for finite values after its last, as `crates/kernels/src/update.rs` does;
    - `review-ratchet`: the number of `[review]` tags in the style guide only goes down;
    - two dylint lints against passing a test without doing the work: `cfg!(test)` inside a
      function that is not a test, and a hand-written `PartialEq` that returns a constant.

    A file-length limit was considered and left out: eleven files of `xtask/` are longer than
    1,000 lines today, two of them rule paths that grow by addition (ADR-0022).

The tasks that build each point are in `docs/exec-plans/active/strict-rust/plan.md`.

## Enforcement

- Points 2 to 4: the workspace lint table in `Cargo.toml` and `clippy.toml`, with a planted
  violation for every lint (guardrails task `clippy-fixtures`).
- Point 1: tidy rule `crate-header`.
- Point 5: `clippy.toml` bans for the paths; tidy ban tables, scoped to nami crates, for the
  types and the macro.
- Point 6: `just fmt-check`, run by `just check` and CI, and the edit hook.
- Point 7: `cargo xtask test-integrity`, a required CI check.
- Point 8: the fixtures of the stop hook and of each failure summary.
- Point 9: tidy, in its existing walk over `#[expect]` attributes.
- Point 10: the tidy rules and dylint lints it names, each with its fixtures or UI case.

## Consequences

- **Agents have less to decide:**
  - one way to name, import, shadow (never), divide and document;
  - review time goes to what the checks cannot see.
- **A large one-off cleanup of `xtask/`.** It is mechanical: 157 shadowing fixes, 223 imports, 49
  visibility changes, 133 doc comments. It conflicts with every open PR that edits `xtask/`, so
  each lint group is its own task, scheduled after the large open PRs merge.
- **Formatting needs the nightly.** A session without it cannot format. The session-start hook
  reports that, as it reports any other tool. The nightly is pinned, so the output does not
  drift; a bump of the pin may reformat, and lands as its own PR.
- **A legitimate test change waits for a human.** Renaming a test counts as removing one. That is
  the price of point 7, and the label makes it one click.
- **The 50 `[review]` tags shrink.** The plan turns about 15 of them into checks, and a ratchet
  keeps the count from growing again.

## Alternatives considered

- **A looser lint tier for `xtask/`.** Rejected, for the reasons in Context. A second tier is
  also one more thing to know, and code drifts into the looser tier.
- **Per-crate `clippy.toml` files for the nami-only bans.** Rejected for the reason in ADR-0013:
  copies drift. Inner attributes plus `crate-header` keep one copy per crate and check it.
- **Stay on stable rustfmt.** Rejected. Every option that fixes import layout, item order and
  comment width is unstable, and the nightly is already pinned and installed for dylint.
- **Make test files read-only by path, with CODEOWNERS.** Rejected. Tests live next to the code
  in `#[cfg(test)]` modules, so there is no path to protect. The check reads the test items
  instead.
- **Ban `/` on integers only in nami crates.** Rejected. Silent truncation is as wrong in tooling,
  and `div_euclid` costs nothing.
- **actionlint and zizmor on the workflows.** Deferred. Both are new tools under ADR-0001's
  allow-list, and they are the maintainer's call.

## Evidence

- **Lint audit, 2026-11-28,** on `main` at `5fa8f54` with clippy 0.1.100 (rustc 1.100.0). The
  table gives the firings in the nami crates (`crates/`) and in `xtask/`.

  | Lint | nami crates | xtask |
  |---|---|---|
  | `shadow_reuse` | 0 | 157 |
  | `shadow_same` | 0 | 2 |
  | `absolute_paths` | 0 | 223 |
  | `unused_qualifications` | 0 | 4 |
  | `unreachable_pub` | 0 | 49 |
  | `missing_docs_in_private_items` | 3 | 130 |
  | `too_long_first_doc_paragraph` | 0 | 10 |
  | `else_if_without_else` | 0 | 7 |
  | `integer_division_remainder_used` | 0 | 7 |
  | `impl_trait_in_params` | 1 | 2 |
  | `let_underscore_untyped` | 1 | 39 |
  | `renamed_function_params` | 2 | 41 |
  | `std_instead_of_core` | 4 | 22 |
  | `print_stdout`, `print_stderr` | 0 | 60 |
  | `min_ident_chars` (equation symbols allowed) | 8 | about 90 |
  | `fn_params_excessive_bools` at 0 | 0 | 12 |
  | `too_many_arguments` at 4 | 0 | 8 |

  `unseparated_literal_suffix`, `mod_module_files`, `doc_paragraphs_missing_punctuation`,
  `ambiguous_negative_literals`, `missing_debug_implementations`, `redundant_lifetimes` and
  `unused_lifetimes` fire nowhere. `suboptimal_flops` fires on `crates/kernels/src/update.rs`
  and suggests `mul_add`.
- **ImpossibleBench** (Carnegie Mellon and Anthropic, 2025): test editing as the main way that
  models cheat, read-only tests, the effect of a stated way out and of more attempts.
- **Anthropic's 2025 study of emergent misalignment from reward hacking:** a model that had
  learned to cheat sometimes wrote a deliberately weak detector when asked to build one.
- **TigerBeetle's `tidy.zig`, rust-lang's `tidy` and the Safety-Critical Rust Consortium's draft
  coding guidelines:** repository checks of the kind in the plan, and the ban on declarative
  macros.
- **The standard library documentation of `f64::max`:** a NaN argument returns the other
  argument, and the sign of a zero result is not specified.
