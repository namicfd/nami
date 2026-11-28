# ADR-0043: Data layout and kernel shape

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends, once accepted: ADR-0003 (pencils become row stencils; `[f64; NVAR]` becomes named
  state structs)
- Summary: Structure of arrays `[var][k][j][i]`, kernels on row stencils, per-face physics over
  named state structs. Measured at 1.9× Athena++ only with `f64::sqrt`, which today's rules ban,
  and `debug_assert!` in `math::sqrt` (with a copy of today's `math::sqrt`: 0.1×, below the 0.8×
  failure line, or 0.9×, below the 1.0× target, depending on inlining). Amends 0003 once accepted.

## Context

ADR-0003 allows three ways to read data in kernels: pencils, `[f64; NVAR]` states, and typed
gathers. Its evidence comes from a 1D stencil. nami is 3D from M0 (ADR-0035) and must be at
least as fast as Athena++ per core (ADR-0034). Copying y- and z-pencils out of a 3D block costs
memory traffic. On stable Rust, an associated constant cannot be an array length in generic code,
so `[f64; E::NVAR]` cannot be written in kernels that are generic over the equation system
(ADR-0037).

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
   or speed. ADR-0038 may choose either for other reasons.
6. **Constrained transport's edge terms** still need their own exception, as ADR-0003 foresaw. It
   comes with M5's plan.

## Enforcement

- Clippy `indexing_slicing = "deny"` (ADR-0003), unchanged.
- Tidy: no `dyn` and no function pointers in `kernels` (ADR-0003), unchanged.
- The template kernel in `crates/kernels/src/update.rs` is replaced in M0 by a row kernel in the
  area/volume form (ADR-0035). `TIGER_STYLE.md`'s "Data access in kernels" changes with it, in a
  protected commit of the M0 plan.
- Performance: the benchmark against Athena++ at every milestone (ADR-0034).

## Consequences

- The Riemann solver, which takes about half the time, vectorises in all three directions. The
  prototype measured 1.9× Athena++ at 64³ and 1.86× at 128³, **but only with a `math::sqrt` that
  differs from today's:** `f64::sqrt` as the backend, which ADR-0005 and `clippy.toml` ban today
  (math goes through `libm`), and `debug_assert!` instead of `assert!` on each call. With a copy
  of today's `kernels::math::sqrt`, the same layout measured 0.1× (below ADR-0034's 0.8× failure
  line) to 0.9× (below the 1.0× target), depending on inlining. The real cross-crate function
  was not measured. The layout choice does not rest on `sqrt`: structure of arrays was the
  fastest layout in every configuration except A and D, the two with libm's `arch` sqrt. There
  the hybrid was faster because it escaped the SSE/AVX transition penalty by accident, not
  because of its layout, and no layout reached the 1.0× target. Reaching the target needs a
  separate decision on `sqrt` before M2, which this ADR does not make.
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
  output across layouts, and L1 errors equal to Athena++'s to seven digits. The 1.9× is the
  prototype's configuration C (`f64::sqrt` backend, `debug_assert!` in `math::sqrt`);
  configuration A, a copy of today's `math::sqrt`, gave structure of arrays 0.10× in the main
  campaign and 0.91× to 0.93× in a rebuild that differs only in inlining.
- Athena++ v24.0 (commit `209a9258`), built with `g++-simd` (`-O3 -ffast-math -march=native`).
