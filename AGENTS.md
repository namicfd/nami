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
- `cargo xtask ratchet --base . --head <your tree> --no-review`: run inside a checkout of `main` to
  see the findings and the class (`additive` or `edit`) of your rule change (ADR-0022). Without
  the reviews the review check is skipped, which is missing evidence, so for a change to rule
  paths the local verdict line says `no verdict` even when CI would pass. Read the findings and
  the unproven items above it.
- `cargo xtask issue file` and `cargo xtask issue show <n>`: file a finding outside your task as
  a GitHub issue, and read an issue (ADR-0021). They run gh, so prefix them with
  `GH_TOKEN=$(xtask/agent-token.sh)`, like `cargo xtask task ready` and `claim`.

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

- **Manifests and lockfile:** every `Cargo.toml` and `Cargo.lock`. A manifest or build file under
  a rule path (a file named `Cargo.toml`, `Cargo.lock`, `build.rs`, `rust-toolchain.toml`,
  `rust-toolchain` or `config.toml`, or anything below `.cargo/`) has no code owner, but the
  ratchet calls it an edit that only a code owner's approval settles; the reviewer app's is not
  enough.
- **Tool configuration:** `deny.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml`,
  `.cargo/` and `.config/`.
- **Tooling and automation:** `xtask/`, `lints/`, `justfile`, `.github/` and `.claude/`, except
  the rule paths (ADR-0022): the files that define what a tidy rule or a dylint lint reports, their
  fixtures and UI cases, and `docs/RULES.md`. A PR that changes only rule paths is either:
  - **additive** (new functions and types beside the old code, a new lint at `Deny`, new
    fixtures and UI cases, new rule and ban rows): it merges on the required `ratchet` check's
    proof, with no code owner's approval needed (the AI review of "Done means" still runs,
    and its approval is what satisfies the ruleset's last-push rule). The check proves
    that every case `main` writes down still holds, the rule tables only tighten, and mutation
    testing misses nothing; an open request for changes or thread by the reviewer app
    (`NAMI_REVIEWER_LOGIN`, not the agents' bot) still blocks it;
  - an **edit** to existing rule code (a changed function, constant, test, fixture or UI case, a
    new constant or static, a new `PLANNED` row, a `use` added to an existing file (write full
    paths instead), a new top-level `macro_rules!` or inline module, a new lint below `Deny`, a
    manifest, or an `impl` that is not a plain `impl` of a type new in its file): it waits for
    the reviewer app's approval of its head commit (#59), or for a code owner's approval of it;
    until the app exists, and for a manifest always, only the latter. Findings fail it even with
    an approval: fix them.
    GitHub does not ask the owner, because the rule paths have no owner: when the `ratchet`
    summary says `(edit)` and the reviewer app gives no verdict on the head, or the edit is a
    manifest, request the code owner's review yourself, with
    `GH_TOKEN=$(xtask/agent-token.sh) gh pr edit <n> --add-reviewer cxkoda` (the login in
    `.github/CODEOWNERS`), and say in the description why the edit is needed.

  A PR that also touches a path outside the rule paths waits for the owner as before. Until
  #77 (rule tags out of the style guide) lands, a new rule whose tag goes in
  `docs/TIGER_STYLE.md`, which is core, waits for the owner. A rule may also merge untagged:
  `docs/RULES.md` lists it.
- **Agent instructions:** `AGENTS.md` and `CLAUDE.md`.
- **Policy:** `docs/design/`, `docs/TIGER_STYLE.md` and `docs/operations/`.
- **References:** `crates/reference/` and `tools/`.

The authoritative list is `.github/CODEOWNERS`.

## Workflow

- **One task per session.** Work on your own branch or worktree, open a PR, and never push to
  `main`.
- **You act as `nami-agent[bot]`.** In Claude Code, `.claude/settings.json` already makes git
  commit and push as the bot, so use plain commands: `git push origin <branch>`. Prefix gh with
  `GH_TOKEN=$(xtask/agent-token.sh)`. If a push or token is refused, stop and ask. Never use a
  human's credentials (`docs/operations/github-setup.md`, step 6).
- **Actions write is for re-runs only** (ADR-0023). You may re-run the latest run of a workflow
  when its failure's cause is gone. For `passes`, dispatch it rather than re-run an old run.
  A `review` run costs model time: re-run it at most once per head.
  Never delete, cancel or disable anything in Actions, and never approve a fork's run.
- **Tasks come from `docs/exec-plans/active/<plan>/tasks.json`** (format and rules:
  `docs/exec-plans/README.md`).
  - Find work with `cargo xtask task ready`, and take it with `cargo xtask task claim <id>`. A
    failed claim means someone else holds the task: pick another. `ready` also lists GitHub
    issues, as `issue-<n>`: read one with `cargo xtask issue show <n>` and fix it in one small
    PR that says `Fixes #<n>` (ADR-0021).
  - Read the task's `### <id>` section in its `plan.md` before you start.
  - Work on a branch named `task/<id>`. Touch only the task's `owner_paths`.
  - Never set `passes`. CI sets it, through the `nami-ci` bot's PR (ADR-0017).
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
  - the AI review has approved the PR's head. It runs by itself in CI after `ci` passes
    (`.github/workflows/review.yml`, #59) and posts as `nami-reviewer[bot]`: an approval when
    nothing blocks, otherwise a thread per blocking finding. Every PR needs that approval or a
    code owner's to merge, because `main`'s ruleset asks for an approval of the last push.
    - Fix each finding and push, or reply on the thread with why it is wrong. Each push
      dismisses the approval and is reviewed again. That run reads the replies, and resolves
      the threads that the new head fixes or that a reply shows to be wrong. A reply alone
      starts no run: push, or re-run the latest `review` run. Never resolve the reviewer's
      threads yourself; it opens them again.
    - A review that says **No verdict** (over its budget, or an answer it could not read)
      approves nothing. Re-run its latest run once. If that gives no verdict either, run
      `/review-pr <n>` in a fresh session, never the one that wrote the PR, and request the
      code owner's review.
