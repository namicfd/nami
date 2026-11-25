# nami: guide for coding agents

nami is a finite-volume framework for hyperbolic conservation laws (advection, Burgers, shallow
water, Euler, MHD), written in Rust in TigerStyle. Most of its code is written by agents. This file
lists the commands and rules you cannot infer from the code. The reasons behind the rules are in
`docs/design/` (ADRs) and `docs/TIGER_STYLE.md`. Read those when a rule surprises you.

## Commands

- `just check`: run before every commit, and make sure it passes. It runs fmt-check, tidy, clippy
  with warnings denied, the tests and the doctests.
- `just check-full`: `just check` plus `cargo deny`, the dylint lints (no recursion) and the tests
  in the `release-checked` profile.
- `just fmt`: format everything.
- `cargo nextest run -p <crate> <filter>`: run some tests while iterating.
- `cargo xtask tidy`: style rules that clippy cannot express.

The toolchain is pinned in `rust-toolchain.toml`. Always pass `--locked`.

Claude Code hooks (`.claude/settings.json`) give you feedback as you work:

- After every edit of a `.rs` file, the file is formatted and checked against the single-file
  tidy rules. Fix what they report straight away.
- Before you stop, `just check` runs if anything changed. You cannot finish while it fails.

## Repository map

| Path | What it is |
|---|---|
| `crates/base` | Typed indices (`CellIndex`, `CellCount`) and field views. The only place `usize` is allowed. `no_std`. |
| `crates/kernels` | Numerical kernels. `no_std` without `alloc`. `math` is the single entry point for `libm`; `reduce` holds the deterministic sums. |
| `crates/solver` | Control plane: `Solver::init` (all allocation), the time loop, boundaries, I/O. |
| `crates/alloc-guard` | Test-only counting allocator: `assert_no_allocations` around everything after init. The only crate with `unsafe` (ADR-0014). |
| `crates/reference` | Numerical references: exact solutions, manufactured solutions, convergence harness. **Protected.** |
| `xtask/` | Project tooling: `cargo xtask tidy`, the Claude Code hooks, and `agent-token.sh`, which makes git and gh act as the `nami-agent[bot]` GitHub App. |
| `lints/` | dylint library on a pinned nightly: compiler-level lints such as `nami_recursion`. |
| `docs/design/` | ADRs: every rule, its reason and how it is enforced. |
| `docs/operations/` | Runbooks: how GitHub and the `nami-agent` bot are set up, and how to set them up again. |
| `docs/TIGER_STYLE.md` | The full style guide. |
| `docs/exec-plans/` | Plans and `tasks.json` task lists (ADR-0011). |
| `docs/research/` | Background research. |

## Non-negotiables

Each rule is checked mechanically or in review. The reference says where the reason is written down.

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
  - Say so at the top of the PR description. A human decides at review.

## Protected paths

These paths need a human's approval at PR review (ADR-0008, ADR-0015). You may edit them when
your task needs it, and nothing blocks you locally. A PR that touches them waits for the code owner
instead of merging automatically, so:

- change them only as far as the task needs;
- put those changes in a commit of their own;
- list every protected file you changed in the PR description, with the reason.

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
    failed claim means someone else holds the task: pick another.
  - Read the task's `### <id>` section in its `plan.md` before you start.
  - Work on a branch named `task/<id>`. Touch only the task's `owner_paths`.
  - Never set `passes`; only CI does.
  - If you give up on a task, `cargo xtask task release <id>` and say why in the plan.
- **A reference lands before the feature it tests.** Never write the reference for code you also
  implement.
- **A change that alters output bits** is labelled `changes-results` and waits for a human. Explain
  the change and include before/after convergence numbers.
- **Commit messages say what changed and why.**
- **Done means all of these hold:**
  - `just check` passes;
  - the task's acceptance tests pass;
  - no new `#[expect]` without a real reason.
