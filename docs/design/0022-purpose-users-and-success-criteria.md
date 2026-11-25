# ADR-0022: Purpose, users and success criteria

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer

## Context

The guardrails (ADR-0001 to ADR-0021) say how nami is written, but not what it is for. Without
that, every architecture decision gets made by default. PR #4, a 1D advection plan, was closed for
this reason: its solver parts would have quietly decided the architecture. There are three
plausible readings: a teaching or research code, a production solver, or an experiment in
agent-built numerical code. They pull scope, performance work and interface design in different
directions.

## Decision

1. **nami is a production solver** for compressible MHD and its sub-systems. Its goals are
   safety, speed, and a demonstration of what a swarm-first approach can build.
2. **Users** are researchers, and agents working for them, who study fluid phenomena with it.
   The results are for the maintainer and a few former colleagues. There is no public release or
   interface stability promise until a later ADR makes one.
3. **End state:** 3D classical MHD on static and adaptive grids. Stretch goal: unstructured and
   moving (Voronoi) grids. The feature set draws on existing codes, Athena++ and AthenaK first.
4. **nami builds its own infrastructure.** ADR-0001 stands: no MPI and no HDF5. nami designs its
   own communication layer and output format.
5. **Failure** is either of:
   - humans can no longer read the code;
   - performance is significantly worse than Athena++/AthenaK.
6. **Success criteria:**
   - **Performance:** zone-cycles per second, per core and per node, on the same machine, problem
     and algorithm as Athena++ and AthenaK's CPU backend, each built as its documentation
     recommends. Target ≥ 1.0×; below 0.8× is failure. Scaling targets come in a later ADR.
   - **Correctness:** every capability claim is backed by references at every tier of ADR-0007,
     including Athena++'s regression problems and published thresholds where they exist.
   - **Readability:** at each milestone, a human who neither wrote nor reviewed the code reads a
     sampled module with its plan and can explain what it computes and why. Findings become
     guardrails (ADR-0020).
   - **Swarm record:** each merged PR records its cost, the maintainer's minutes, whether a human
     changed the code, and every defect that escaped and was found later.

## Enforcement

- **Performance:** none yet. A benchmark job that runs pinned builds of Athena++ and AthenaK
  next to nami on the same machine is planned. It arrives with the first plan that has a solver
  worth timing. Gungraun instruction counts (TIGER_STYLE.md, Performance) guard against
  regressions between comparisons.
- **Correctness:** ADR-0007 and its planned `reference-suite`.
- **Readability:** human review at each milestone, plus TigerStyle's mechanical limits (function
  length, line length, naming).
- **Swarm record:** none yet. The mechanism is decided with the first implementation plan.

## Consequences

- Performance is a design input from the first kernel. Every plan carries its back-of-the-envelope
  estimate of bytes and flops per cell (TIGER_STYLE.md, Performance).
- Later ADRs have to resolve the tensions listed in `docs/ARCHITECTURE.md` § 1:
  - GPUs, which AthenaK uses and nami cannot yet;
  - fast interconnects, which only C libraries reach;
  - the cost of determinism;
  - fixed-capacity pools for AMR and moving meshes;
  - typed gathers for unstructured grids.
- Since agents are users, the interface must suit them: explicit configuration, terse errors,
  machine-readable diagnostics.

## Alternatives considered

- **A teaching or research code.** Rejected. Clarity stays a requirement (the readability
  criterion), but the maintainer wants a code used for real studies.
- **An experiment first, with the solver only as the vehicle.** Rejected. Without a performance
  bar from outside the project, the result proves less.
- **Bind MPI and HDF5.** Rejected, consistent with ADR-0001. Writing these parts in-house is a
  deliberate choice to tune them to nami's needs.

## Evidence

- Athena++: Stone et al. 2020, ApJS 249, 4. AthenaK: Stone et al. 2024, arXiv:2409.16053. Both
  set the feature set and the performance bar. Their measured zone-cycle rates on our hardware
  come from the planned benchmark job, not from the papers.
- The RDMA-versus-TCP latency ratio in `docs/ARCHITECTURE.md` § 1 is background knowledge. Verify
  it when § 5 is decided.
- The maintainer's answers in the architecture discussion of 2026-11-26.
