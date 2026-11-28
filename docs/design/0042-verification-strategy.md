# ADR-0042: Verification strategy

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends, once accepted: ADR-0007 (thresholds as Rust constants; Athena++ as a second source of
  reference data)

## Context

ADR-0007 defines nami's references: a code-owned crate, references that land before the feature
they test, anchors outside the swarm, and tiers. ADR-0036 adds that every combination the
configuration can select is verified. ADR-0035 to ADR-0041 bring multi-D, AMR, ranks and restarts.
A suite that an implementer wires in by hand, check by check, would not scale to that, and an
implementer who chooses which checks to run can leave out the one that fails.

ADR-0007 also puts thresholds in `reference/thresholds.toml`. Reading TOML needs a parser, and
ADR-0001 rules out a crate. The closed PR #4 proposed Rust constants instead (its ADR-0016).

## Decision

1. **Check suites are chosen by declared properties.** `reference` defines one contract per
   equation system. A combination declares its properties (formal order, TVD,
   positivity-preserving, well-balanced) and is wired in with one line. `reference` maps
   properties to checks, and the implementer cannot pick or skip any.
2. **An outside anchor for every system:**
   - advection: the exact solution;
   - Burgers: the exact Riemann solution and Rankine–Hugoniot speeds;
   - Euler: Toro's tables;
   - shallow water: the exact dam break and the lake at rest;
   - MHD: Ryu–Jones Riemann problems and linear waves against Athena++'s published thresholds;
   - source terms, curvilinear coordinates and diffusion: SymPy-generated manufactured solutions.
3. **Athena++ is a second source of reference data,** besides PyClaw, for MHD and constrained
   transport. It is pinned, its data is committed with provenance, and only a maintainer-approved
   PR regenerates it.
4. **The architecture's bitwise tests:**
   - 1D embedded in 3D;
   - rotation along x, y and z;
   - full refinement against a uniform mesh;
   - decomposition (ADR-0039) and restart (ADR-0041);
   - conservation across levels with the exact accumulator.
5. **Benchmark problems are regression references:**
   - Sod, Shu–Osher, Woodward–Colella blast, double Mach reflection, Kelvin–Helmholtz, isentropic
     vortex;
   - Brio–Wu, Orszag–Tang, rotor, MHD blast, field loop, oblique linear waves.
6. **The gate is split:**
   - before merging: a fixed, deterministic sample at coarse resolution;
   - after merging: the full matrix and mutation testing, with automatic revert (ADR-0009).
7. **Thresholds are Rust constants** in `crates/reference/src/thresholds.rs`, each with a doc
   comment that derives it. There is no `thresholds.toml`.

## Enforcement

- CODEOWNERS on `crates/reference/` and `tools/` (ADR-0008), which covers the suites, the
  thresholds and the reference data.
- The mapping from properties to checks lives in `reference`, so a solver PR cannot change which
  checks run without a protected change.
- Tidy: the planned `tier-tag` rule. A planned rule also rejects a float literal used as a
  tolerance in a test outside `thresholds.rs`.
- The guardrails plan's task `thresholds-file` changes its target from `thresholds.toml` to
  `thresholds.rs`, and `TIGER_STYLE.md`'s rule on magic tolerances changes with it. Both are
  updated in the PR that writes the M0 plan.
- ADR-0009's post-merge job runs the full matrix and the mutation tests.

## Consequences

- Wiring a new combination costs one line, and its checks follow from what it claims.
- Claiming a property costs the checks that come with it. A combination that claims less is
  checked less, so review watches the declared properties.
- The full matrix runs after merging. A bug that only a rare combination shows can reach `main`
  for one post-merge run, and is then reverted automatically.
- Rust constants need a recompile to change. That is fine for values only a human may change.

## Alternatives considered

- **Each solver PR wires its own checks.** Rejected. The implementer chooses what is tested.
- **`thresholds.toml` read by the solver's configuration parser.** Rejected. The references would
  depend on code they check.
- **PyClaw as the only source of reference data.** Rejected for MHD. Athena++ is the more natural
  independent implementation of constrained transport.
- **The full matrix in the merge gate.** Rejected. It would break ADR-0009's budget as the menu
  grows.

## Evidence

- Toro, *Riemann Solvers and Numerical Methods for Fluid Dynamics*, 3rd ed., tables for tests 1–5.
- Ryu & Jones 1995, ApJ 442, 228 (MHD Riemann problems).
- Athena++'s linear-wave regression thresholds (ADR-0007, Evidence).
- The closed PR #4 and its ADR-0016 (`git fetch origin pull/4/head`).
- The maintainer's answers in the architecture discussion of 2026-11-26.
