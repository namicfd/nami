# nami architecture

> Status: **being written, one topic at a time,** in a discussion between the maintainer and an
> agent. Each section says whether it is agreed, proposed or open. The decisions themselves are
> ADRs in `docs/design/`, status Proposed until the maintainer accepts them. Where this file and an
> ADR disagree, the ADR wins.

| § | Topic | State | ADR |
|---|---|---|---|
| 1 | Purpose, users and success criteria | Agreed | [0022](design/0022-purpose-users-and-success-criteria.md) |
| 2 | Scope and roadmap | Agreed | [0023](design/0023-scope-and-roadmap.md) |
| 3 | Numerics | Agreed, except § 3.3 (proposed) | [0024](design/0024-numerics.md) |
| 4 | Core architecture | (a) agreed as a hypothesis, waiting for the prototype; (b), (c) agreed | [0025](design/0025-extension-points-and-composition.md), [0026](design/0026-mesh-blocks-and-time-loop.md) |
| 5 | Performance and parallelism | Agreed (problem sizes open until M7) | [0027](design/0027-parallelism-and-communication.md), [0028](design/0028-infiniband-binding-crate.md) |
| 6 | Interface, output and determinism guarantees | Agreed | [0029](design/0029-interface-output-and-guarantees.md) |
| 7 | Verification strategy | Open | |

## 1. Purpose, users and success criteria

**nami is a production solver** for compressible magnetohydrodynamics and its sub-systems. It has
four goals:

- **Safety:** results you can trust. Rust's type system, TigerStyle assertions, bitwise
  determinism (ADR-0005) and references anchored outside the agent swarm (ADR-0007) make it so.
- **Speed:** at least as fast as Athena++ and AthenaK on the same problem and hardware.
- **Flexibility:** future features fit in naturally, through named extension points. The aim
  is to exceed AthenaK's feature set eventually.
- **A swarm-first demonstration:** agents write the code, and humans own the references, the
  thresholds and the ADRs. The project shows how far that approach goes.

**Users** are researchers, and agents working for them, who apply the code to study fluid
phenomena. The results are for the maintainer and a few former colleagues. There is no
public release and no promise of a stable interface yet.

**End state:** 3D classical MHD on static and adaptively refined grids. A stretch goal is
unstructured and moving grids, such as a moving Voronoi mesh. The feature set draws heavily on
existing codes, Athena++ and AthenaK first.

**Hardware.** nami is developed on a workstation, and production runs use several nodes
connected by InfiniBand. It targets CPUs only. GPUs are out of scope until a later ADR brings them
in.

**nami builds its own infrastructure.** The only runtime dependency is `libm` (ADR-0001), so there
is no MPI and no HDF5. That is deliberate: nami designs its own communication layer and output
format and tunes them to its needs. Reaching InfiniBand and similar fabrics takes a C system
library (libibverbs, libfabric or UCX). A small binding crate, with its `unsafe` confined the way
`alloc-guard`'s is (ADR-0014), is acceptable for that. § 5 decides its shape in its own ADR.

### Failure

The project has failed if:

- **humans can no longer read the code;** or
- **its performance is significantly worse than Athena++/AthenaK.** The aim is to be faster.

### Success criteria

1. **Performance.** The measure is zone-cycles per second: cell updates per second, per core and
   per node. It is measured on the same CPUs and the same problem as Athena++ and AthenaK's
   CPU backend, with the same algorithm (for example PLM, HLLD, VL2 and constrained transport),
   each code built as its documentation recommends. The target is at least 1.0× per core and per
   node, and below 0.8× counts as failure. The scaling targets come with § 5.
2. **Correctness.** Every capability claim, such as "3D MHD on AMR works", is backed by
   references at every tier of ADR-0007. That includes the problems and published thresholds of
   Athena++'s regression suite, where they exist.
3. **Readability, with no human in the loop.** At each milestone, an agent session with no
   context reads a sampled module **without** its plan, and writes down what the module computes:
   its equations, its inputs, its assumptions. A second session compares that reconstruction with
   the plan. Each mismatch, and each place where the reader had to guess, becomes a finding, and
   each finding becomes a guardrail (ADR-0020). An agent reading agent-written code is a weaker
   test than a human reader. The maintainer reads code when they choose to, but nothing waits for
   it. TigerStyle's mechanical limits (function length, line length, naming, assertions) also
   apply.
4. **The swarm demonstration is recorded.** For each merged PR we record the cost, the
   maintainer's minutes, whether a human changed the code, and every defect that escaped and was
   found later.

### Tensions that later topics must resolve

- **Fast interconnects (§ 5).** With `std` alone, nami reaches other nodes only over TCP, whose
  latency is about ten times higher than RDMA's. The binding crate for InfiniBand amends
  ADR-0001 (it links a system library) and ADR-0014 (a second place with `unsafe`). § 5 writes
  that ADR, and also decides how the layer works over TCP and shared memory on the workstation.
- **Determinism costs speed.** ADR-0005 estimates 5–15% per step, and nami pays it against the
  performance target. In return, nami can offer what Athena++ does not: results that are
  bitwise the same for any decomposition and thread count, if § 5 and § 6 choose to.
- **No allocation after init (ADR-0003) against AMR and moving meshes (§ 4).** Blocks and cells
  must come from fixed-capacity pools, sized at init from a configured memory budget.
- **Pencils against unstructured grids (§ 4).** ADR-0003's pencils cover structured blocks. A
  Voronoi mesh needs typed gathers as a first-class path, not as an exception.
- **Agents are users (§ 6).** Configuration, errors and output must be usable by an agent:
  explicit configuration, terse greppable errors, machine-readable diagnostics.

## 2. Scope and roadmap

### One data model from the first slice

1D is 3D with one cell in y and z, as in Athena++. The mesh is made of blocks from day one, and a
uniform grid is a single block. Coordinates are part of the model from the start: the update
uses face areas and cell volumes,

  ū ← ū − (Δt/V) (A_R F_R − A_L F_L) + Δt S_geom,

with non-uniform spacing allowed. Cartesian coordinates are the special case with constant areas
and volumes and no geometric source. So no milestone rewrites the data model, and every slice,
the first one included, exercises the real architecture.

### Flexibility through named extension points

Flexibility (§ 1) comes from a small, fixed set of extension points, not from a general framework.
They are:

- equation system and equation of state;
- Riemann solver and reconstruction;
- time integrator;
- coordinates;
- source terms;
- boundary conditions;
- refinement criteria;
- outputs.

Each is resolved at compile time, so hot loops have no `dyn` and no indirection (ADR-0003). The
run configuration picks one compiled combination once, at init. § 4 designs the interfaces. Two
risks: abstraction layers that make the code hard to read (§ 1, Failure), and compile time or
code size growing with the number of combinations. Keeping the set of extension points small and
fixed guards against both.

### Equation systems and physics

- **Equation systems, in order:** linear advection, Burgers, Euler, shallow water, ideal MHD.
  - Burgers and shallow water are not on the path to MHD. They exist to get the abstractions
    right, because an interface designed from one example is a guess.
  - The four systems before MHD differ in their number of variables (1, 1, 3, 2), in
    nonlinearity, and in having source terms.
  - Shallow water includes bathymetry and is well-balanced (lake at rest), which tests the
    source-term interface the way a hydrostatic atmosphere will in Euler and MHD. Wet/dry fronts
    are deferred.
- **Core physics:** adiabatic and isothermal equations of state, passive scalars, and static
  gravity and user-defined source terms.
- **Next:** resistivity, viscosity and thermal conduction, explicit first and with
  super-time-stepping later.
- **Deferred:** self-gravity, the shearing box and cooling.
- **Deferred, but the architecture must not rule them out:** relativity, radiation and particles.
  The aim to exceed AthenaK (§ 1) includes them eventually.
- **Coordinates:** Cartesian, cylindrical and spherical.

### Milestones

Each milestone is a complete vertical slice, with references first (ADR-0007).

| M | Slice | Grid | What it adds |
|---|---|---|---|
| 0 | Linear advection: the walking skeleton | 1D, one block, Cartesian | The pipeline end to end: references, kernels, time loop, boundaries, output. It also exercises the task, claim and review process. |
| 1 | Burgers | 1D | Nonlinearity: a Riemann solution that is physical at a transonic rarefaction, shocks with known speeds |
| 2 | Euler | 1D | A system: equation of state, HLLE and HLLC, Toro's tests 1–5, Sod, Shu–Osher |
| 3 | Shallow water | 1D | Source terms and well-balancing. **Then a review of the equation-system interface** across all four systems, before it is frozen. |
| 4 | Euler and shallow water | 2D/3D, many blocks, threads | Unsplit multi-dimensional integration, block boundary exchange, the first performance comparison with Athena++ |
| 5 | Ideal MHD | 1D → 3D, constrained transport | HLLD, ∇·B = 0, Brio–Wu, linear waves, Orszag–Tang, field-loop advection |
| 6 | Curvilinear coordinates | cylindrical, spherical | Geometric source terms, CT on curvilinear grids |
| 7 | Multiple nodes | uniform grid | The communication layer: shared memory and TCP, then InfiniBand |
| 8 | Static mesh refinement | | Prolongation and restriction, flux correction, CT across refinement levels |
| 9 | Adaptive mesh refinement | | Refinement criteria, block pools, load balancing |
| 10 | Unstructured and moving (Voronoi) mesh, stretch goal | | Its own design pass first |

- Threads come at M4, because they decide the data layout.
- Curvilinear coordinates come before mesh refinement, so that prolongation, restriction and
  flux correction are written for every coordinate system from the start.
- Multiple nodes come before mesh refinement, because load balancing under AMR depends on the
  communication layer.

## 3. Numerics

### 3.1 Baseline: the standard methods, with Athena++ as the yardstick

nami implements the standard methods of the field. Athena++'s menu is the baseline, so that "the
same algorithm" in the performance criterion (§ 1) means the same thing in both codes. It is a
yardstick, not a design to copy (§ 3.3).

| Part | Baseline | Later (the interfaces allow it now) |
|---|---|---|
| Scheme | Method of lines: reconstruct, solve the Riemann problem, update, once per stage | |
| Time integrators | VL2 (van Leer predictor-corrector), SSP-RK2, SSP-RK3 | Higher-order and low-storage RK |
| Reconstruction | Donor cell; PLM with minmod, van Leer and MC limiters; PPM. Primitive variables by default, characteristic as an option. | WENO-Z; fourth-order finite volume (Felker & Stone 2018) |
| Riemann solvers | LLF and HLLE (every system); HLLC (Euler, shallow water); HLLD (MHD); exact for advection and Burgers | Roe |
| Constrained transport | Gardiner–Stone upwind CT, with face-centred magnetic field | |
| Source terms | Added unsplit in every stage; hydrostatic reconstruction for shallow water (Audusse et al. 2004) | |
| Diffusion | Explicit | RKL2 super-time-stepping (Meyer, Balsara & Aslam 2014) |
| Time step | CFL number times the largest signal speed, as a global minimum, which gives the same result in any order | Per-level steps (§ 3.3) |
| Boundaries | Periodic, outflow, reflecting, user inflow; polar boundaries with M6 | Shearing box |

### 3.2 Positivity without silent floors

Athena++ keeps density and pressure positive with fixed floors that it applies silently. nami may
not (TIGER_STYLE.md, Physical admissibility). It uses named scheme components instead:

1. **First-order flux correction**, as in AthenaK. Where a high-order update would make density
   or pressure non-physical, the affected faces fall back to the first-order flux. The update
   stays conservative and deterministic.
2. **A Zhang–Shu positivity limiter** for the higher-order schemes, used together with wave-speed
   estimates that are guaranteed upper bounds (Guermond & Popov 2016).
3. **Floors only as a last resort.** They are configured explicitly and every activation is
   counted per step. An unexpected count fails a test.

Each mechanism counts its activations and reports them per step.

### 3.3 Better than the baseline (proposed)

**Firm goals:**

- **Every selectable combination is verified.** Each combination of equation system,
  reconstruction, Riemann solver, integrator and coordinates that the configuration can select
  passes the reference tiers of ADR-0007. With compile-time extension points (§ 2), the selectable
  set and the tested set are the same set.
- **Provable positivity** (§ 3.2).
- **Well-balanced gravity** for hydrostatic equilibria in Euler and MHD, extending the shallow-water
  well-balancing of M3.

**The interfaces must allow these now; they are built later:**

- **Subcycling in time per refinement level** (M9). Each level takes its own Δt, and fluxes and
  electric fields are corrected between levels. The time-integration and flux-correction
  interfaces must not assume one global Δt.
- **Higher order:** WENO-Z and fourth-order finite volume. The ghost-cell width comes from the
  reconstruction, as a compile-time constant. Cell averages and point values are distinct types,
  so a fourth-order scheme cannot mix them up.

### 3.4 What this means for § 4

- Reconstruction works on primitive variables, so converting between conserved and primitive
  variables is part of the equation-system interface. For relativity, that conversion needs a
  root finder with a named iteration limit and an assertion that it converged (TIGER_STYLE.md).
- VL2 with constrained transport needs fluxes and electric fields on edges. That goes beyond
  ADR-0003's pencils. It is the CT exception that ADR-0003 already foresees, and it needs its
  ADR in § 4.
- Positivity mechanisms work per face and per cell, and count their activations. They need an
  output channel for the counts that does not allocate.

## 4. Core architecture

The topic has three parts:

- (a) data layout and kernel shape;
- (b) the extension-point interfaces and how they compose;
- (c) the mesh and blocks, and what happens at init versus in the time loop.

### 4(a) Data layout and kernel shape (agreed as a hypothesis, waiting for the prototype)

- **Structure of arrays.** A block stores one array per variable, laid out `[var][k][j][i]` with
  i fastest, as Athena++ and AthenaK do.
- **Kernels work on row stencils.** The loop over i vectorises in every sweep direction. A sweep
  in y reconstructs from the rows at j−1, j and j+1 (and further for wider stencils), each
  contiguous in i, so nothing is transposed or copied into pencils. Vectorising across i does not
  reorder any floating-point operation, so it is compatible with ADR-0005.
- **Per-face physics over named-field state structs,** for example
  `EulerPrimitive { density, velocity: [f64; 3], pressure }`. Riemann solvers and equations of
  state are written for one face and called in a loop over i that loads each field from its own
  row.
  - On stable Rust, an associated constant cannot be an array length in generic code, so state
    types are associated types in any case.
  - Named fields also read better than `[f64; NVAR]` with constant indices (§ 1, readability).
- **This amends ADR-0003:**
  - pencils become row stencils;
  - `[f64; NVAR]` becomes named state structs;
  - typed gathers stay;
  - raw indexing stays banned.

  The ADR is written once the prototype confirms the layout.
- **The prototype** measures zone-cycles per second for 3D Euler (PLM, HLLC, RK2, single core,
  64³ and 128³). It compares structure of arrays with row stencils, array of structures, and a
  hybrid, against Athena++ built on the same workstation. The code stays on a branch, and the
  results go to `docs/research/`.

### 4(b) Extension points and how they compose (agreed)

**A run is a compiled combination.** A user writes a small **problem crate** that depends on nami.
It provides the initial conditions, user boundary conditions and source terms, and names its
combination of extension points in Rust types. The runtime configuration sets only parameters
(resolution, CFL number, γ, end time, outputs), and init validates them and returns a `Result`.

- The speed comes from monomorphisation: the Riemann solver, the equation of state and the
  reconstruction are inlined into the row loop, constants fold, and the loop vectorises.
  Selecting among already-compiled combinations once at init would be just as fast. Compile-time
  selection wins in two other ways:
  - build cost: each build compiles only what it uses;
  - more constants: block size, the number of passive scalars and the ghost width can be
    compile-time constants, and loops of known length can be faster. Whether block size should
    be one is for the prototype to measure.
- A problem crate may compile several combinations and select one at init.
- nami's own test suite instantiates every supported combination, which is how "every selectable
  combination is verified" (§ 3.3) is achieved.

**The extension points:**

- **Equation system.** A trait with associated types `Conserved` and `Primitive` (named-field
  structs, § 4(a)) and `Parameters`, such as γ, always spelled out with no `Default`. It provides:
  - conversion between conserved and primitive variables;
  - the physical flux along a face normal;
  - the largest signal speed, a guaranteed upper bound that is asserted.
- **Wave-speed bounds.** A small trait, implemented per system: the smallest and largest signal
  speeds of the Riemann fan, given the left and right states. These are guaranteed bounds
  (§ 3.2).
- **The face frame.** Riemann solvers only see states rotated into the face's frame, normal
  component first.
  - On structured grids the rotation is an exact permutation: it decides which row is loaded as
    the normal velocity, and costs nothing.
  - Unstructured grids add a general rotation later. Every Riemann solver works there unchanged,
    but those results are not bitwise rotation-invariant.
  - For MHD, the face fills the normal magnetic field from the face-centred field; it is not
    reconstructed.
- **Riemann solvers.** A trait `RiemannSolver<E>`.
  - LLF and HLLE are implemented once, for every system with wave-speed bounds.
  - HLLC (Euler, shallow water) and HLLD (MHD) are implemented per system.
  - The compiler rejects a combination that does not exist.
- **Reconstruction.** Works per variable over rows, so it does not depend on the equation system.
  Characteristic reconstruction asks the system for its eigenvectors through a separate trait.
  The ghost width is a constant of the reconstruction type.
- **Time integrators as data.** Each integrator is a constant table of stages in the low-storage
  3S* form (Ketcheson 2010): a few coefficients per stage, two or three registers allocated at
  init, and the reconstruction order of each stage. One generic driver runs the table. The form
  covers forward Euler, SSP-RK2 and SSP-RK3, RK4, SSP-RK(5,4) and VL2, whose first stage is donor
  cell with half a step. Implicit-explicit methods and super-time-stepping get their own drivers
  when they come.
- **Coordinates.** A trait for face areas, cell volumes and geometric source terms. The Cartesian
  version returns constants, which fold away. The curvilinear versions read arrays precomputed
  per block at init.
- **Source terms.** A trait, composed statically as a tuple, for example
  `(Gravity, PassiveScalars)`, and applied in a fixed order every stage.
- **Boundary conditions.** Chosen per block face at init, and dispatched once per face per stage
  with a `match`, never per cell. User boundary conditions come from the problem crate.

### 4(c) Mesh, blocks, and init versus the time loop (agreed)

**Blocks.**

- Every block has the same size: nx1×nx2×nx3 cells plus ghost cells. That gives one scratch size,
  equal work per block, and refinement that splits a block into 8 children of the same size.
  Block size may become a compile-time constant (§ 4(b)) if the prototype shows that it helps.
- A block's state is its slice of a pool, in the layout of § 4(a). For constrained transport, a
  block also holds the face-centred magnetic field and the edge electric fields, each with one
  extra cell in its staggered direction.
- A uniform Cartesian block stores only its origin and spacing. Curvilinear and non-uniform blocks
  store their face positions, areas and volumes, computed at init.

**The block tree is a linear octree.**

- It is a sorted array of Morton keys. Each key is a level plus logical coordinates, as typed
  newtypes (ADR-0002).
- Finding a neighbour is arithmetic on the key followed by a binary search: iterative, with fixed
  bounds, and no recursion (ADR-0006).
- The Morton order is also the space-filling curve that load balancing partitions, so the
  partition is deterministic.
- With 2:1 balance between levels, a 3D block has at most 56 neighbour slots: up to 4 per face,
  2 per edge and 1 per corner. Every neighbour table is a fixed-size array.

**Pools with a fixed capacity.**

- The configuration sets a memory budget. Init turns it into a maximum number of blocks per rank,
  and allocates every pool once: block storage, neighbour buffers, flux-correction buffers, and
  scratch space for each thread.
- Refining takes slots from a fixed free list, and coarsening returns them.
- Running out of slots is an operating error. The run stops cleanly, writes a checkpoint, and
  reports the memory it would have needed. It never allocates (ADR-0003).

**Init.** `Solver::init(config, problem) -> Result<Solver, InitError>`:

1. validates the configuration;
2. builds the initial tree, refining iteratively up to a configured maximum level;
3. allocates every pool;
4. calls the problem's initial conditions;
5. sets `alloc-guard`'s flag that init is done.

**One step** runs the integrator's table of stages (§ 4(b)). Every stage runs a fixed list of
phases over all blocks:

1. fill ghost cells, and exchange them between blocks and ranks;
2. compute the fluxes for each direction with the row kernels;
3. compute the edge electric fields (constrained transport);
4. update;
5. add the source terms;
6. apply the positivity correction (§ 3.2);
7. correct fluxes and electric fields between refinement levels.

After the last stage, the next time step is the minimum over all blocks, which does not depend
on order. AMR regrids every N steps: tag blocks, refine and coarsen within the pool, rebalance
(§ 5).

**Bulk-synchronous first.** Each phase finishes on every block before the next one starts. The
overlap of communication with computation (interiors first, then the cells next to boundaries)
comes in M7, without changing any kernel. Block order never changes results: a block's update
depends only on its own data and its ghost cells, and reductions combine in block-key order.

**Crates.**

- A new `mesh` crate holds the tree, the neighbour tables and the pools.
- `kernels` never sees the mesh, so the face-frame physics of § 4(b) can be reused on unstructured
  meshes later.
- The manifest change lands in M0 as its own protected commit.
- An unstructured or Voronoi mesh (M10) will be a second mesh kind with its own design pass. It
  shares `kernels`, not `mesh`.

## 5. Performance and parallelism

The hardware is fixed in § 1: CPUs only, a workstation for development, and production on several
nodes connected by InfiniBand, scheduled by Slurm.

### 5.1 Within a core: auto-vectorisation only

Explicit SIMD needs `std::arch` intrinsics, which are `unsafe`, or `std::simd`, which is
nightly-only. So the row kernels of § 4(a) must vectorise on their own, which the layout prototype
measures. A loop that does not vectorise is restructured, never rewritten with intrinsics.

### 5.2 Within a node: one process per NUMA domain, with threads

- Threads are created once at init with `std::thread::scope` and live for the whole run.
- Blocks are assigned to threads statically, in contiguous ranges along the Morton curve. There is
  no work stealing, so the schedule is the same every run.
- Phases are synchronised with `std::sync::Barrier`.
- Each thread writes its own blocks first at init, so their memory pages land on its NUMA node.
- Threads share ghost cells within a process without passing messages.

### 5.3 Between nodes: nami's own communication layer

- **A `Transport` trait** has three implementations:
  - in-process: several ranks in one process, for deterministic tests on the workstation;
  - TCP, over `std::net`;
  - InfiniBand verbs, through the binding crate of ADR-0028.
- **No new message pattern after init.** Every pair of neighbouring ranks gets a persistent channel
  with preallocated buffers, built at init and rebuilt only when AMR rebalances.
- **Collectives:** a barrier, a minimum (for the time step), sums for diagnostics, and an all-gather
  of tree changes under AMR.
- **Launching:** Slurm's `srun` starts the processes, and each reads its rank from `SLURM_PROCID`
  and the number of ranks from `SLURM_NTASKS`. Rank 0 runs a rendezvous over TCP, where the ranks
  exchange their addresses, including their InfiniBand queue-pair information. nami has no
  launcher of its own.
- **A failed link is an operating error.** The run stops and writes a checkpoint if it can.

### 5.4 Bitwise-identical results for any decomposition

One thread or a thousand ranks, any placement of blocks: the result is bitwise the same. This
extends ADR-0005.

- A block's update depends only on its own data and its ghost cells, and the ghost cells hold the
  same values whatever the layout.
- The time step is a minimum, which does not depend on order.
- Diagnostic sums, such as conservation totals, use an exact accumulator (an integer
  superaccumulator) whose result does not depend on order. Across ranks it replaces
  `sum_compensated`. It is not in the hot path.
- Refinement decisions depend only on block data. Load balancing changes where blocks live, not
  what they compute.

So every scaling run is also a correctness test.

### 5.5 Targets and measurement

- **Per core and per node:** at least 1.0× Athena++ on the same CPUs and the same algorithm, and
  below 0.8× counts as failure (§ 1).
- **Scaling, first target:** at least 90% weak-scaling efficiency from 1 to 64 nodes. Problem sizes
  and strong-scaling targets are set with M7.
- **Measurement:**
  - Gungraun instruction counts in CI catch regressions;
  - zone-cycles per second against Athena++ on the workstation at every milestone, not in CI,
    because shared runners are too noisy;
  - scaling runs on the cluster from M7 on.
- **Widths:** Morton keys and global cell numbers are `u64`, since 21 levels in 3D take 63 bits.
  ADR-0002's limit of 2³² − 1 cells per rank stays.

## 6. Interface, output and determinism guarantees

nami follows no outside standard for its interface or files. It optimises for its own goals.
Compatibility with existing tools is welcome where it comes for free. nami will probably have its
own visualisation tools.

### 6.1 Running a problem

A problem crate (§ 4(b)) is a small binary whose `main` calls `nami::run::<Problem>()`. Its
commands are:

- `run <config>`;
- `check <config>`: validates without running, for agents and for job scripts;
- `restart <checkpoint>`.

`--set key=value` overrides one value of the configuration.

### 6.2 Configuration

The configuration is a strict subset of TOML, read by nami's own parser (ADR-0001 rules out a
crate).

- There are no defaults. Every key the compiled combination needs must be present (TIGER_STYLE.md:
  numerical configuration is spelled out in full).
- Unknown keys, missing keys and wrong types are errors. The message names the file, the line and
  the fix.
- The resolved configuration is written into every output and checkpoint.

### 6.3 Diagnostics

- Per-step diagnostics go to a JSON Lines file, written by nami's own code: time, time step,
  conservation totals, and the activation counts of positivity mechanisms and floors (§ 3.2).
- Errors on stderr are one line each and start with `ERROR`.
- The buffers for both are allocated at init.

### 6.4 Output and checkpoints

- **nami's own binary format.** There is one file per output. Every rank writes its blocks at
  offsets computed in advance, with `pwrite`, which gives parallel I/O without MPI-IO. The data is
  raw little-endian `f64`, after a self-describing header.
- **Checkpoints use the same format** and keep exact bits.
- **Readers:** a small Python reader in `tools/` that needs only numpy. An XDMF sidecar that lets
  ParaView and VisIt read the raw data is optional, kept only if it stays free. nami's own
  visualisation tools come later.

### 6.5 Version metadata

Every output and checkpoint header records:

- `format_version`: the file layout;
- `results_version`: an integer in the code that every `changes-results` PR increments. Two
  outputs with the same `results_version` and configuration are expected to be bitwise equal;
- the build:
  - the nami commit, flagged if the working tree was dirty;
  - the `rustc` version;
  - a hash of `Cargo.lock`;
  - the target and its CPU flags;
  - the type names of the compiled combination;
- the resolved configuration.

A build script that uses only `std` gathers the build fields at compile time. On restart, nami
compares the checkpoint with the running build. If any of the fields that affect bits differ, the
restart is refused and the message names the difference. `--allow-different-build` overrides
this, and the output then records that the restart guarantee does not hold.

### 6.6 Guarantees users can rely on

With the same binary, configuration and toolchain, a run is bitwise reproducible:

1. across platforms: Linux x86_64 and macOS arm64 (ADR-0005);
2. across decompositions: threads, ranks and the placement of blocks (ADR-0027);
3. across restarts: a run that stops and restarts from a checkpoint gives the same bits as one
   that does not stop.

Bits are **not** guaranteed to match across `results_version` changes, which include toolchain
upgrades. Each such change is labelled `changes-results` and explained (ADR-0005).

There are no Python bindings: `pyo3` is a crate, so ADR-0001 rules it out. Python only reads
output.
