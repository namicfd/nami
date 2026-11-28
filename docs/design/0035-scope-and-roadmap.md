# ADR-0035: Scope and roadmap

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Summary: One block-based, coordinate-aware data model from M0. Compile-time extension points.
  Advection, Burgers, Euler, shallow water, then MHD, curvilinear, multiple nodes, SMR, AMR.

## Context

ADR-0034 sets the end state: 3D classical MHD on static and adaptive grids, flexible enough to
exceed AthenaK's feature set eventually, with Voronoi meshes as a stretch goal. Two risks follow
from getting from here to there:

- The first slices could quietly fix a data model that the later ones have to rewrite. The closed
  1D advection plan (PR #4) would have done that.
- Interfaces designed from a single example tend to break on the second one.

## Decision

1. **One data model from the first slice.** 1D is 3D with one cell in y and z. The mesh is made
   of blocks, and a uniform grid is a single block. The update uses face areas and cell volumes
   with a geometric source term, and Cartesian coordinates are the special case. No milestone
   rewrites this model.
2. **Flexibility comes from a small, fixed set of extension points** that are resolved at compile
   time:
   - equation system and equation of state;
   - Riemann solver and reconstruction;
   - time integrator;
   - coordinates;
   - source terms;
   - boundary conditions;
   - refinement criteria;
   - outputs.

   The run configuration picks one compiled combination at init. Adding an extension point needs
   an ADR. ADR-0037 designs the interfaces.
3. **Equation systems:** linear advection, Burgers, Euler, shallow water (with bathymetry,
   well-balanced, wet/dry deferred), ideal MHD. Burgers and shallow water are there to test the
   abstractions before MHD depends on them.
4. **Physics:**
   - core: adiabatic and isothermal equations of state, passive scalars, static gravity and user
     source terms;
   - next: resistivity, viscosity and conduction;
   - deferred: self-gravity, the shearing box and cooling;
   - deferred, but not ruled out by the architecture: relativity, radiation and particles.
5. **Coordinates:** Cartesian, cylindrical and spherical.
6. **Milestones,** each a vertical slice with references first:
   - M0: 1D advection, the walking skeleton;
   - M1: 1D Burgers;
   - M2: 1D Euler;
   - M3: 1D shallow water, then a review of the equation-system interface;
   - M4: Euler and shallow water in 2D/3D, with many blocks and threads;
   - M5: ideal MHD with constrained transport;
   - M6: curvilinear coordinates;
   - M7: multiple nodes;
   - M8: static mesh refinement;
   - M9: adaptive mesh refinement;
   - M10, stretch: unstructured and moving Voronoi meshes, after their own design pass.

   `docs/ARCHITECTURE.md` § 2 gives what each one adds and why they come in this order.

## Enforcement

- Each milestone becomes a plan in `docs/exec-plans/` (ADR-0011). Its reference tasks come first
  (ADR-0007).
- The M3 interface review is a task in the M3 plan. Its output is an ADR, which the maintainer
  accepts or rejects.
- No mechanical check that the data model is not rewritten. That is for review: a plan that
  changes the data model needs an ADR.

## Consequences

- M0 is heavier than a plain 1D code would be. Its kernels take areas and volumes, and its mesh
  has blocks. The current `update_pencil` template (Δt/Δx) is Cartesian-only and gets replaced
  in M0.
- Four cheap systems put the pipeline (tasks, claims, review, references) through several full
  runs before the expensive work starts.
- Every compiled combination of extension points costs compile time and binary size. Only the
  combinations that the configuration can select are compiled.

## Alternatives considered

- **Drop Burgers and shallow water and go straight to MHD.** Rejected. It is cheaper in the short
  term, but the equation-system interface would be designed from advection and Euler alone.
- **A 1D-only code first, generalised later.** Rejected. That is the rewrite this ADR exists to
  avoid.
- **Runtime plug-ins (`dyn` extension points).** Rejected. They conflict with ADR-0003 (no `dyn`
  in kernels) and cost speed in hot loops.
- **Cartesian only, like AthenaK.** Rejected. The maintainer wants curvilinear coordinates, and
  adding them after mesh refinement would mean rewriting refinement.

## Evidence

- Athena++ (Stone et al. 2020, ApJS 249, 4): 1D as 3D with one cell per inactive dimension, mesh
  blocks as the unit of refinement and of parallelism, curvilinear coordinates with AMR.
- That AthenaK supports Cartesian grids only is background knowledge (Stone et al. 2024,
  arXiv:2409.16053). Verify it before claiming it as a difference.
- The maintainer's answers in the architecture discussion of 2026-11-26.
