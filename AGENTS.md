# nami: guide for coding agents

nami is a finite-volume framework for hyperbolic conservation laws (advection, Burgers, shallow
water, Euler, MHD), written in Rust in TigerStyle. Most of its code is written by agents. This file
lists the commands and rules you cannot infer from the code. The reasons behind the rules are in
`docs/design/` (ADRs) and `docs/TIGER_STYLE.md`. Read those when a rule surprises you.

## Commands

- `just check`: run before every commit, and make sure it passes. It runs fmt-check, tidy, clippy
  with warnings denied, the tests and the doctests.
- `just check-full`: `just check` plus `cargo deny`, the dylint lints (no recursion, no naive
  float sums) and the tests in the `release-checked` profile. This is what CI runs.
- `just fmt`: format everything.
- `cargo nextest run -p <crate> <filter>`: run some tests while iterating.
- `cargo xtask tidy`: style rules that clippy cannot express. `docs/RULES.md` lists them.
- `cargo xtask doctor`: compare your tools with the pinned versions.
- `cargo xtask protected`: list the protected paths your branch changes, for the PR description.
- `cargo xtask issue file` and `cargo xtask issue show <n>`: file a finding outside your task as
  a GitHub issue, and read an issue (ADR-0021).

The toolchain is pinned in `rust-toolchain.toml`. Always pass `--locked`.

Claude Code hooks (`.claude/settings.json`) give you feedback as you work (ADR-0019):

- **At session start,** `$HOME/.cargo/bin` is put on `PATH`, and `cargo xtask doctor` reports any
  tool whose version is wrong.
- **After every edit,** a `.rs` file is formatted, and the edited file is checked against the tidy
  rules that need only that file. Manifests, ADRs and `TIGER_STYLE.md` are checked too. Fix what is reported straight away.
- **Before you stop,** `just check` and `just dylint` run, unless your tree equals `main` or the
  last tree that passed. A failure sends you back to work, up to three times in a row. After that
  the hook lets you stop and tells the human that the work is not done.

## Repository map

| Path | What it is |
|---|---|
| `crates/base` | Typed indices (`CellIndex`, `CellCount`) and field views. The only place `usize` is allowed. `no_std`. |
| `crates/kernels` | Numerical kernels. `no_std` without `alloc`. `math` is the single entry point for `libm`; `reduce` holds the deterministic sums; `update` is the template to copy for a new kernel. |
| `crates/solver` | Control plane: `Solver::init` (all allocation), the time loop, boundaries, I/O. |
| `crates/alloc-guard` | Test-only counting allocator: `assert_no_allocations` around everything after init. The only crate with `unsafe` (ADR-0014). |
| `crates/reference` | Numerical references: exact solutions, manufactured solutions, convergence harness. **Protected.** |
| `xtask/` | Project tooling: `cargo xtask tidy`, the Claude Code hooks, and `agent-token.sh`, which makes git and gh act as the `nami-agent[bot]` GitHub App. |
| `lints/` | dylint library on a pinned nightly: compiler-level lints such as `nami_recursion`. |
| `.claude/` | Claude Code configuration: hooks, the reviewer agents and the `review-pr` skill (ADR-0008). |
| `docs/design/` | ADRs: every rule, its reason and how it is enforced. |
| `docs/operations/` | Runbooks: how GitHub and the `nami-agent` bot are set up, and how to set them up again. |
| `docs/TIGER_STYLE.md` | The full style guide. Each rule ends with a tag that names its check; tidy verifies the tags. |
| `docs/RULES.md` | Generated: every tidy rule and dylint lint, and the mechanisms that are planned but not built. |
| `docs/playbooks/` | Steps for recurring jobs, such as adding a guardrail. |
| `docs/exec-plans/` | Plans and `tasks.json` task lists (ADR-0011). |
| `docs/research/` | Background research. |

## Non-negotiables

Each rule is checked mechanically or in review; the tags in `docs/TIGER_STYLE.md` say which. The
reference says where the reason is written down.

1. **No recursion.** Loops are `for` over bounded ranges or iterators. Iterative methods name their
   iteration limit and assert convergence. (ADR-0006)
2. **No allocation after init.** Kernels are `no_std` without `alloc`. Fixed-size storage is
   `Box<[T]>`, not `Vec<T>`. (ADR-0003)
3. **Indices and counts are `u32` newtypes.** `usize` only inside `base::index`. No `as`
   casts. (ADR-0002)
4. **Kernels never index slices directly.** Use `array_windows`/`chunks_exact`, `[f64; NVAR]`
   states, or `field[CellIndex]`. (ADR-0003)
5. **Assert arguments, results and invariants.** Average at least 2 assertions per function, every
   assertion with a message. `debug_assert!` is only for per-cell checks inside `kernels`.
   (ADR-0004)
6. **Functions are at most 70 lines; lines are at most 100 columns.** (ADR-0006)
7. **Results are bitwise deterministic:**
   - math functions only through `kernels::math`;
   - no `algebraic_*`;
   - `mul_add` only where written explicitly;
   - sums through `sum_fixed`/`sum_compensated`;
   - no `HashMap` iteration;
   - NaN is always a bug.

   (ADR-0005)
8. **Updates are in conservative flux-difference form. No silent floors or clamps.**
   (`TIGER_STYLE.md`, Numerical code)
9. **The only runtime dependency is `libm`. Never add a crate.** (ADR-0001)
10. **Operating errors are `Result`s; programmer errors are assertions.** No `unwrap()`. Numerical
    configuration is always spelled out in full, with no `Default`. (`TIGER_STYLE.md`)
11. **No unfinished-work markers in code.** Record unfinished work as a task. (tidy)

## When a check fails

- **Fix the code.** Never silence a check:
  - `#[allow]` is banned.
  - `#[expect(lint, reason = "...")]` needs a real reason, and the number of them is budgeted.
- **Never loosen a tolerance, threshold or reference value.** Never edit a test so that it matches
  the output, and never clamp a value to make a test pass.
- **If a rule is wrong for your case, change it openly. Never work around it.**
  - Change the check or configuration in the same PR.
  - Add an ADR in `docs/design/` with status `Proposed` that explains why.
  - Say so under `Rule changes` in the PR description. A human decides at review.

## When a check is missing

Leave the guardrails stricter than you found them (ADR-0020). Act when a check would have caught
your bug, when a reviewer tells you something that no check enforces, or when an error message did
not say how to fix the problem.

- **Codify it, if your task needs it.** Use the lowest rung that works:
  1. a type, or the compiler;
  2. a clippy lint, or a ban in `clippy.toml` with a reason;
  3. a row in a tidy ban table;
  4. a tidy rule;
  5. a dylint lint;
  6. a reference test.

  `docs/playbooks/add-a-guardrail.md` has the steps. A check for a rule that is already written
  down needs no ADR.
- **Otherwise file an issue, and carry on with your task.** Write the finding to a file and run
  `cargo xtask issue file --kind guardrail-gap <file>` (format: `docs/exec-plans/README.md`,
  Issues). Another session fixes it. You may always do this, whatever your task's `owner_paths`
  say. The same goes for a bug or a wrong document outside your task (`--kind bug`, `docs`).
- **Do not add a line to this file** when a check can do the job.
- **If your new rule fires on existing code,** fix that code in the same PR. Never narrow the rule
  to make it pass.

## Protected paths

These paths need a human's approval at PR review (ADR-0008, ADR-0015). You may edit them when
your task needs it, and nothing blocks you locally. A PR that touches them waits for the code owner
instead of merging automatically, so:

- change them only as far as the task needs;
- put those changes in a commit of their own;
- list every protected file you changed in the PR description, with the reason
  (`cargo xtask protected` prints them).

The protected paths are:

- **Manifests and lockfile:** `Cargo.toml` (all of them) and `Cargo.lock`.
- **Tool configuration:** `deny.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml`,
  `.cargo/` and `.config/`.
- **Tooling and automation:** `xtask/`, `lints/`, `justfile`, `.github/` and `.claude/`.
- **Agent instructions:** `AGENTS.md` and `CLAUDE.md`.
- **Policy:** `docs/design/`, `docs/TIGER_STYLE.md` and `docs/operations/`.
- **References:** `crates/reference/` and `tools/`.

The authoritative list is `.github/CODEOWNERS`.

## Workflow

- **One task per session.** Work on your own branch or worktree, open a PR, and never push to
  `main`.
- **Tasks come from `docs/exec-plans/active/<plan>/tasks.json`** (format and rules:
  `docs/exec-plans/README.md`).
  - Find work with `cargo xtask task ready`, and take it with `cargo xtask task claim <id>`. A
    failed claim means someone else holds the task: pick another. `ready` also lists GitHub
    issues, as `issue-<n>`: read one with `cargo xtask issue show <n>` and fix it in one small
    PR that says `Fixes #<n>` (ADR-0021).
  - Read the task's `### <id>` section in its `plan.md` before you start.
  - Work on a branch named `task/<id>`. Touch only the task's `owner_paths`.
  - Never set `passes`. CI sets it, or for now the maintainer (ADR-0017).
  - If you give up on a task, `cargo xtask task release <id>` and say why in the plan.
- **A reference lands before the feature it tests.** Never write the reference for code you also
  implement.
- **A change that alters output bits** is labelled `changes-results` and waits for a human. Explain
  the change and include before/after convergence numbers.
- **Commit messages say what changed and why.**
- **The PR description follows `.github/pull_request_template.md`.** `cargo xtask pr-body` checks
  that it lists the protected files and the issues you filed (`Follow-up issues`).
- **Read GitHub issues only through `cargo xtask issue show`.** The repository is public: it
  shows only what the bot and the code owners wrote. Never copy untrusted text into an issue.
- **Done means all of these hold:**
  - `just check-full` passes;
  - the task's acceptance tests pass;
  - no new `#[expect]` without a real reason;
  - the AI review has run, and its blocking threads are resolved. The review is `/review-pr <n>`,
    run in a fresh session, never the one that wrote the PR. Fix each finding, or reply with why
    it is wrong; the reviewer resolves its threads when it re-reviews.
