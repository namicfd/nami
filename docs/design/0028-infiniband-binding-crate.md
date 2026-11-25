# ADR-0028: A binding crate for InfiniBand

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends, once accepted: ADR-0001 (a system library besides `libm`) and ADR-0014 (`unsafe` in a
  second crate)

## Context

Production runs use several nodes connected by InfiniBand (ADR-0022). From `std` alone, nami can
reach other nodes only over TCP, whose latency is roughly ten times that of RDMA. That figure is
background knowledge; measure it in M7. The fabric is reachable only through a C system
library. ADR-0022 accepted a small binding crate in principle. This ADR gives it a shape.

## Decision

1. **A crate `fabric`** binds `libibverbs`, the lowest-level and most stable interface for
   InfiniBand and RoCE. libfabric and UCX are not used: they are larger and add layers nami does
   not need.
2. **It holds only the binding.** It contains the `extern "C"` declarations for the verbs nami
   uses, and safe wrappers around them. It contains no communication logic, which stays in
   `Transport` (ADR-0027).
3. **`unsafe` is confined to this crate**, as in `alloc-guard` (ADR-0014). Every `unsafe` block
   carries a `// SAFETY:` comment that names the invariant, and each wrapper asserts its
   preconditions before the call.
4. **The system library is linked only with the cargo feature `infiniband`,** which is off by
   default. Workstation builds, CI and every test run without it. No crate is added, so the
   dependency closure of ADR-0001 stays `{libm}`.
5. **It is built in M7.** This ADR is accepted before then, so that M7's plan can rely on it.

## Enforcement

- Tidy: the `unsafe` allow-list grows from `crates/alloc-guard` to also include `crates/fabric`.
  That is a protected change in M7.
- Tidy: every `unsafe` block in `fabric` has a `// SAFETY:` comment (a new rule, added with the
  crate).
- The ADR-0001 dependency check is unchanged: no new crate.
- CI builds without the feature. A cluster job with the feature is planned with M7.

## Consequences

- RDMA latency and bandwidth become available without MPI.
- There is a second place with `unsafe` to review, which is small and code-owned.
- The InfiniBand path cannot be tested in CI. The in-process and TCP transports carry the logic
  tests, and the cluster carries the fabric tests.

## Alternatives considered

- **TCP only.** Rejected for production: latency at many nodes.
- **libfabric or UCX.** Rejected for now. They are larger, and nami needs only point-to-point RDMA
  and a few collectives. libfabric would be the choice if a cluster with Slingshot ever matters.
- **Binding MPI after all.** Rejected by ADR-0001 and ADR-0022.

## Evidence

- `libibverbs` (rdma-core) is the user-space verbs library on Linux InfiniBand systems.
- ADR-0014 (how `unsafe` is confined today).
- The maintainer's answers in the architecture discussion of 2026-11-26.
