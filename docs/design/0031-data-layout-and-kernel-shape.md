# ADR-0031: Data layout and kernel shape

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends, once accepted: ADR-0003 (pencils become row stencils; `[f64; NVAR]` becomes named
  state structs)

## Context

ADR-0003 allows three ways to read data in kernels: pencils, `[f64; NVAR]` states, and typed
gathers. Its evidence comes from a 1D stencil. nami is 3D from M0 (ADR-0023) and must be at
least as fast as Athena++ per core (ADR-0022). Copying y- and z-pencils out of a 3D block costs
memory traffic. On stable Rust, an associated constant cannot be an array length in generic code,
so `[f64; E::NVAR]` cannot be written in kernels that are generic over the equation system
(ADR-0025).

The layout prototype (`docs/research/layout-prototype.md`, PR #47) measured three layouts on 3D
Euler with PLM, HLLC and SSP-RK2, against Athena++ v24.0 on the same core.

## Decision

1. **Structure of arrays.** A block stores one array per variable, laid out `[var][k][j][i]`
   with i fastest.
2. **Kernels work on row stencils.** A sweep in any direction loops over i. The stencil across
   the sweep direction is a set of rows (j−1, j, j+1, or k−1, k, k+1, and wider for wider
   reconstructions), each contiguous in i. Nothing is transposed or copied into pencils.
3. **Per-face physics over named-field state structs,** such as
   `EulerPrimitive { density, velocity: [f64; 3], pressure }`. Riemann solvers and equations of
   state are written for one face and called in a loop over i that loads each field from its own
   row. They replace `[f64; NVAR]` with constant indices.
4. **The sanctioned ways to read data are therefore** row stencils, named state structs and typed
   gathers (`field[CellIndex]`). Raw slice indexing stays banned. The prototype was written
   entirely with `chunks_exact`, `array_windows`, zipped rows and typed row accessors, and lost
   nothing for it.
5. **Block sizes need not be compile-time constants.** The prototype found no difference in code
   or speed. ADR-0026 may choose either for other reasons.
6. **Constrained transport's edge terms** still need their own exception, as ADR-0003 foresaw. It
   comes with M5's plan.

## Enforcement

- Clippy `indexing_slicing = "deny"` (ADR-0003), unchanged.
- Tidy: no `dyn` and no function pointers in `kernels` (ADR-0003), unchanged.
- The template kernel in `crates/kernels/src/update.rs` is replaced in M0 by a row kernel in the
  area/volume form (ADR-0023). `TIGER_STYLE.md`'s "Data access in kernels" changes with it, in a
  protected commit of the M0 plan.
- Performance: the benchmark against Athena++ at every milestone (ADR-0022).

## Consequences

- The Riemann solver, which takes about half the time, vectorises in all three directions. The
  prototype measured 1.9× Athena++ at 64³ and 1.86× at 128³.
- Kernels are generic over named state types, so each equation system writes its fields once,
  and the rows are tied to them by one accessor per system.
- The speed depends on LLVM vectorising the face loop. An early exit in that loop, such as an
  `assert!` inside a function it calls, stops vectorisation. Per-face checks must be
  `debug_assert!` (ADR-0004), backed by the per-block backstop.

## Alternatives considered

- **Array of structures, `[f64; NVAR]` per cell.** Rejected. It reached 1.1× in the prototype,
  and its transverse Riemann loop does not vectorise.
- **The hybrid (structure of arrays within groups of 4 or 8 cells).** Rejected. It reached
  1.1–1.2×, its transverse sweeps do not vectorise, and it would need explicit SIMD types, which
  are unstable.
- **Pencils copied out of the block, as ADR-0003 describes.** Rejected for multi-D. Row stencils
  read the block in place.

## Evidence

- `docs/research/layout-prototype.md` (PR #47) and the prototype code on branch
  `research/layout-prototype`: 64³ and 128³, five interleaved repetitions, bitwise-identical
  output across layouts, and L1 errors equal to Athena++'s to seven digits.
- Athena++ v24.0 (commit `209a9258`), built with `g++-simd` (`-O3 -ffast-math -march=native`).
