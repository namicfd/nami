# nami architecture

> Status: **being written, one topic at a time,** in a discussion between the maintainer and an
> agent. Each section says whether it is agreed, proposed or open. The decisions themselves are
> ADRs in `docs/design/`, status Proposed until the maintainer accepts them. Where this file and an
> ADR disagree, the ADR wins.

| § | Topic | State | ADR |
|---|---|---|---|
| 1 | Purpose, users and success criteria | Proposed | [0022](design/0022-purpose-users-and-success-criteria.md) |
| 2 | Scope and roadmap | Open | |
| 3 | Numerics | Open | |
| 4 | Core architecture | Open | |
| 5 | Performance and parallelism | Open | |
| 6 | Interface, output and determinism guarantees | Open | |
| 7 | Verification strategy | Open | |

## 1. Purpose, users and success criteria

**nami is a production solver** for compressible magnetohydrodynamics and its sub-systems. It has
three goals:

- **Safety:** results you can trust. Rust's type system, TigerStyle assertions, bitwise
  determinism (ADR-0005) and references anchored outside the agent swarm (ADR-0007) make it so.
- **Speed:** at least as fast as Athena++ and AthenaK on the same problem and hardware.
- **A swarm-first demonstration:** agents write the code, and humans own the references, the
  thresholds and the ADRs. The project shows how far that approach goes.

**Users** are researchers, and agents working for them, who apply the code to study fluid
phenomena. The results are for the maintainer and a few former colleagues. There is no
public release and no promise of a stable interface yet.

**End state:** 3D classical MHD on static and adaptively refined grids. A stretch goal is
unstructured and moving grids, such as a moving Voronoi mesh. The feature set draws heavily on
existing codes, Athena++ and AthenaK first.

**nami builds its own infrastructure.** The only runtime dependency is `libm` (ADR-0001), so there
is no MPI and no HDF5. That is deliberate: nami designs its own communication layer and output
format and tunes them to its needs.

### Failure

The project has failed if:

- **humans can no longer read the code;** or
- **its performance is significantly worse than Athena++/AthenaK.** The aim is to be faster.

### Success criteria

1. **Performance.** The measure is zone-cycles per second: cell updates per second, per core and
   per node. It is measured on the same machine and the same problem as Athena++ and AthenaK's
   CPU backend, with the same algorithm (for example PLM, HLLD, VL2 and constrained transport),
   each code built as its documentation recommends. The target is at least 1.0× per core and per
   node, and below 0.8× counts as failure. The scaling targets come with § 5.
2. **Correctness.** Every capability claim, such as "3D MHD on AMR works", is backed by
   references at every tier of ADR-0007. That includes the problems and published thresholds of
   Athena++'s regression suite, where they exist.
3. **Readability.** At each milestone, a human who neither wrote nor reviewed the code reads a
   sampled module together with its plan. They must be able to explain what it computes and why.
   Each finding becomes a guardrail (ADR-0020).
4. **The swarm demonstration is recorded.** For each merged PR we record the cost, the
   maintainer's minutes, whether a human changed the code, and every defect that escaped and was
   found later.

### Tensions that later topics must resolve

- **GPUs (§ 5).** AthenaK's headline performance is on GPUs, through Kokkos. nami has no GPU path
  under ADR-0001 and ADR-0014, and CPU code will not beat a GPU node. Until § 5 decides, the
  comparison is CPU against CPU.
- **Fast interconnects (§ 5).** InfiniBand and Slingshot are reached through C libraries
  (libibverbs, libfabric, UCX). With `std` alone, nami reaches other nodes only over TCP, whose
  latency is about ten times higher than RDMA's. That is fine on one node and a few nodes, and a
  risk at large scale. One option is a small crate that binds a system library, with its `unsafe`
  confined like `alloc-guard`'s (ADR-0014).
- **Determinism costs speed.** ADR-0005 estimates 5–15% per step, and nami pays it against the
  performance target. In return, nami can offer what Athena++ does not: results that are
  bitwise the same for any decomposition and thread count, if § 5 and § 6 choose to.
- **No allocation after init (ADR-0003) against AMR and moving meshes (§ 4).** Blocks and cells
  must come from fixed-capacity pools, sized at init from a configured memory budget.
- **Pencils against unstructured grids (§ 4).** ADR-0003's pencils cover structured blocks. A
  Voronoi mesh needs typed gathers as a first-class path, not as an exception.
- **Agents are users (§ 6).** Configuration, errors and output must be usable by an agent:
  explicit configuration, terse greppable errors, machine-readable diagnostics.
