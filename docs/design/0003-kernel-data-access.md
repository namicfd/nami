# ADR-0003: Kernel crate, static allocation and data access

- Status: Accepted (amended by ADR-0013)
- Date: 2026-11-24
- Deciders: maintainer

## Context

TigerStyle requires static allocation: allocate at startup and never afterwards. Out-of-bounds access must also be impossible or asserted.

With the strict lint set (ADR-0006), a raw stencil line such as `out[i] = u[i] - c*(u[i+1] - u[i-1])` produced **7 warnings** (`indexing_slicing`, `arithmetic_side_effects`), even with length asserts placed before the loop. `missing_asserts_for_indexing` never fires on loops. The following forms produced **no** warnings:

- constant indices into fixed-size arrays;
- indexing through our own `Index<CellIndex>` impl;
- `windows(N)` combined with slice patterns.

The `windows` form was also the fastest version measured, with no bounds checks left in the generated code.

## Decision

### Crate layout and allocation

1. Numerical kernels (reconstruction, Riemann solvers, flux and update kernels, invariant reductions) live in **`nami-kernels`**, which is `#![no_std]` with **no `extern crate alloc`**. The compiler therefore rejects `Vec`, `Box` and `String` in that crate.
2. All allocation happens in `Solver::init()` (or equivalent) in the orchestration crate: fields, ghost layers, pencil scratch buffers and output buffers. After init, nothing allocates.
3. In test and `release-checked` builds, a counting `#[global_allocator]` (about 40 lines, in-house) **aborts** on any allocation once an `INIT_DONE` flag is set. It aborts rather than panics because panic formatting can itself allocate. A test asserts zero allocations across N timesteps.

### Data access in kernels

There are exactly three sanctioned ways to read data:

1. **Pencils.** Dimension-by-dimension 1D sweeps over contiguous slices that include ghost cells, using `windows(STENCIL)` or `chunks_exact` with slice patterns (`let [l, c, r] = *w`). Multi-dimensional fields are swept as pencils in each direction.
2. **Fixed-size state arrays.** Per-cell states are `[f64; NVAR]`, read through const indices or named accessors (`rho()`, `mom_x()`, `energy()`).
3. **Typed gathers.** Indirect access (face → cell connectivity, corner or transverse terms) goes through `field[CellIndex]`, which uses the single audited `Index` impl from ADR-0002.

Raw slice indexing (`u[i]`) is not allowed anywhere. There is no escape hatch. If a scheme cannot be expressed in these forms (for example CTU transverse corrections or constrained transport), it needs a new ADR that defines the exception.

`dyn Trait` objects and function pointers are banned in `nami-kernels`, so that the call graph can be fully resolved statically for the recursion check (ADR-0006).

## Enforcement

- `nami-kernels` has `#![no_std]` and no `alloc`, enforced by the compiler.
- Clippy: `indexing_slicing = "deny"` and `arithmetic_side_effects = "warn"`, one table for the whole workspace. `disallowed-methods`/`disallowed-macros` cover `Vec::push`, `Extend::extend` and `vec!` outside init code, with reasons.
- Tidy: no `dyn` and no `fn(` pointer types in `nami-kernels`.
- The counting-allocator test runs in `just check`.
- The `/add-flux-scheme` skill and a template kernel show the canonical pencil form.

## Consequences

- Kernels cannot panic on out-of-bounds access by construction, the bounds-check overhead is gone, and the whole workspace shares one lint table.
- Copying into pencils costs some memory traffic. Most structured FV codes accept this.
- Some multi-dimensional terms are clumsy to write as typed gathers. Agents need the template to get this right the first time.

## Alternatives considered

- **Raw indexing in kernel modules with asserts before the loop.** Rejected. Performance depends on agents remembering the asserts, which nothing enforces. It would also need a second lint table, because Cargo cannot override workspace lints per crate.
- **Structured access by default, with per-function `#[expect(clippy::indexing_slicing)]`.** Rejected for now. Agents would use the escape hatch whenever the iterator form gets hard.
- **Clippy `disallowed_types` only, with kernels in a std crate.** Rejected. Bans by name are easy to get around; `no_std` without `alloc` cannot be.

## Evidence

Local clippy and benchmark runs on rustc 1.100, Apple Silicon, 1M-cell 3-point stencil:

| Variant | Time | Bounds-check calls in the generated code |
|---|---|---|
| raw indexing, no asserts | 0.24 ms | 3 |
| raw indexing, asserts before the loop | 0.16 ms | 1 |
| `windows(3)` | 0.15 ms | 0 |

- The published no-allocation crate `assert_no_alloc` was last released in 2021 and needs std.
- Trixi.jl gates hot-path allocations in CI with `@test_allocations`.
