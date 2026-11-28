# ADR-0038: Mesh, blocks and the time loop

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer

## Context

ADR-0035 asks for one block-based data model from M0 to AMR, and ADR-0003 allows no allocation
after init. AMR creates and destroys blocks while a run is going. Athena++'s mesh tree is
recursive, as far as we know, and TigerStyle bans recursion (ADR-0006).
ADR-0037 fixes the extension points, and this ADR fixes the structure they run on.

## Decision

1. **Every block has the same size,** with ghost cells. Refinement splits a block into 8 children
   of the same size. A block holds its cell-centred state in the layout of
   `docs/ARCHITECTURE.md` § 4(a), and for MHD its face-centred field and edge electric fields.
   Curvilinear and non-uniform blocks store their geometry, computed at init.
2. **The block tree is a linear octree:** a sorted array of Morton keys (level plus logical
   coordinates, typed newtypes). Neighbours are found by key arithmetic and binary search, with
   no recursion. With 2:1 balance, neighbour tables have a fixed size of at most 56 slots in 3D.
   The Morton order is the space-filling curve for load balancing.
3. **Pools with a fixed capacity.** The configuration sets a memory budget, and init allocates
   every pool once: blocks, neighbour and flux-correction buffers, and scratch space per thread.
   Refinement and coarsening take and return pool slots. Running out of slots is an operating
   error: the run stops cleanly, writes a checkpoint and reports the memory it needed.
4. **Init** validates the configuration, builds the initial tree up to a configured maximum level,
   allocates, sets the initial conditions and then sets `alloc-guard`'s flag.
5. **A step runs a fixed list of phases per stage of the integrator, over all blocks:**
   1. ghost cells and their exchange;
   2. fluxes per direction;
   3. edge electric fields;
   4. update;
   5. source terms;
   6. positivity correction;
   7. flux and field correction between levels.

   The time step is a minimum over blocks. AMR regrids every N steps.
6. **Bulk-synchronous first.** Communication overlaps with computation from M7 on, without
   changes to kernels. Results never depend on the order of blocks.
7. **A `mesh` crate** holds the tree, the neighbour tables and the pools. `kernels` never depends
   on it. An unstructured mesh will be a second mesh kind that shares `kernels`.

## Enforcement

- `alloc-guard`: the planned `alloc-after-init` test covers refinement and coarsening steps once
  AMR exists.
- A test over the crate graph (`xtask/tests/crate_graph.rs`, added by the M0 scaffold) checks that
  `kernels` depends only on `base` and `libm`. The compiler does not: a `no_std` crate can depend
  on a `std` crate.
- Tidy (ADR-0006) and `nami_recursion` (dylint): no recursion in the tree code.
- Determinism (ADR-0005): the planned `determinism-hash` job compares runs, and a test that
  processes blocks in a shuffled order and compares bits is planned with M4.
- Review: a design that changes the block size per block, or allocates in the time loop, needs a
  new ADR.

## Consequences

- AMR never allocates. The price is that the memory budget has to be configured, and a run can
  stop early when it runs out.
- Equal-sized blocks make load balancing a matter of counting blocks along the curve. They waste
  some cells where refinement is uneven.
- The neighbour tables cost a fixed 56 slots per block, even on a uniform grid.
- Bulk-synchronous phases are simpler to verify. They leave speed on the table at many nodes
  until M7 adds the overlap.

## Alternatives considered

- **A pointer-based tree, as in Athena++.** Rejected. It needs recursion or an explicit stack,
  and it is harder to keep deterministic and allocation-free.
- **Blocks of different sizes** (patch-based AMR, as in AMReX). Rejected. Load balancing and
  scratch sizing get harder, and the octree loses its simple structure.
- **Allocating blocks on refinement.** Rejected by ADR-0003.
- **A dependency-driven task list from the start.** Deferred to M7. Nothing before multi-node runs
  needs it.

## Evidence

- Athena++ (Stone et al. 2020, ApJS 249, 4): blocks of one size, an octree of logical locations,
  2:1 balance, a task list per stage. That its tree code is recursive is background knowledge.
  Check the source before citing it.
- Linear octrees with Morton keys: Sundar, Sampath & Biros 2008, SIAM J. Sci. Comput. 30, 2675
  (Dendro). p4est (Burstedde, Wilcox & Ghattas 2011, SIAM J. Sci. Comput. 33, 1103).
- The maintainer's answers in the architecture discussion of 2026-11-26.
