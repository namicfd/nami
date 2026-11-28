# ADR-0039: Parallelism and communication

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer

## Context

ADR-0034 targets CPUs: a workstation for development, and several InfiniBand nodes under Slurm
in production. Performance must be at least that of Athena++ per core and per node. ADR-0001
rules out MPI, rayon and every other parallelism crate, and ADR-0001 deferred the choice of
parallelism to a later ADR, which is this one. ADR-0005 makes results bitwise deterministic on one
process. ADR-0038 fixes blocks, a Morton-ordered tree and bulk-synchronous phases.

## Decision

1. **Within a core, auto-vectorisation only.** No `std::arch` intrinsics (they are `unsafe`) and
   no `std::simd` (nightly). A loop that does not vectorise is restructured.
2. **Within a node, one process per NUMA domain, with threads** created at init by
   `std::thread::scope`. Blocks are assigned statically, in contiguous Morton ranges, without work
   stealing. Phases are synchronised with `std::sync::Barrier`. Each thread first-touches its own
   blocks at init.
3. **Between nodes, a `Transport` trait** with in-process, TCP (`std::net`) and InfiniBand
   (ADR-0040) implementations. Channels between neighbouring ranks are persistent, with buffers
   preallocated at init and rebuilt only on rebalancing. The collectives are a barrier, a minimum,
   diagnostic sums and an all-gather of tree changes.
4. **Launching through Slurm.** `srun` starts the processes, and ranks come from `SLURM_PROCID`
   and `SLURM_NTASKS`. Rank 0 runs a TCP rendezvous for exchanging addresses. nami has no launcher.
5. **Results are bitwise identical for any decomposition:** any number of threads or ranks, and
   any placement of blocks. The time step is a minimum; diagnostic sums across blocks use an
   exact, order-independent accumulator; refinement depends only on block data.
6. **A failed link is an operating error.** The run stops and checkpoints if it can.
7. **Targets:** at least 1.0× Athena++ per core and per node (ADR-0034), and at least 90%
   weak-scaling efficiency from 1 to 64 nodes as the first scaling target. Problem sizes and
   strong scaling are set with M7.
8. **Widths:** Morton keys and global cell numbers are `u64`. Each rank still has at most 2³² − 1
   cells (ADR-0002).

## Enforcement

- `unsafe_code = "deny"` and tidy keep intrinsics out (ADR-0014). `std::simd` needs a nightly
  feature, which the pinned stable toolchain rejects.
- A decomposition test is planned with M4: the same problem with 1, 2 and an odd number of
  threads, and from M7 the same with in-process ranks, must give identical output hashes. It
  blocks merging, like the same-seed rerun of ADR-0005.
- `alloc-guard`: the communication layer allocates only at init and at rebalancing, which runs in
  init-like code (ADR-0038). The planned `alloc-after-init` test covers the steps in between.
- Scaling targets: none yet; scaling runs start with M7.

## Consequences

- Every scaling run is also a determinism test. A bug in the ghost exchange or in the partition
  shows up as a changed hash.
- A static schedule cannot absorb load imbalance within a process. The Morton partition balances
  by block count, which equal-sized blocks make adequate.
- nami writes and maintains its own communication layer, rendezvous and collectives. That is
  the design freedom ADR-0034 asked for, and also a body of code with no outside users to find
  its bugs. The in-process transport exists so that it can be tested deterministically.
- The exact accumulator is slower than `sum_compensated`. It is only used for diagnostics, never
  in the hot path.

## Alternatives considered

- **One process per core, as Athena++ usually runs.** Rejected as the default. Ghost cells within
  a node would be copied through messages. The design still allows it: it is a configuration
  with one thread per process.
- **Work stealing.** Rejected. It makes the schedule depend on timing, and equal-sized blocks do
  not need it.
- **Decomposition-dependent sums with tolerances, as most codes do.** Rejected. ADR-0005's
  argument applies: a tolerance is a knob, and bits are a signal.
- **Our own launcher.** Rejected. Slurm already starts processes on every cluster in sight.

## Evidence

- `std::thread::scope` and `std::sync::Barrier` are stable. `std::simd` (portable SIMD) is still
  nightly-only on Rust 1.100.
- Exact, order-independent summation: Kulisch accumulators; Demmel & Nguyen 2015 (ReproBLAS);
  Neal 2015, "Fast exact summation using small and large superaccumulators", arXiv:1505.05571.
- Slurm's `srun` sets `SLURM_PROCID` and `SLURM_NTASKS` for every task.
- The maintainer's answers in the architecture discussion of 2026-11-26.
