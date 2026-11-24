# ADR-0007: Numerical references

- Status: Accepted
- Date: 2026-11-24
- Deciders: maintainer

## Context

The dominant failure in LLM-written simulation code is code that "runs but solves the wrong physics": dropped terms, boundary conditions applied on the wrong boundary, and update steps that do nothing. LLM reviewers did not reliably catch any of these ([LLM-PDEveloper, arXiv 2509.25194](https://arxiv.org/html/2509.25194)).

A swarm is only as good as its reference. If the same model writes both the solver and its check, a single misconception can produce green CI on wrong physics. Rust has no mature finite-volume framework whose tests nami could reuse.

## Decision

### Ownership and process

1. All references live in **`reference`**, a crate code-owned by the maintainer (ADR-0008). It holds:
   - exact Riemann solvers;
   - manufactured solutions and their source terms;
   - the convergence-order harness;
   - invariant checkers;
   - reference-data loaders.
2. **Agents may write reference code**, but:
   - a reference PR lands **before** the feature it tests;
   - it is written by a **different agent session from the implementer**, with no shared context;
   - it merges only with human approval.
3. **Every reference is anchored to something outside the swarm:**
   - **Exact Riemann solver (Euler):** must reproduce Toro's published star-state values (p\*, u\*) for tests 1–5 to the printed digits. The values are checked in as constants, with the book citation. Other systems need an analogous published anchor before their solver merges.
   - **Manufactured solutions:** source terms come from committed SymPy scripts in `tools/reference-gen/` that generate Rust code (cell-averaged sources). They are never derived by hand. The generator and its output are both committed, and CI checks that regenerating produces no diff.
   - **Reference problems** (Shu–Osher, Brio–Wu, Orszag–Tang, …): data is produced offline by Clawpack/PyClaw. It is committed with a provenance file (tool version, command, input hash, date), and only a human-approved PR may regenerate it.
4. **Thresholds** live in one human-owned file, `reference/thresholds.toml`. They cover allowed order shortfall, absolute error ceilings and conservation tolerances.

### Reference tiers

Every test is tagged with its tier.

| Tier | Examples | Comparison | Who may change it |
|---|---|---|---|
| Invariants (every run) | conservation to round-off (compensated sums), positivity, TVD / maximum principle, entropy decay, free-stream preservation, lake-at-rest, ∇·B, CFL ≥ true max eigenvalue | exact or a few ULP | nobody (code) |
| Symmetry and determinism | mirror-symmetric input gives mirror output; a 1D problem rotated to y and z; same-seed rerun hash | bitwise | nobody |
| Convergence order (smooth problems only) | MMS, linear waves, isentropic vortex at ≥ 3 resolutions | observed order p ≥ p_formal − δ **and** an absolute error ceiling | human (thresholds) |
| Exact solutions (discontinuous problems) | Sod and Toro 1–5 via the exact solver, Burgers Rankine–Hugoniot shock speed, dam-break | L1 error ceiling, one-sided rate bounds | human-written anchors |
| Reference regression | Shu–Osher, Brio–Wu, Orszag–Tang norms and probe values | bitwise on CI (ADR-0005), with provenance | human-approved PR only |

### Rules for writing references

- **The solution must have moved correctly.** Every time-dependent reference asserts a *correct change*, for example a wave translated by c·t. A no-op solver must fail it. Steady-state or "error versus the initial condition" checks alone are not enough.
- **Each boundary-condition type gets its own reference**, for example a reflecting wall compared against a doubled periodic domain, and non-reflecting outflow. In the literature, boundary conditions were where agents were weakest.
- **Convergence tests run on smooth problems only.** Limiters drop to first order at shocks and extrema. The initial default is δ = 0.1, measured on the finest pair of resolutions, overridable per test in `thresholds.toml`. Error ceilings catch "right order, wrong constant" bugs.
- **Output stays terse.** One line per failure, starting with `ERROR` (greppable). Details go to a file. There is a deterministic `--fast` sample mode for agents.
- **Mutation testing checks the references.** A mutant in solver code that survives the reference suite means the reference is weak, not just the unit tests (ADR-0009).

## Enforcement

- CODEOWNERS on `reference/`, `tools/reference-gen/`, reference data and `thresholds.toml`.
- A Claude Code PreToolUse hook blocks agent edits to reference data and thresholds. It is backed by `permissions.deny`.
- CI regenerates the SymPy output and fails on any diff.
- Tidy requires every test under `tests/` to carry a tier tag.

## Consequences

- Correctness rests on anchors that no agent produced, and human review concentrates on small, high-value reference PRs.
- The repo needs a Python toolchain (`uv`) for offline generation. Building and testing nami does not.
- A new equation system cannot merge until its reference and published anchor exist.

## Alternatives considered

- **Humans write every reference.** Rejected. The maintainer becomes a bottleneck, and hand-derived MMS sources are error-prone.
- **Agents write references as ordinary tests.** Rejected. A single misconception can then pass in both the solver and its check.

## Evidence

- Athena++ linear-wave test: L1 error ≤ 3.7e-8 at 64 cells, error ratio ≤ 0.325, and bitwise left/right symmetry ([hydro_linwave.py](https://raw.githubusercontent.com/PrincetonUniversity/athena/master/tst/regression/scripts/tests/hydro/hydro_linwave.py)).
- Trixi.jl: inline error norms with atol = 500ε and rtol = √ε; any output on stderr fails the test.
- Clawpack: maintainers regenerate the reference data.
- Roy 2005 (MMS and observed order of accuracy).
- Judge-agent physics audits cut silent failures from 42% to 1.5% ([arXiv 2603.25780](https://arxiv.org/html/2603.25780v1), single author with a declared conflict of interest).
- Toro's tables and Rust's FMA behaviour were taken from background knowledge in the research phase. Verify them when implementing.
