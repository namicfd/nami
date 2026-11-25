# ADR-0024: Numerical methods

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer

## Context

ADR-0022 asks nami to be at least as fast as Athena++/AthenaK and, eventually, to exceed AthenaK's
feature set. A speed comparison means something only between the same algorithms. TigerStyle bans
silent floors, which is how Athena++ keeps density and pressure positive. ADR-0023 fixes the
equation systems and the milestones. This ADR fixes the numerical methods.

## Decision

1. **The standard methods, with Athena++'s menu as the baseline** (yardstick, not template):
   - method of lines, reconstructing, solving the Riemann problem and updating once per stage;
   - VL2, SSP-RK2 and SSP-RK3;
   - donor cell, PLM (minmod, van Leer, MC) and PPM, on primitive variables by default and on
     characteristic variables as an option;
   - LLF, HLLE, HLLC, HLLD, and exact solvers for advection and Burgers;
   - Gardiner–Stone upwind constrained transport;
   - unsplit source terms, hydrostatic reconstruction for shallow water;
   - explicit diffusion;
   - a CFL time step from the largest signal speed;
   - periodic, outflow, reflecting, user-inflow and polar boundaries.

   `docs/ARCHITECTURE.md` § 3.1 lists the methods that come later.
2. **Positivity comes from named, counted scheme components, never silent floors:**
   - first-order flux correction;
   - a Zhang–Shu positivity limiter for higher-order schemes;
   - wave-speed estimates that are guaranteed upper bounds (Guermond & Popov 2016);
   - floors only as a last resort, configured explicitly and counted.

   An unexpected count fails a test.
3. **Firm goals beyond the baseline:**
   - every combination the configuration can select is verified at every reference tier
     (ADR-0007);
   - well-balanced gravity for hydrostatic equilibria in Euler and MHD.
4. **The interfaces must allow these now:**
   - per-level time steps (subcycling) under AMR;
   - WENO-Z and fourth-order finite volume: the ghost width comes from the reconstruction as a
     compile-time constant, and cell averages and point values are distinct types.

## Enforcement

- References (ADR-0007) per method. A combination the configuration can select without a
  passing reference is a bug. A test that lists every selectable combination is planned with the
  first configuration code.
- Floor and limiter activation counts: `[planned:floor-activation-count]` in TIGER_STYLE.md,
  task `floor-activation-count`.
- Wave-speed bounds are asserted (TIGER_STYLE.md, Physical admissibility).
- Everything else: review against this ADR.

## Consequences

- Speed comparisons with Athena++ use the same methods on both sides.
- Proving positivity costs some speed: the flux-correction pass and the limiter. It is reported
  per step, so its cost is visible.
- Verifying every selectable combination makes the test matrix grow with the menu. That pushes
  the menu to stay curated, which is intended.
- Leaving room for subcycling and fourth order makes the M0 to M5 interfaces somewhat more
  general than those milestones need.

## Alternatives considered

- **Copy Athena++ exactly, floors included.** Rejected. Silent floors break TigerStyle, and the
  aim is to be better.
- **Positivity-preserving schemes only, no floors at all.** Rejected for now. Extreme problems
  (strong blast waves into near-vacuum) may still need a last resort, so it exists, but every
  activation is counted.
- **Commit to subcycling and fourth order now.** Deferred. They are expensive, and their value
  shows only with AMR and smooth flows.

## Evidence

- Athena++: Stone et al. 2020, ApJS 249, 4. AthenaK's first-order flux correction: Stone et al.
  2024, arXiv:2409.16053.
- Gardiner & Stone 2005, 2008 (CT). Felker & Stone 2018 (fourth-order finite volume). Zhang & Shu
  2010 (positivity limiter). Guermond & Popov 2016 (guaranteed wave-speed bounds). Audusse et al.
  2004 (hydrostatic reconstruction). Meyer, Balsara & Aslam 2014 (RKL2).
- That Athena++ and AthenaK use one global Δt, apply floors silently, test only a sample of
  combinations and have no general well-balancing is background knowledge. Verify each before
  claiming it as a difference.
- The maintainer's answers in the architecture discussion of 2026-11-26.
