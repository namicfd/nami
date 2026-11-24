# Architecture decision records

The rules for how nami is written, tested, merged and paid for, and why. The process is defined in ADR-0000. Accepted ADRs are not edited; a new ADR supersedes them. Only a human can accept an ADR. Agents may propose them.

| ADR | Title | Status | One-line rule |
|---|---|---|---|
| [0000](0000-architecture-decision-records.md) | Record decisions as ADRs | Accepted | Policy lives here, with rationale and an enforcement mechanism. |
| [0001](0001-dependency-policy.md) | Dependency policy | Accepted | The only runtime dependency is `libm`. Dev and tooling crates are allow-listed. Anything new needs an ADR. |
| [0002](0002-integer-width.md) | Integer width and typed indices | Accepted | Use `u32` newtypes (`CellIndex`, …). `usize` is allowed only in `nami_core::index`. |
| [0003](0003-kernel-data-access.md) | Kernel crate, static allocation and data access | Accepted (amendment proposed in 0013) | Kernels are `no_std` without `alloc`. No allocation after init. Data access only via pencils, `[f64; NVAR]` and typed gathers. |
| [0004](0004-assertions-and-overflow.md) | Assertions, overflow checks and build profiles | Accepted | `assert!` at function boundaries, before loops and once per step. `debug_assert!` only for per-cell checks in kernels. Overflow checks stay on in release. |
| [0005](0005-floating-point-determinism.md) | Floating-point determinism contract | Accepted | Output is bitwise identical across platforms: `libm` only, no `algebraic_*`, `mul_add` only where written explicitly, fixed-order reductions. |
| [0006](0006-custom-checks.md) | Mechanical checks: clippy, tidy and dylint | Accepted (amendment proposed in 0013) | Each rule lives in exactly one tool. Error messages name the fix. Fixture tests prove every rule still fires. |
| [0007](0007-numerical-oracles.md) | Numerical oracles | Accepted | Oracles live in a code-owned crate, land before the feature, and are anchored to Toro, SymPy and PyClaw. Thresholds are human-owned. |
| [0008](0008-human-review.md) | Human review model | Accepted | Humans approve guardrails, oracles, ADRs and any change to output bits. Everything else auto-merges after the gates and an AI review. |
| [0009](0009-hosting-and-merge-gate.md) | Hosting and merge gate | Accepted | Public repo in a GitHub org with a merge queue. Strict ~5-minute gate with no test retries. Slow checks run after merge, with automatic revert. |
| [0010](0010-orchestration.md) | Agent orchestration | Accepted | Phase 1: interactive lead with worktree subagents. Phase 2: sandboxed `claude -p` bash-loop runner. |
| [0011](0011-task-tracking.md) | Task tracking | Accepted | Plans in `docs/exec-plans/` with a `tasks.json` whose `passes` field only CI flips. Claims are lock files on a `claims` branch, managed via `cargo xtask task`. |
| [0012](0012-budget.md) | Budget and spend controls | Accepted | Pilot at ~$1–3k/month with per-session and daily caps. Scaling up is decided on measured cost per merged PR. |
| [0013](0013-lint-table-refinements.md) | Lint table refinements from the first scaffold | Proposed | Drop `arithmetic_side_effects`, `default_numeric_fallback` and the `Vec` bans; allow `missing_panics_doc`. |

Research behind these decisions: [the report](../research/ai-agent-swarm-repo-readiness.md) and [its source notes](../research/ai-agent-swarm-repo-readiness-notes/).
