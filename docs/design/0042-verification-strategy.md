# ADR-0042: Verification strategy

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends, once accepted: ADR-0007 (thresholds as Rust constants; Athena++ as a second source of
  reference data) and ADR-0001 (the pinned Athena++ of ADR-0034 point 8 becomes a
  reference-generation tool beside SymPy and PyClaw)

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
   equation system. Each supported combination has one row in a table in `reference` that names
   it and declares its properties (formal order, TVD, positivity-preserving, well-balanced,
   divergence-free). `reference` maps properties to checks; divergence-free maps to ADR-0007's
   ∇·B invariant. The test that instantiates every combination (ADR-0037) looks up each
   combination's row and fails if it has none. So an implementer cannot pick the checks, and
   cannot change what a combination claims without a change to `reference`, which the
   maintainer approves. A combination that claims too little, such as PLM declared first order,
   is caught there, at review.
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
   - 1D embedded in 3D, on a Cartesian grid uniform in y and z. It holds only if both runs use
     the same Δt (the 3D CFL condition also sees the transverse directions, so the test fixes
     Δt) and the Cartesian update is written per direction as (Δt/Δxₐ) times the flux
     difference, so that it reduces exactly to the 1D form. This covers cell-centred schemes.
     With constrained transport (M5), By and Bz come from corner electric fields that average
     face fields, and that average does not round to the 1D field;
   - rotation along x, y and z, by a cyclic permutation of the axes. It holds for cell-centred
     schemes only if the face frame orders the transverse components cyclically (for a y face:
     z, then x) and cell-level norms such as |v|² and |B|² add their squared components in an
     order that no permutation changes, such as smallest first;
   - constrained-transport combinations are left out of these two tests until M5's plan shows
     how they hold. Writing the corner formula as the 1D field plus terms that are exactly zero
     needs data uniform in y and z, and a Riemann solver whose electric field for equal left and
     right states is bitwise the cell-centred one, which HLL's is not. Rotation also needs the
     corner sums written in cyclic axis order. ADR-0007 asks for the rotation test to be bitwise,
     so M5's plan either makes it hold for constrained transport or proposes an amendment;
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
- The table of combinations and their properties, and the mapping from properties to checks,
  live in `reference`, so a solver PR cannot change which checks run without a protected change.
  The combination test fails for a combination that has no row (planned with M0, ADR-0037).
- Tidy: the planned `tier-tag` rule. A planned rule also rejects a float literal used as a
  tolerance in a test outside `thresholds.rs`.
- The guardrails plan's task `thresholds-file` changes its target from `thresholds.toml` to
  `thresholds.rs`, and `TIGER_STYLE.md`'s rule on magic tolerances changes with it. Both are
  updated in the PR that writes the M0 plan.
- ADR-0009's post-merge job runs the full matrix and the mutation tests.

## Consequences

- Wiring a new combination costs one line, and its checks follow from what it claims.
- Claiming a property costs the checks that come with it. A combination that claims less is
  checked less, so the maintainer's review of `reference` watches the declared properties. No
  mechanism derives them from the component types yet.
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
